//! VoiceSession — thin coordinator that spawns the Reactor.
//!
//! One VoiceSession per active call. Creates channels, starts the Reactor,
//! and exposes a minimal API for the transport handler.

use std::sync::Arc;

use agent_kit::swarm::AgentGraphDef;
use tracing::info;
use voice_trace::Tracer;
use voice_transport::TransportHandle;

use crate::audio_ml::denoiser::DenoiserBackend;
use crate::providers::stt::SttProviderConfig;
use crate::providers::tts::TtsProviderConfig;
use crate::reactor::{AgentAudioCursor, Reactor};
use crate::settings::AgentTaskSettings;
use agent_kit::agent_backends::SharedSecretMap;
use agent_kit::providers::LlmProvider;
use agent_kit::RecordingConfig;
use agent_kit::{
    AgentBackend, AgentBackendConfig, ArtifactInterceptor, ArtifactStore, DefaultAgentBackend,
};

// ── Native Multimodal Config ─────────────────────────────────────

/// When set on a session, the STT/LLM/TTS pipeline is bypassed entirely.
/// Audio flows directly between WebRTC and the Gemini Live WebSocket.
#[derive(Clone, Debug)]
pub struct NativeMultimodalConfig {
    /// Gemini API key for the Live (bidirectional audio) endpoint.
    pub api_key: String,
    /// Model override. Defaults to `models/gemini-3.1-flash-live-preview`.
    pub model: Option<String>,
}

// ── Configuration ───────────────────────────────────────────────

/// Configuration for a voice session.
#[derive(Clone)]
pub struct SessionConfig {
    pub agent_id: String,
    pub system_prompt: String,
    pub voice_id: String,
    pub language: String,
    pub temperature: f64,
    pub max_tokens: u32,
    pub input_sample_rate: u32,
    /// Output sample rate for TTS audio (e.g. 24000 for browser, 8000 for telephony).
    pub output_sample_rate: u32,
    pub greeting: Option<String>,
    /// Path to models directory. Default: `./dsp_models`
    pub models_dir: String,
    /// Smart turn threshold. Default: 0.5
    pub smart_turn_threshold: f32,
    /// Enable denoiser. Default: true
    pub denoise_enabled: bool,
    /// Which denoiser backend to use. Default: RNNoise
    pub denoise_backend: DenoiserBackend,
    /// Enable smart turn. Default: true
    pub smart_turn_enabled: bool,

    // ── Turn Wisdom features ────────────────────────────────
    /// Enable LLM turn completion marker detection (✓/○/◐).
    pub turn_completion_enabled: bool,

    /// Seconds of user silence before sending a re-engagement prompt.
    pub idle_timeout_secs: u32,

    /// Number of idle nudges to send before hanging up.
    pub idle_max_nudges: u32,

    /// Minimum number of STT words required before a barge-in is triggered.
    pub min_barge_in_words: u32,

    /// Maximum ms to wait after SpeechStarted before triggering barge-in.
    pub barge_in_timeout_ms: u32,

    /// Expected P99 latency from speech end to final STT transcript (ms).
    pub stt_p99_latency_ms: u32,

    // ── Tool execution ──────────────────────────────────────────
    /// Optional agent graph for multi-node / swarm routing.
    pub agent_graph: Option<AgentGraphDef>,

    // ── Observability labels ────────────────────────────────────
    pub stt_provider: String,
    pub stt_model: String,
    pub tts_provider: String,
    pub tts_model: String,

    // ── Session Recording ───────────────────────────────────────
    pub recording: RecordingConfig,

    // ── Native Multimodal (Gemini Live) ─────────────────────────
    /// When `Some`, the session bypasses STT/LLM/TTS entirely and uses
    /// Gemini Live's bidirectional audio WebSocket instead.
    pub native_multimodal: Option<NativeMultimodalConfig>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("agent_id", &self.agent_id)
            .field("system_prompt", &"<redacted>")
            .field("voice_id", &self.voice_id)
            .field("language", &self.language)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("input_sample_rate", &self.input_sample_rate)
            .field("output_sample_rate", &self.output_sample_rate)
            .field("greeting", &self.greeting.as_deref().map(|_| "<set>"))
            .field("agent_graph", &self.agent_graph.as_ref().map(|_| "<set>"))
            .field(
                "native_multimodal",
                &self.native_multimodal.as_ref().map(|_| "<set>"),
            )
            .finish()
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            system_prompt: "You are a helpful voice assistant.".to_string(),
            voice_id: "default".to_string(),
            language: "en".to_string(),
            temperature: 0.7,
            max_tokens: 32768,
            input_sample_rate: 48_000,
            output_sample_rate: 24_000,
            greeting: None,
            models_dir: "./dsp_models".to_string(),
            smart_turn_threshold: 0.5,
            denoise_enabled: true,
            denoise_backend: DenoiserBackend::default(),
            smart_turn_enabled: true,
            turn_completion_enabled: true,
            idle_timeout_secs: 4,
            idle_max_nudges: 2,
            min_barge_in_words: 2,
            barge_in_timeout_ms: 800,
            stt_p99_latency_ms: 1000,
            agent_graph: None,
            stt_provider: "unknown".to_string(),
            stt_model: String::new(),
            tts_provider: "unknown".to_string(),
            tts_model: String::new(),
            recording: RecordingConfig::default(),
            native_multimodal: None,
        }
    }
}

