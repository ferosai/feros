//! Tracer façade — single entry point for the Reactor.
//!
//! Owns the [`EventBus`] and [`TurnTracker`]. The Reactor
//! replaces its separate fields with a single `Tracer` and calls
//! methods like `tracer.trace("BargeIn")` and `tracer.emit(Event::*)`.
//!
//! Two methods push to the bus:
//!
//! - **`trace(label)`** — emits `Event::Trace` on the bus with a monotonic
//!   sequence number and microsecond timestamp. Use for infrequent, meaningful
//!   state transitions that external consumers (OTel, debug UI) may care about.
//! - **`emit(event)`** — emits any structured [`Event`] variant directly.

use std::collections::HashSet;
use std::time::Instant;

use tokio::sync::broadcast;

use crate::bus::{EventBus, FilteredReceiver};
use crate::event::{Event, EventCategory};
use crate::turn_tracker::{TurnMetrics, TurnTracker};

/// Unified observability façade for a voice session.
///
/// The Reactor owns a single `Tracer`. All event emission, trace
/// recording, and turn metric tracking flows through this struct.
pub struct Tracer {
    bus: EventBus,
    turn_tracker: TurnTracker,
    /// Monotonically increasing sequence counter for `Event::Trace`.
    trace_seq: u64,
    /// Session start time — used to compute `elapsed_us` in `Event::Trace`.
    session_start: Instant,
    /// Accumulated text fed to TTS this turn (for TtsComplete event).
    tts_text: String,
    /// Monotonically increasing turn counter (1-based).
    turn_count: u64,
    /// True while a turn span is open (gates TurnEnded emission).
    turn_open: bool,
}

impl Tracer {
    /// Create a new tracer with a fresh EventBus and TurnTracker.
    pub fn new() -> Self {
        Self {
            bus: EventBus::new(),
            turn_tracker: TurnTracker::new(),
            trace_seq: 0,
            session_start: Instant::now(),
            tts_text: String::new(),
            turn_count: 0,
            turn_open: false,
        }
    }

    // ── Raw trace ───────────────────────────────────────────────────────

    /// Record a raw trace event with microsecond timing.
    ///
    /// Emits `Event::Trace` on the bus for external consumers (OTel, debug UI).
    ///
    /// Use for infrequent, meaningful state transitions (e.g. `"BargeIn"`,
    /// `"HangUp"`).
    pub fn trace(&mut self, label: &'static str) {
        self.trace_seq += 1;
        let elapsed_us = self.session_start.elapsed().as_micros() as u64;
        self.bus.emit(Event::Trace {
            seq: self.trace_seq,
            elapsed_us,
            label: label.to_string(),
        });
    }

    // ── Structured event emission ───────────────────────────────

    /// Emit a structured event to all subscribers.
    ///
    /// Use this for all high-level events (state changes, transcripts,
    /// tool activity, audio, agent events). Never blocks.
    pub fn emit(&self, event: Event) {
        self.bus.emit(event);
    }

    // ── TurnTracker delegates ───────────────────────────────────

    /// EOU decision committed — the transcript is about to start an LLM turn.
    ///
    /// Anchors `pipeline.speech_ended` as the start of the user→agent latency
    /// clock. Called from the turn coordinator when a committed transcript
    /// arrives (not at raw VAD SpeechEnded time). First-write-wins: safe to
    /// call multiple times (e.g. EotFallback fires + subsequent transcript).
    pub fn mark_speech_ended(&mut self) {
        self.turn_tracker.mark_speech_ended();
    }

    /// STT streaming started (user speaking).
    pub fn mark_stt_audio_start(&mut self) {
        self.turn_tracker.mark_stt_audio_start();
    }

    /// STT finalize signal sent to provider.
    pub fn mark_stt_finalize_sent(&mut self) {
        self.turn_tracker.mark_stt_finalize_sent();
    }

    /// First partial text received from STT.
    pub fn mark_stt_first_text(&mut self) {
        self.turn_tracker.mark_stt_first_text();
    }

    /// STT returned the final transcript.
    pub fn mark_transcript(&mut self) {
        self.turn_tracker.mark_transcript();
    }

