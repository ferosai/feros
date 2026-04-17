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
use crate::native_session::run_native_multimodal;
use crate::providers::stt::SttProviderConfig;
use crate::providers::tts::TtsProviderConfig;
use crate::reactor::Reactor;
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