impl SessionConfig {
    /// Apply derived mutations that depend on multiple fields.
    pub fn finalize(mut self) -> Self {
        if !self.language.is_empty() && self.language != "en" {
            use crate::language_config::SUPPORTED_LANGUAGES;
            let lang_label = SUPPORTED_LANGUAGES
                .iter()
                .find(|l| l.code == self.language.as_str())
                .map(|l| l.label)
                .unwrap_or(self.language.as_str());
            let instruction = format!(
                "IMPORTANT: Always respond in {lang_label}. \
                 Never switch languages regardless of what language the user speaks.\n\n"
            );
            self.system_prompt = format!("{instruction}{}", self.system_prompt);
        }

        // Capitalize first letter of voice_id for Gemini Live native multimodal path only.
        // Gemini expects e.g. "Kore" not "kore". Standard TTS providers (Cartesia, ElevenLabs,
        // Deepgram) use their own case-sensitive voice names and must NOT be modified.
        if self.native_multimodal.is_some() && !self.voice_id.is_empty() {
            let mut c = self.voice_id.chars();
            if let Some(f) = c.next() {
                self.voice_id =
                    f.to_uppercase().collect::<String>() + c.as_str().to_lowercase().as_str();
            }
        }

        self
    }
}

// ── Voice Session ───────────────────────────────────────────────

/// A single voice conversation session backed by the Reactor.
pub struct VoiceSession {
    reactor_handle: Option<tokio::task::JoinHandle<()>>,
    /// Keeps the transport alive (background tasks are aborted on drop).
    _transport: TransportHandle,
}

impl VoiceSession {
    /// Create and start a new voice session.
    ///
    /// When `config.native_multimodal` is `Some`, the STT/LLM/TTS pipeline is
    /// bypassed and audio flows directly to/from the Gemini Live WebSocket.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        _session_id: String,
        config: SessionConfig,
        secrets: SharedSecretMap,
        llm_provider: Box<dyn LlmProvider>,
        stt_config: SttProviderConfig,
        tts_config: TtsProviderConfig,
        tracer: Tracer,
        mut transport: TransportHandle,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // ── Native Multimodal (Gemini Live) path ───────────────────────────
        if let Some(nm_config) = config.native_multimodal.clone() {
            // Pull mic audio receiver; transport (with audio_tx sink) is moved into the task.
            let audio_in_rx = transport.take_audio_rx();

            let agent_graphdef = config.agent_graph.clone();
            let system_prompt = config.system_prompt.clone();
            let voice_id = config.voice_id.clone();
            let input_sample_rate = config.input_sample_rate;

            let task = AgentTaskSettings::get();
            let backend_config = AgentBackendConfig {
                temperature: config.temperature,
                max_tokens: config.max_tokens,
                max_tool_rounds: 5,
                secrets,
                tool_summarizer: task.agent_tool_summarizer,
                context_summarizer: task.agent_context_summarizer,
                tool_filler: task.agent_tool_filler,
            };

            let models_dir = config.models_dir.clone();
            let recording_enabled = config.recording.enabled;

            // Move the transport into the task so its audio_tx sink stays alive.
            let reactor_handle = tokio::spawn(async move {
                run_native_multimodal(
                    nm_config,
                    agent_graphdef,
                    system_prompt,
                    voice_id,
                    backend_config,
                    audio_in_rx,
                    transport,
                    tracer,
                    input_sample_rate,
                    models_dir,
                    recording_enabled,
                    config.language.clone(),
                    config.greeting.clone(),
                )
                .await;
            });

            info!("Voice session started (Gemini Live native audio)");
            return Ok(Self {
                reactor_handle: Some(reactor_handle),
                // Real transport was moved into the task; placeholder keeps the struct valid.
                _transport: TransportHandle::dummy(),
            });
        }