    /// First LLM token received.
    pub fn mark_llm_first_token(&mut self) {
        self.turn_tracker.mark_llm_first_token();
    }

    /// TTS synthesis session started (first call per turn).
    pub fn mark_tts_start(&mut self) {
        self.turn_tracker.mark_tts_start();
    }

    /// First block of text fed to TTS synthesis.
    pub fn mark_tts_text_fed(&mut self) {
        self.turn_tracker.mark_tts_text_fed();
    }

    /// First TTS audio chunk sent to client.
    pub fn mark_tts_first_audio(&mut self) {
        self.turn_tracker.mark_tts_first_audio();
    }

    /// TTS synthesis finished (last audio chunk sent).
    pub fn mark_tts_finished(&mut self) {
        self.turn_tracker.mark_tts_finished();
    }

    /// Record the VAD speech-ended timestamp (before SmartTurn decision).
    /// Backdates by the session-level `vad_silence_ms` set via `set_vad_silence_ms`.
    pub fn mark_vad_speech_ended(&mut self) {
        self.turn_tracker.mark_vad_speech_ended();
    }

    /// Set the VAD silence window (ms) — called once at session startup.
    pub fn set_vad_silence_ms(&mut self, ms: f64) {
        self.turn_tracker.set_vad_silence_ms(ms);
    }

    /// Get the VAD silence window (ms) — used for STT timeout calculation.
    pub fn vad_silence_ms(&self) -> f64 {
        self.turn_tracker.vad_silence_ms()
    }

    /// Set the duration of the user's input speech audio (in ms).
    pub fn set_input_audio_duration(&mut self, ms: f64) {
        self.turn_tracker.set_input_audio_duration(ms);
    }

    /// Set the duration of the generated TTS audio (in ms).
    pub fn set_output_audio_duration(&mut self, ms: f64) {
        self.turn_tracker.set_output_audio_duration(ms);
    }

    // ── Turn lifecycle ────────────────────────────────────────────

    /// Open a new turn span and emit the STT result.
    ///
    /// Increments the turn counter, emits `TurnStarted` + `SttComplete`.
    /// Each transcript maps to exactly one turn — a new transcript
    /// during an active turn is a barge-in (handled by `cancel_turn`).
    pub fn start_turn(
        &mut self,
        stt_provider: &str,
        stt_model: &str,
        transcript: &str,
        language: &str,
        vad_enabled: bool,
    ) {
        // Guard: cancel any open turn before starting a new one
        // (e.g. barge-in transcript arriving before cancel_turn).
        if self.turn_open {
            tracing::warn!("[tracer] start_turn called with an open turn — cancelling previous");
            self.cancel_turn();
        }

        self.turn_count += 1;
        self.turn_open = true;
        self.bus.emit(Event::TurnStarted {
            turn_number: self.turn_count,
        });

        // Freeze the current VAD/STT anchors for this turn and clear input
        // state so the tracker is ready for the next potential barge-in.
        self.turn_tracker.commit_input_anchors();
        let (duration_ms, ttfb_ms) = self.turn_tracker.stt_metrics();

        self.bus.emit(Event::SttComplete {
            provider: stt_provider.to_string(),
            model: stt_model.to_string(),
            transcript: transcript.to_string(),
            is_final: true,
            language: Some(language.to_string()),
            duration_ms,
            ttfb_ms,
            vad_enabled,
        });
    }

    /// Open a programmatic/synthetic turn (e.g. tool results, transfer failures)
    /// without consuming user VAD/STT anchors or emitting `SttComplete`.
    ///
    /// The teardown path is identical to a normal turn: callers **must** still
    /// call [`finish_turn`](Self::finish_turn) or [`cancel_turn`](Self::cancel_turn)
    /// to close the span and emit `TurnEnded`. Synthetic turns are not self-closing.
    pub fn start_synthetic_turn(&mut self) {
        if self.turn_open {
            tracing::warn!("[tracer] start_synthetic_turn called with an open turn — cancelling previous");
            self.cancel_turn();
        }

        self.turn_count += 1;
        self.turn_open = true;
        self.bus.emit(Event::TurnStarted {
            turn_number: self.turn_count,
        });

        // Set the interaction anchor so end-to-end metrics have a starting point
        self.mark_speech_ended();
    }

