//! Event loop for Gemini Live native audio sessions.
//!
//! Contains [`run_native_multimodal`], the single entry point for the native
//! multimodal path that bypasses the standard STT/LLM/TTS Reactor entirely.

use agent_kit::agent_backends::native::{NativeAgentEvent, NativeMultimodalBackend};
use agent_kit::providers::gemini_live::{GeminiLiveProvider, OUTPUT_SAMPLE_RATE};
use agent_kit::swarm::AgentGraphDef;
use agent_kit::AgentBackend as _;
use agent_kit::AgentBackendConfig;
use bytes::Bytes;
use soxr::SoxrStreamResampler;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info, warn};
use voice_trace::event::LlmCompletionData;
use voice_trace::{Event, Tracer};
use voice_transport::TransportHandle;

use crate::audio_ml::vad::{VadConfig, VAD_THRESHOLD_IDLE, VAD_THRESHOLD_PLAYBACK_RAW};
use crate::reactor::proc::vad::VadStage;
use crate::reactor::AgentAudioCursor;
use crate::session::NativeMultimodalConfig;
use crate::types::VadEvent;
use crate::utils::{AudioRingBuffer, PlaybackTracker, SAMPLE_RATE};

/// WebRTC Opus clock rate.
const WEBRTC_RATE: u32 = 48_000;

