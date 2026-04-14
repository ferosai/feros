//! Unified event types for the voice engine event bus.
//!
//! Every event the engine can produce is represented as a variant of [`Event`].
//! Each variant maps to exactly one [`EventCategory`], which subscribers use
//! to filter the stream.

use bytes::Bytes;
use serde::Serialize;

use crate::turn_tracker::TurnMetrics;

// ── Event Categories (for subscriber filtering) ─────────────────

/// Categories for subscriber filtering.
///
/// Each [`Event`] variant maps to exactly one category. Subscribers
/// specify a set of categories they care about; events outside that
/// set are silently skipped.
///
/// # Subscriber guidance
///
/// | Consumer | Categories |
/// |---|---|
/// | **WebSocket transport** | `Session`, `Transcript`, `Tool`, `Agent`, `AgentAudio`, `Error` |
/// | **WebRTC / Telephony transport** | `AgentAudio`, `Session` |
/// | **Server WS forwarder** (hybrid WebRTC) | `Session`, `Transcript`, `Tool`, `Agent`, `Metrics`, `Error` |
/// | **OTel subscriber** | `Session`, `Trace`, `Metrics`, `Tool`, `Transcript`, `Agent`, `Error` |
/// | **Langfuse subscriber** | `Observability` |
/// | **Recording subscriber** | `UserAudio`, `AgentAudio`, `Session`, `Transcript`, `Tool`, `Observability`, `Metrics` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventCategory {
    /// Session lifecycle: `SessionReady`, `StateChanged`, `Interrupt`, `SessionEnded`.
    ///
    /// Subscribe if you need connection state or session boundaries.
    /// All transports should subscribe. Low frequency.
    Session,

    /// Conversation text: `Transcript { role, text }`.
    ///
    /// Subscribe if you display or log what the user / assistant said.
    /// Not needed for audio-only transports (WebRTC, telephony).
    Transcript,

    /// Tool execution lifecycle: `ToolActivity { tool_call_id, tool_name, status, error_message }`.
    ///
    /// Subscribe if you display or audit tool calls (UI, logging, OTel).
    Tool,

    /// Agent lifecycle: `AgentEvent { kind }`.
    ///
    /// Subscribe to track high-level agent behavior (idle, barge-in, etc.).
    Agent,

    /// **Outbound** TTS audio chunks: `AgentAudio { pcm, sample_rate, offset_samples }` (binary PCM16).
    ///
    /// This is agent-generated speech only. User (inbound) audio flows
    /// through a separate `mpsc` channel and is **not** available on the
    /// bus. Subscribe only if you deliver audio to end users (transports).
    /// **High frequency** (~50 events/sec during speech).
    AgentAudio,

    /// Per-turn latency breakdown: `TurnMetrics(TurnMetrics)`.
    ///
    /// Subscribe for analytics dashboards, latency monitoring, or
    /// client-side latency display. One event per completed turn.
    Metrics,

    /// Raw debug breadcrumbs: `Trace { seq, elapsed_us, label }`.
    ///
    /// Low-frequency trace events emitted via [`Tracer::trace()`]. These
    /// represent meaningful state transitions (e.g. `BargeIn`, `HangUp`).
    Trace,

    /// Pipeline errors: `Error { source, message }`.
    ///
    /// Subscribe if you display errors to users or want alerting.
    Error,

    /// Per-service hierarchical traces: `TurnStarted`, `TurnEnded`,
    /// `SttComplete`, `LlmComplete`, `TtsComplete`.
    ///
    /// Subscribe if you produce Langfuse-style hierarchical traces
    /// with provider/model details, token counts, and service TTFBs.
    /// These events use `#[serde(skip)]` — not serializable to clients.
    Observability,

    /// Denoised user input audio: `UserAudio { pcm, sample_rate }` (PCM16).
    ///
    /// Only emitted when session recording is enabled. Subscribe if you
    /// are writing session recordings to disk.
    /// **High frequency**.
    UserAudio,
}

// ── Shared observability data structs ───────────────────────────

/// Data for an LLM generation completion event.
///
/// Shared between `Event::LlmComplete` and `LlmEvent::LlmComplete`
/// to avoid duplicating the same 12-field struct across crates.
#[derive(Debug, Clone)]
pub struct LlmCompletionData {
    pub provider: String,
    pub model: String,
    pub input_json: String,
    pub output_json: String,
    pub tools_json: Option<String>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub duration_ms: f64,
    pub ttfb_ms: Option<f64>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: Option<u32>,
    /// Span label for Langfuse: `"llm"`, `"llm_tool_req"`, or `"llm_tool_resp"`.
    pub span_label: String,
}

// ── Unified Event Enum ──────────────────────────────────────────