        // ── Standard STT/LLM/TTS Reactor path ───────────────────────────────
        if tts_config.voice_id.is_empty() {
            return Err("A voice must be configured to start the session.".into());
        }

        let audio_in_rx = transport.take_audio_rx();

        let agent_graph = config.agent_graph.clone();
        let task = AgentTaskSettings::get();
        let backend_config = AgentBackendConfig {
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            max_tool_rounds: 5,
            secrets,
            tool_summarizer: task.agent_tool_summarizer,
            context_summarizer: task.agent_context_summarizer,
            tool_filler: task.agent_tool_filler,
        };

        let llm_provider: Arc<dyn LlmProvider> = Arc::from(llm_provider);
        let backend: Box<dyn AgentBackend> = Box::new(
            DefaultAgentBackend::new(llm_provider, agent_graph, backend_config).with_interceptor(
                std::sync::Arc::new(ArtifactInterceptor::new(ArtifactStore::new())),
            ),
        );

        let transport_control_tx = transport.control_tx.clone();

        let mut reactor = Reactor::new(
            config,
            backend,
            audio_in_rx,
            tracer,
            stt_config,
            tts_config,
            transport_control_tx,
        );

        reactor.start().await.map_err(|e| e.to_string())?;

        let reactor_handle = tokio::spawn(async move {
            reactor.run().await;
        });

        info!("Voice session started (Reactor architecture)");

        Ok(Self {
            reactor_handle: Some(reactor_handle),
            _transport: transport,
        })
    }

    /// Close the session cleanly.
    pub async fn close(&mut self) {
        if let Some(handle) = self.reactor_handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        info!("Voice session closed");
    }

    /// Wait for the session to complete.
    pub async fn wait_for_completion(&mut self) {
        if let Some(handle) = self.reactor_handle.take() {
            let _ = handle.await;
        }
    }
}