/// Self-contained event loop for Gemini Live native audio sessions.
///
/// # Audio path
/// ```text
/// WebRTC mic → 48kHz PCM → resample 16kHz → GeminiLiveProvider.push_audio()
///                                                     │
///                                         Gemini Live WS (bidirectional)
///                                                     │
/// backend.recv() → NativeAgentEvent::BotAudio (24kHz) → resample 48kHz → tracer.emit(AgentAudio)
///                                                                                  │
///                                                              WebRTC forwarder ←──┘
///                                                              Recording sink  ←──┘
/// ```
///
/// Audio is delivered via the shared EventBus, identical to the standard Reactor path.
/// This ensures recording, WebRTC delivery, and future transports all work without special-casing.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_native_multimodal(
    nm_config: NativeMultimodalConfig,
    agent_graph: Option<AgentGraphDef>,
    system_prompt: String,
    voice_id: String,
    backend_config: AgentBackendConfig,
    mut mic_rx: UnboundedReceiver<Bytes>,
    transport: TransportHandle,
    mut tracer: Tracer,
    input_sample_rate: u32,
    models_dir: String,
    recording_enabled: bool,
    language: String,
    greeting: Option<String>,
) {
    tracer.emit(Event::SessionReady);

    // ── Build provider and backend ─────────────────────────────────
    let api_key = nm_config.api_key.clone();

    if api_key.trim().is_empty() {
        error!("[native] No Gemini API key found in the Native Multimodal configuration block. Terminating session.");
        tracer.emit(Event::Error {
            source: "native_multimodal".into(),
            message: "No Gemini API key found in Native Multimodal configuration.".into(),
        });
        tracer.emit(Event::SessionEnded);
        return;
    }

    // ── AgentAudio cursor ─────────────────────────────────────────────
    // Created here — BEFORE the WebSocket connect — so that
    // elapsed_samples() includes the full connection + setup latency.
    // This aligns the cursor's clock origin with the recording subscriber's
    // own session_start (which is set at subscriber spawn, also before setup),
    // preventing bot audio from being placed too early in the recording.
    let mut tts_cursor = AgentAudioCursor::new(WEBRTC_RATE);
    let mut playback = PlaybackTracker::new(WEBRTC_RATE);

    let provider = Box::new(GeminiLiveProvider::new(api_key, nm_config.model.clone()));
    let mut backend = NativeMultimodalBackend::new(
        provider,
        agent_graph.as_ref(),
        backend_config,
        voice_id.clone(),
    );
    let mut final_system_prompt = system_prompt;
    if let Some(mut greet) = greeting {
        greet = greet.trim().to_string();
        if !greet.is_empty() {
            final_system_prompt = format!(
                "{final_system_prompt}\n\nYour first message must be EXACTLY this greeting: \"{greet}\""
            );
        }
    }
    backend.set_system_prompt(final_system_prompt);

    // Connect to Gemini Live WebSocket.
    if let Err(e) = backend.connect().await {
        error!("[native] Failed to connect to Gemini Live: {}", e);
        tracer.emit(Event::Error {
            source: "native_multimodal".into(),
            message: format!("Gemini Live connect failed: {e}"),
        });
        tracer.emit(Event::SessionEnded);
        return;
    }
    info!("[native] Gemini Live connected");

    // ── Resamplers ─────────────────────────────────────────────────
    // Input: client rate (e.g. 48kHz) → 16kHz (Gemini input requirement).
    let mut in_resampler = SoxrStreamResampler::new(input_sample_rate, SAMPLE_RATE)
        .expect("Native in-resampler creation failed");

    // Output: Gemini 24kHz → WebRTC 48kHz.
    let mut out_resampler = SoxrStreamResampler::new(OUTPUT_SAMPLE_RATE, WEBRTC_RATE)
        .expect("Native out-resampler creation failed");

    // ── Local VAD for barge-in ─────────────────────────────────────
    let vad_path = format!("{}/silero_vad/silero_vad.onnx", models_dir);
    let mut vad = VadStage::new(&vad_path, VadConfig::default());
    let vad_ok = vad.initialize().is_ok();
    if !vad_ok {
        warn!("[native] VAD init failed — barge-in disabled");
    }

    let mut ring = AudioRingBuffer::default();
    let mut bot_speaking = false;
    let mut bot_transcript_buf = String::new();
    let mut hangup_target: Option<tokio::time::Instant> = None;
    let mut hangup_max_target: Option<tokio::time::Instant> = None;

    // ── Main event loop ────────────────────────────────────────────
    loop {
        tokio::select! {
            _ = async {
                if let Some(target) = hangup_target {
                    tokio::time::sleep_until(target).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                info!("[native] Hangup delay elapsed. Terminating session.");

                // Fallback: flush any transcript that arrived during the drain window
                // but for which TurnComplete never came (Gemini omits it after tool calls).
                let bot_text = std::mem::take(&mut bot_transcript_buf);
                let bot_text_trimmed = bot_text.trim();
                if !bot_text_trimmed.is_empty() {
                    tracer.emit(Event::Transcript {
                        text: bot_text_trimmed.to_string(),
                        role: "assistant".into(),
                    });
                }

                let provider_name = "gemini_live";
                let model_name = nm_config.model.as_deref().unwrap_or("gemini_live");
                tracer.finish_turn(false, provider_name, model_name, &voice_id);

                let _ = transport.control_tx.send(voice_transport::TransportCommand::Close);
                break;
            }

            // Mic audio: resample → push to Gemini; also run VAD for barge-in.
            raw = mic_rx.recv() => {
                match raw {
                    Some(raw_bytes) => {
                        let resampled = in_resampler.process(&raw_bytes);

                        // Frame-align for VAD; collect audio to push async afterward.
                        let mut pending_pcm: Vec<Vec<i16>> = Vec::new();
                        let mut vad_event: Option<VadEvent> = None;

                        // Threshold is a packet-level decision: bot_speaking doesn't change
                        // within a process_frames batch, so set it once here.
                        // Raw (undenoised) audio goes to Gemini — raise threshold during playback
                        // to suppress background noise from falsely triggering a local barge-in.
                        if vad_ok {
                            vad.set_threshold(if bot_speaking {
                                VAD_THRESHOLD_PLAYBACK_RAW
                            } else {
                                VAD_THRESHOLD_IDLE
                            });
                        }

                        ring.process_frames(&resampled, |frame| {
                            if recording_enabled {
                                tracer.emit(Event::UserAudio {
                                    pcm: Bytes::copy_from_slice(frame),
                                    sample_rate: SAMPLE_RATE,
                                });
                            }
                            if vad_ok {
                                if let Some(ev) = vad.process(frame) {
                                    vad_event = Some(ev);
                                }
                            }
                            let samples: Vec<i16> = frame
                                .chunks_exact(2)
                                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                                .collect();
                            pending_pcm.push(samples);
                        });

                        for samples in pending_pcm {
                            if let Err(e) = backend.push_audio(&samples).await {
                                warn!("[native] push_audio error: {}", e);
                            }
                        }

                        // Barge-in when speech detected while bot is talking.
                        if let Some(VadEvent::SpeechStarted) = vad_event {
                            tracer.trace("SpeechStarted");
                            if bot_speaking {
                                info!("[native] Barge-in — interrupting Gemini");
                                if let Err(e) = backend.interrupt().await {
                                    warn!("[native] Interrupt failed: {}", e);
                                }
                                let _ = transport.audio_tx.interrupt().await;
                                bot_speaking = false;
                                playback.reset();

                                // Flush any partial output transcript that accumulated before
                                // the barge-in BEFORE calling cancel_turn(), so that:
                                //  a) Event::Transcript lands inside the open Langfuse turn span
                                //  b) tts_text (accumulated by append_tts_text) is still intact
                                //     if a TtsComplete is ever emitted from the turn.
                                // (Gemini does NOT emit TurnComplete on interruption.)
                                let partial = std::mem::take(&mut bot_transcript_buf);
                                let partial = partial.trim().to_string();
                                if !partial.is_empty() {
                                    // Emit the partial turn transcript before closing
                                    // the span so observers receive it while the turn
                                    // is still open.
                                    tracer.emit(Event::Transcript {
                                        text: partial,
                                        role: "assistant".into(),
                                    });
                                }
                                // If partial is empty, barge-in fired before any
                                // output transcript arrived — nothing to flush.

                                // Now close the turn span. cancel_turn() emits
                                // TurnEnded(was_interrupted=true) and clears tts_text.
                                // Everything above was emitted while the turn was still open.
                                tracer.cancel_turn();
                                tracer.trace("SpeechEnded");

                                // Signal downstream subscribers that the bot was
                                // interrupted and audio should be discarded.
                                // StateChanged notifies observers that the engine
                                // has returned to listening mode.
                                tracer.emit(Event::Interrupt);
                                tracer.emit(Event::StateChanged { state: "listening".into() });
                            }
                        }
                    }
                    None => {
                        info!("[native] Mic channel closed — ending session");
                        break;
                    }
                }
            }

            // Gemini events: audio out, transcripts, tool calls.
            event = backend.recv() => {
                match event {
                    Some(ev) => match ev {
                        NativeAgentEvent::BotAudio(samples) => {
                            if !bot_speaking {
                                bot_speaking = true;
                                // Snap the cursor to wall-clock on the first chunk of each
                                // new turn. This encodes the real inter-turn gap (user speech
                                // + STT + LLM + TTS TTFB) so recording is wall-clock accurate.
                                tts_cursor.begin_turn();
                                tracer.mark_tts_first_audio();
                                playback.reset();
                            }

                            let pcm_bytes: Vec<u8> =
                                samples.iter().flat_map(|s| s.to_le_bytes()).collect();

                            // Resample 24kHz → 48kHz before putting on the bus.
                            // The WebRTC forwarder and recording sinks both consume
                            // AgentAudio from the bus — this is the single audio path.
                            let upsampled = out_resampler.process(&pcm_bytes);

                            // Track bytes sent so remaining_playback() is accurate at hang_up.
                            playback.record(upsampled.len());

                            if hangup_target.is_some() {
                                let new_target =
                                    tokio::time::Instant::now() + playback.remaining_playback();
                                hangup_target = match hangup_max_target {
                                    Some(max_target) if new_target > max_target => {
                                        info!("[native] Playback extension exceeds 15s hard timeout. Clamping drain duration.");
                                        Some(max_target)
                                    }
                                    _ => Some(new_target),
                                };
                            }

                            // stamp() takes upsampled byte count (at WEBRTC_RATE = 48kHz),
                            // which matches the sample_rate we advertise below.
                            let offset = tts_cursor.stamp(upsampled.len());

                            tracer.emit(Event::AgentAudio {
                                pcm: Bytes::from(upsampled),
                                sample_rate: WEBRTC_RATE,
                                offset_samples: offset,
                            });
                        }
                        NativeAgentEvent::TurnComplete {
                            prompt_tokens,
                            completion_tokens,
                        } => {
                            bot_speaking = false;

                            let bot_text = std::mem::take(&mut bot_transcript_buf);
                            let bot_text_trimmed = bot_text.trim();

                            // Emit the canonical turn transcript to all observers
                            // once the turn is complete.
                            if !bot_text_trimmed.is_empty() {
                                tracer.emit(Event::Transcript {
                                    text: bot_text_trimmed.to_string(),
                                    role: "assistant".into(),
                                });
                                info!("[native] Agent turn complete: {}", bot_text_trimmed);
                            }

                            let provider_name = "gemini_live";
                            let model_name = nm_config.model.as_deref().unwrap_or("gemini_live");

                            tracer.emit(Event::LlmComplete(LlmCompletionData {
                                provider: provider_name.to_string(),
                                model: model_name.to_string(),
                                input_json: "{}".to_string(),
                                output_json: "{}".to_string(),
                                tools_json: None,
                                temperature: 0.0,
                                max_tokens: 0,
                                duration_ms: 0.0,
                                ttfb_ms: None,
                                prompt_tokens,
                                completion_tokens,
                                cache_read_tokens: None,
                                span_label: "llm".into(),
                            }));

                            tracer.finish_turn(false, provider_name, model_name, &voice_id);
                            info!(
                                "[native] Turn complete (prompt={}, completion={})",
                                prompt_tokens, completion_tokens
                            );
                        }
                        NativeAgentEvent::InputTranscript { text, is_final } => {
                            if is_final {
                                // Final transcript: emit to all observers and open a
                                // new tracer turn span for the upcoming agent response.
                                tracer.emit(Event::Transcript {
                                    text: text.clone(),
                                    role: "user".into(),
                                });
                                tracer.start_turn(
                                    "gemini_live",
                                    nm_config.model.as_deref().unwrap_or("gemini_live"),
                                    &text,
                                    &language,
                                    vad_ok,
                                );
                                info!("[native] User: {}", text);
                            } else {
                                // Non-final chunk: forwarded as an interim event.
                                tracer.emit(Event::TranscriptChunk {
                                    role: "user".into(),
                                    text: text.clone(),
                                    is_final: false,
                                });
                            }
                        }
                        NativeAgentEvent::OutputTranscript { text, is_final } => {
                            if !text.is_empty() {
                                if is_final {
                                    // The provider has canonicalized the full turn text.
                                    // Replace the streaming buffer so TurnComplete emits
                                    // exactly ONE Event::Transcript — not two.
                                    //
                                    // Do NOT emit here; the single canonical emit happens
                                    // at TurnComplete to avoid duplicate transcript events.
                                    bot_transcript_buf.clear();
                                    bot_transcript_buf.push_str(&text);
                                } else {
                                    // Non-final chunk: emit as an interim event and accumulate
                                    // into the buffer for the canonical TurnComplete emit.
                                    tracer.emit(Event::TranscriptChunk {
                                        role: "assistant".into(),
                                        text: text.clone(),
                                        is_final: false,
                                    });
                                    bot_transcript_buf.push_str(&text);
                                }
                            }
                            // Feed text to the TTS metrics accumulator so that finish_turn()
                            // emits a TtsComplete observability event (Langfuse tts span,
                            // character count for billing). These calls do NOT trigger any
                            // audio synthesis — the audio comes exclusively from the Bus path above.
                            tracer.mark_tts_text_fed();
                            tracer.append_tts_text(&text);
                        }
                        NativeAgentEvent::ToolCallStarted { id, name } => {
                            tracer.emit(Event::ToolActivity {
                                tool_call_id: Some(id),
                                tool_name: name.clone(),
                                status: "started".into(),
                                error_message: None,
                            });
                        }
                        NativeAgentEvent::ToolCallCompleted { name, success, .. } => {
                            tracer.emit(Event::ToolActivity {
                                tool_call_id: None,
                                tool_name: name.clone(),
                                status: if success { "completed".into() } else { "failed".into() },
                                error_message: None,
                            });
                        }
                        NativeAgentEvent::HangUp { reason } => {
                            if hangup_target.is_none() {
                                let delay = playback.remaining_playback();
                                let max_delay = std::time::Duration::from_secs(15);
                                let actual_delay = std::cmp::min(delay, max_delay);

                                info!(
                                    "[native] Agent hang_up (reason={}) intercepted. Commencing {:?} (max 15s) drain sequence before termination.",
                                    reason, actual_delay
                                );

                                let now = tokio::time::Instant::now();
                                hangup_target = Some(now + actual_delay);
                                hangup_max_target = Some(now + max_delay);

                                tracer.emit(Event::ToolActivity {
                                    tool_call_id: None,
                                    tool_name: "hang_up".into(),
                                    status: "completed".into(),
                                    error_message: None,
                                });
                            }
                        }

                        NativeAgentEvent::Error(msg) => {
                            warn!("[native] Provider error: {}", msg);
                            tracer.emit(Event::Error {
                                source: "gemini_live".into(),
                                message: msg,
                            });
                        }
                    }
                    None => {
                        // Stream ended — attempt reconnect with exponential backoff.
                        info!("[native] Provider stream ended — reconnecting");
                        let mut connected = false;
                        let mut backoff_ms = 500u64;
                        for attempt in 1..=5 {
                            if let Err(e) = backend.connect().await {
                                warn!("[native] Reconnect failed (attempt {}): {}", attempt, e);
                                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms))
                                    .await;
                                backoff_ms *= 2;
                            } else {
                                connected = true;
                                break;
                            }
                        }

                        if !connected {
                            error!("[native] Reconnect failed completely — ending session");
                            break;
                        }

                        // Reset session logic state. The tts_cursor is NOT reset —
                        // its monotonically increasing value prevents audio trace
                        // corruption by keeping all future chunks after the
                        // reconnection gap, not back at position 0.
                        bot_speaking = false;
                        tracer.cancel_turn();

                        info!("[native] Reconnected to Gemini Live");
                    }
                }
            }
        }
    }

    tracer.emit(Event::SessionEnded);
    info!("[native] Gemini Live session ended");
}
