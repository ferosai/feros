//! Audio and VAD event handlers.
//!
//! Handles every incoming audio frame and the two VAD events it can produce.
//! Barge-in logic (immediate and word-count modes) also lives here.

use bytes::Bytes;
use tracing::info;
use voice_trace::Event;

use crate::types::VadEvent;

use super::turn_phase::TurnEvent;
use super::{Reactor, SessionPhase};

impl Reactor {
    pub(super) async fn on_audio(&mut self, raw: Bytes) {
        // 1. Resample from client rate (e.g. 48kHz) to 16kHz internal rate
        let resampled = self.resampler.process(&raw);

        // 2. Split into 512-sample (1024-byte) frames via ring buffer.
        //    process_frames() yields &[u8] slices directly from internal storage —
        //    zero Vec<u8> allocation per frame (eliminates ~50 allocs/sec at 50fps).
        //
        //    SAFETY NOTE: The closure is sync and non-recursive; the reactor's
        //    on_vad_event is async but is called *after* process_frames completes
        //    (we collect the VAD result inside the closure, not await inside it).

        // True while TTS audio is actively being streamed to the client.
        // bot_audio_sent: set on first TTS chunk, cleared on barge-in/cancel.
        // tts.is_active(): true between start_ws/http() and mark_finished()/cancel().
        // Combined this is narrower than is_pipeline_active(), which also covers LLM.
        let is_playing = self.bot_audio_sent && self.tts.is_active();
        let mut vad_event: Option<crate::types::VadEvent> = None;

        // Threshold is a packet-level decision: is_playing doesn't change within a
        // process_frames batch, so set it once here rather than once per frame.
        //
        // When the denoiser is disabled, audio reaches VAD unfiltered (raw mic),
        // so we use VAD_THRESHOLD_PLAYBACK_RAW (0.90) instead of the denoised
        // playback threshold (0.85) to maintain equivalent echo rejection.
        //
        // Known behaviour: if is_playing flips true while the user is already
        // mid-utterance, the threshold elevation can cause a premature SpeechEnd
        // (~192 ms / 6 silence frames). This is acceptable because the bot does not
        // normally start TTS while the user is speaking (barge-in clears TTS first);
        // re-engagement prompts are the only realistic scenario.
        self.vad.set_threshold(if is_playing {
            if self.denoiser.is_enabled() {
                crate::audio_ml::vad::VAD_THRESHOLD_PLAYBACK
            } else {
                crate::audio_ml::vad::VAD_THRESHOLD_PLAYBACK_RAW
            }
        } else {
            crate::audio_ml::vad::VAD_THRESHOLD_IDLE
        });

        self.ring_buffer.process_frames(&resampled, |frame| {
            // Denoise (inline ONNX, or passthrough if disabled).
            // denoiser.process() allocates for the model output; the frame
            // slice itself is free (direct view into ring buffer storage).
            let clean = self.denoiser.process(frame);

            // Emit denoised user audio for session recording (zero-cost when off)
            if self.config.recording.enabled {
                self.tracer.emit(Event::UserAudio {
                    pcm: Bytes::copy_from_slice(&clean),
                    sample_rate: crate::utils::SAMPLE_RATE,
                });
            }

            // Feed to STT (always-on streaming)
            self.stt.feed(&clean);

            // VAD (inline ONNX) — may emit SpeechStarted or SpeechEnded.
            // We take only the *last* VAD event from this batch; in practice
            // a single WebRTC packet contains at most one transition.
            if let Some(event) = self.vad.process(&clean) {
                vad_event = Some(event);
            }
        });

        if let Some(event) = vad_event {
            self.on_vad_event(event).await;
        }
    }

    pub(super) async fn on_vad_event(&mut self, event: VadEvent) {
        match event {
            VadEvent::SpeechStarted => {
                self.tracer.trace("SpeechStarted");
                info!("[reactor] SpeechStarted");

                // ── Auxiliary effects (not part of state machine) ──
                self.session_phase = SessionPhase::Active;
                self.tracer.mark_stt_audio_start();

                // Cancel re-engagement (UserIdle or TurnCompletion) — user is active.
                self.reengagement.cancel(&mut self.timers);

                // ── Dispatch to state machine ──
                let event = TurnEvent::SpeechStarted {
                    pipeline_was_active: self.is_pipeline_active(),
                    bot_audio_sent: self.bot_audio_sent,
                };
                self.dispatch_turn_event(event).await;
            }

            VadEvent::SpeechEnded(audio) => {
                self.tracer.trace("SpeechEnded");

                // ── Auxiliary metrics (not part of state machine) ──
                self.tracer.mark_vad_speech_ended();
                let input_duration_ms = audio.len() as f64
                    / (crate::utils::SAMPLE_RATE as f64 * crate::utils::SAMPLE_WIDTH as f64)
                    * 1000.0;
                self.tracer.set_input_audio_duration(input_duration_ms);

                // ── SmartTurn prediction ──
                let smart_complete = if let Some(st) = &mut self.smart_turn {
                    st.predict(&audio)
                } else {
                    true // no SmartTurn model — always complete
                };

                // ── Dispatch to state machine ──
                // Side-effect protection is handled inside dispatch_turn_event:
                // it intercepts SpeechEnded when side_effect_in_flight > 0,
                // finalizes STT, and resets turn_phase to re-open the TTS gate
                // — without letting the state machine's barge-in actions fire.
                let event = TurnEvent::SpeechEnded { smart_complete };
                self.dispatch_turn_event(event).await;
            }
        }
    }
}