// ── Native Multimodal Event Loop ───────────────────────────────────────

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
async fn run_native_multimodal(
    nm_config: NativeMultimodalConfig,
    agent_graph: Option<AgentGraphDef>,
    system_prompt: String,
    voice_id: String,
    backend_config: AgentBackendConfig,
    mut mic_rx: tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>,
    transport: TransportHandle,
    mut tracer: Tracer,
    input_sample_rate: u32,
    models_dir: String,
    recording_enabled: bool,
    language: String,
    greeting: Option<String>,
) {
    use agent_kit::agent_backends::native::{NativeAgentEvent, NativeMultimodalBackend};
    use agent_kit::providers::gemini_live::{GeminiLiveProvider, OUTPUT_SAMPLE_RATE};
    use agent_kit::AgentBackend as _;
    use bytes::Bytes;
    use tracing::{error, info, warn};
    use voice_trace::Event;

    /// WebRTC Opus clock rate.
    const WEBRTC_RATE: u32 = 48_000;

    tracer.emit(Event::SessionReady);

    // ── Build provider and backend ─────────────────────────────────
    let api_key = nm_config.api_key.clone();

    if api_key.is_empty() || api_key.trim().is_empty() {
        tracing::error!("[native] No Gemini API key found in the Native Multimodal configuration block. Terminating session.");
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
    let mut playback = crate::utils::PlaybackTracker::new(WEBRTC_RATE);

    let provider = Box::new(GeminiLiveProvider::new(api_key, nm_config.model.clone()));
    let mut backend = NativeMultimodalBackend::new(
        provider,
        agent_graph.as_ref(), // No tools support yet? Graph is passed.
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
    let mut in_resampler =
        soxr::SoxrStreamResampler::new(input_sample_rate, crate::utils::SAMPLE_RATE)
            .expect("Native in-resampler creation failed");

    // Output: Gemini 24kHz → WebRTC 48kHz.
    let mut out_resampler = soxr::SoxrStreamResampler::new(OUTPUT_SAMPLE_RATE, WEBRTC_RATE)
        .expect("Native out-resampler creation failed");

    // ── Local VAD for barge-in ─────────────────────────────────────
    let vad_path = format!("{}/silero_vad/silero_vad.onnx", models_dir);
    let mut vad = crate::reactor::proc::vad::VadStage::new(
        &vad_path,
        crate::audio_ml::vad::VadConfig::default(),
    );
    let vad_ok = vad.initialize().is_ok();
    if !vad_ok {
        warn!("[native] VAD init failed — barge-in disabled");
    }

    let mut ring = crate::utils::AudioRingBuffer::default();
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
                        let mut vad_event: Option<crate::types::VadEvent> = None;
                        ring.process_frames(&resampled, |frame| {
                            if recording_enabled {
                                tracer.emit(Event::UserAudio {
                                    pcm: Bytes::copy_from_slice(frame),
                                    sample_rate: crate::utils::SAMPLE_RATE,
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
                        if let Some(crate::types::VadEvent::SpeechStarted) = vad_event {
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
                                    // Emit for DB / observability sinks AND as the UI close
                                    // signal. The transcript handler in the UI will see an
                                    // open (_final:false) streaming bubble and replace it.
                                    tracer.emit(Event::Transcript {
                                        text: partial,
                                        role: "assistant".into(),
                                    });
                                }
                                // If partial is empty there is no open bubble to close —
                                // barge-in fired before any output transcript arrived.

                                // Now close the turn span. cancel_turn() emits
                                // TurnEnded(was_interrupted=true) and clears tts_text.
                                // Everything above was emitted while the turn was still open.
                                tracer.cancel_turn();
                                tracer.trace("SpeechEnded");

                                // Notify the client that a barge-in occurred so it can
                                // flush any audio it is playing (matches standard Reactor
                                // behaviour). Also flip the state indicator back to listening.
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

                            let pcm_bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();

                            // Resample 24kHz → 48kHz before putting on the bus.
                            // The WebRTC forwarder and recording sinks both consume
                            // AgentAudio from the bus — this is the single audio path.
                            let upsampled = out_resampler.process(&pcm_bytes);

                            // Track bytes sent so remaining_playback() is accurate at hang_up.
                            playback.record(upsampled.len());

                            if hangup_target.is_some() {
                                let new_target = tokio::time::Instant::now() + playback.remaining_playback();
                                hangup_target = match hangup_max_target {
                                    Some(max_target) if new_target > max_target => {
                                        info!("[native] Playback extension exceeds 10s hard timeout. Clamping drain duration.");
                                        Some(max_target)
                                    },
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
                        NativeAgentEvent::TurnComplete { prompt_tokens, completion_tokens } => {
                            bot_speaking = false;

                            let bot_text = std::mem::take(&mut bot_transcript_buf);
                            let bot_text_trimmed = bot_text.trim();

                            // Emit the full turn text for DB / observability sinks AND as the
                            // UI close signal (the frontend transcript handler replaces the
                            // open streaming bubble with this canonical text).
                            if !bot_text_trimmed.is_empty() {
                                tracer.emit(Event::Transcript {
                                    text: bot_text_trimmed.to_string(),
                                    role: "assistant".into(),
                                });
                                info!("[native] Agent turn complete: {}", bot_text_trimmed);
                            }

                            let provider_name = "gemini_live";
                            let model_name = nm_config.model.as_deref().unwrap_or("gemini_live");

                            use voice_trace::event::LlmCompletionData;
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
                            info!("[native] Turn complete (prompt={}, completion={})", prompt_tokens, completion_tokens);
                        }
                        NativeAgentEvent::InputTranscript { text, is_final } => {
                            if is_final {
                                // Canonical transcript: goes to DB / Langfuse AND closes the
                                // open streaming bubble in the UI (transcript handler checks
                                // _final and replaces rather than duplicating).
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
                                // Non-final: stream chunk to UI only (no DB write).
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
                                    // Do NOT emit here. Emitting both here AND at TurnComplete
                                    // creates two bubbles in the UI (was the double-bubble bug).
                                    bot_transcript_buf.clear();
                                    bot_transcript_buf.push_str(&text);
                                } else {
                                    // Non-final chunk: stream to UI, accumulate for turn-level log.
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

                                info!("[native] Agent hang_up (reason={}) intercepted. Commencing {:?} (max 15s) drain sequence before termination.", reason, actual_delay);

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
                        let mut backoff_ms = 500;
                        for attempt in 1..=5 {
                            if let Err(e) = backend.connect().await {
                                warn!("[native] Reconnect failed (attempt {}): {}", attempt, e);
                                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
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

#[cfg(test)]
mod tests {
    fn clean_for_tts(token: &str) -> String {
        token
            .chars()
            .filter(|c| !c.is_ascii_control() && *c != '*' && *c != '#' && *c != '_')
            .filter(|c| {
                !matches!(c, '\u{1F600}'..='\u{1F6FF}' | '\u{2600}'..='\u{26FF}' | '\u{2700}'..='\u{27BF}')
            })
            .collect()
    }

    #[test]
    fn test_clean_for_tts() {
        assert_eq!(clean_for_tts("Hello **world**!"), "Hello world!");
        assert_eq!(clean_for_tts("## Heading"), " Heading");
    }
}