/// Every event the voice engine can produce.
///
/// JSON-serialized via serde for WebSocket consumers.
/// Binary variants (Audio) are skipped during serialization.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // ── Session lifecycle ───────────────────────────────
    /// Session is initialized and ready to receive audio.
    SessionReady,

    /// Session state has changed (listening, speaking, processing, etc.).
    StateChanged { state: String },

    /// Pipeline interrupted (barge-in). Client should flush audio buffers.
    Interrupt,

    /// Session ended by the agent (hang_up tool called).
    SessionEnded,

    // ── Conversation ────────────────────────────────────
    /// A transcript from the user or assistant.
    /// Emitted once per turn (for logging, DB, observability).
    Transcript { role: String, text: String },

    /// A streaming transcript chunk from the assistant, emitted mid-turn.
    ///
    /// Used exclusively by the native multimodal (Gemini Live) path to stream
    /// output transcription word-by-sentence to the UI without waiting for
    /// `TurnComplete`. The frontend should append each non-final chunk to the
    /// current in-progress assistant bubble rather than creating a new one.
    ///
    /// `is_final: true` signals that the turn has ended (or was interrupted)
    /// and the in-progress bubble can be closed. The `text` field is empty
    /// when used purely as a close-signal.
    TranscriptChunk {
        role: String,
        text: String,
        /// If `true`, this is the last chunk for this turn.
        is_final: bool,
    },

    // ── Agent / Tool activity ───────────────────────────
    /// A tool execution lifecycle event.
    ToolActivity {
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        tool_name: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },

    /// An agent lifecycle event (barge-in, idle, etc.).
    AgentEvent { kind: String },

    /// An error from the pipeline (LLM failure, TTS failure, etc.).
    Error { source: String, message: String },

    // ── Agent Audio ─────────────────────────────────────
    /// Agent (TTS) audio chunk (binary PCM data).
    ///
    /// `sample_rate` is the PCM16 sample rate of the chunk (e.g. 8 000 for
    /// telephony, 24 000 for cloud TTS, 48 000 for WebRTC).  Carrying it
    /// in the event lets the recording subscriber resample correctly without
    /// needing to know the rate at subscribe time.
    ///
    /// `offset_samples` is the reactor's monotonic agent-audio sample counter
    /// (at `sample_rate`) at the moment this chunk was emitted.  The recording
    /// subscriber uses it for exact placement — independent of broadcast channel
    /// latency, scheduling jitter, or how many chunks arrive in a burst.
    ///
    /// Skipped during JSON serialization — the WebSocket subscriber
    /// sends this as a binary frame instead.
    #[serde(skip)]
    AgentAudio {
        pcm: Bytes,
        sample_rate: u32,
        offset_samples: u64,
    },

    // ── Metrics ─────────────────────────────────────────
    /// Aggregated per-turn latency breakdown.
    TurnMetrics(TurnMetrics),

    // ── Raw trace (debug/observability) ─────────────────
    /// Low-level reactor event with microsecond timing.
    Trace {
        seq: u64,
        elapsed_us: u64,
        label: String,
    },

    // ── Langfuse / observability spans ──────────────────
    /// A new conversational turn has started.
    #[serde(skip)]
    TurnStarted { turn_number: u64 },

    /// The current turn has ended.
    #[serde(skip)]
    TurnEnded {
        turn_number: u64,
        was_interrupted: bool,
        /// Total turn duration in milliseconds (EOU decision → TTS finished).
        turn_duration_ms: Option<f64>,
        /// User-to-agent latency: user_stopped_speaking → agent_started_speaking (ms).
        user_agent_latency_ms: Option<f64>,
        /// VAD silence window duration in ms (how long VAD waited before confirming speech ended).
        vad_silence_ms: Option<f64>,
    },

    /// STT transcription completed.
    #[serde(skip)]
    SttComplete {
        provider: String,
        model: String,
        transcript: String,
        is_final: bool,
        language: Option<String>,
        duration_ms: f64,
        ttfb_ms: Option<f64>,
        vad_enabled: bool,
    },

    /// LLM generation completed.
    #[serde(skip)]
    LlmComplete(LlmCompletionData),

    /// TTS synthesis completed.
    #[serde(skip)]
    TtsComplete {
        provider: String,
        model: String,
        text: String,
        voice_id: String,
        character_count: usize,
        duration_ms: f64,
        ttfb_ms: Option<f64>,
        /// Time from first LLM token → first text fed to TTS (ms).
        /// Captures turn-completion buffering / sentence aggregation delay.
        text_aggregation_ms: Option<f64>,
    },

    // ── User Audio ──────────────────────────────────────
    /// Denoised user input audio frame (mono PCM16).
    ///
    /// Only emitted when session recording is enabled.
    /// High frequency.
    #[serde(skip)]
    UserAudio {
        pcm: Bytes,
        sample_rate: u32,
    },
}

impl Event {
    /// Returns the category this event belongs to.
    pub fn category(&self) -> EventCategory {
        match self {
            Event::SessionReady
            | Event::StateChanged { .. }
            | Event::Interrupt
            | Event::SessionEnded => EventCategory::Session,
            Event::Transcript { .. } | Event::TranscriptChunk { .. } => EventCategory::Transcript,
            Event::ToolActivity { .. } => EventCategory::Tool,
            Event::AgentEvent { .. } => EventCategory::Agent,
            Event::Error { .. } => EventCategory::Error,
            Event::AgentAudio { .. } => EventCategory::AgentAudio,
            Event::TurnMetrics(_) => EventCategory::Metrics,
            Event::Trace { .. } => EventCategory::Trace,
            Event::TurnStarted { .. }
            | Event::TurnEnded { .. }
            | Event::SttComplete { .. }
            | Event::LlmComplete(_)
            | Event::TtsComplete { .. } => EventCategory::Observability,
            Event::UserAudio { .. } => EventCategory::UserAudio,
        }
    }
}