    // ── TTS text accumulation (for TtsComplete event) ──

    /// Append text fed to TTS this turn.
    pub fn append_tts_text(&mut self, text: &str) {
        self.tts_text.push_str(text);
    }

    /// Turn complete — emit TtsComplete + TurnMetrics + TurnEnded.
    ///
    /// Handles the full end-of-turn ceremony:
    /// 1. Emit `TtsComplete` (if any text was accumulated)
    /// 2. Emit `TurnMetrics`
    /// 3. Emit `TurnEnded`
    ///
    /// No-op if no turn is open. Returns metrics if available.
    pub fn finish_turn(
        &mut self,
        was_interrupted: bool,
        tts_provider: &str,
        tts_model: &str,
        voice_id: &str,
    ) -> Option<TurnMetrics> {
        if !self.turn_open {
            tracing::warn!(
                "[tracer] finish_turn called but no turn is open — did you forget start_turn()?"
            );
            return None;
        }
        self.turn_open = false;

        // Emit TtsComplete before TurnEnded so Langfuse sees it as a child span.
        // Gate on whether TTS was actually invoked (tts_text_fed set), not on
        // accumulated text — handles edge cases where text is empty-string.
        if self.turn_tracker.has_tts_text_fed() {
            let (duration_ms, ttfb_ms, text_aggregation_ms) = self.turn_tracker.tts_metrics();
            let text = std::mem::take(&mut self.tts_text);
            let character_count = text.chars().count();
            self.bus.emit(Event::TtsComplete {
                provider: tts_provider.to_string(),
                model: tts_model.to_string(),
                text,
                voice_id: voice_id.to_string(),
                character_count,
                duration_ms,
                ttfb_ms,
                text_aggregation_ms,
            });
        } else {
            self.tts_text.clear();
        }

        let metrics = self.turn_tracker.finish(self.turn_count);

        if let Some(ref m) = metrics {
            self.bus.emit(Event::TurnMetrics(m.clone()));
        }

        // Always emit TurnEnded to close the Langfuse turn span.
        self.bus.emit(Event::TurnEnded {
            turn_number: self.turn_count,
            was_interrupted,
            turn_duration_ms: metrics.as_ref().map(|m| m.total_ms),
            user_agent_latency_ms: metrics.as_ref().and_then(|m| m.user_agent_latency_ms),
            vad_silence_ms: metrics.as_ref().map(|m| m.vad_silence_ms),
        });

        metrics
    }

    /// Cancel the current turn (barge-in / abort).
    ///
    /// Emits `TurnEnded { was_interrupted: true }` only if a turn is open.
    /// Resets the tracker unconditionally.
    pub fn cancel_turn(&mut self) {
        self.turn_tracker.reset_pipeline();
        self.tts_text.clear();

        if self.turn_open {
            self.turn_open = false;
            self.bus.emit(Event::TurnEnded {
                turn_number: self.turn_count,
                was_interrupted: true,
                turn_duration_ms: None,
                user_agent_latency_ms: None,
                vad_silence_ms: None,
            });
        }
    }

    // ── Subscriptions (for external consumers) ──────────────────

    /// Subscribe to ALL events (unfiltered).
    ///
    /// Returns a broadcast receiver. Multiple consumers can subscribe.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    /// Subscribe with a category filter.
    ///
    /// Only events matching the provided categories are yielded.
    pub fn subscribe_filtered(&self, categories: HashSet<EventCategory>) -> FilteredReceiver {
        self.bus.subscribe_filtered(categories)
    }

    /// Get the underlying broadcast sender.
    ///
    /// This allows external code to subscribe independently (e.g. the WS
    /// handler can subscribe to the same event bus as the WebRTC session).
    pub fn subscribe_sender(&self) -> broadcast::Sender<Event> {
        self.bus.sender()
    }

}

impl Default for Tracer {
    fn default() -> Self {
        Self::new()
    }
}
