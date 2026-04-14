//! Serializable projection of bus events for storage sinks (Postgres, etc.).
//!
//! # Why this exists
//!
//! [`Event`] has two roles that are in tension:
//!
//! 1. **Wire format** — it is JSON-serialized and sent to frontend WebSocket
//!    clients.  To prevent internal-only fields from leaking to clients, many
//!    variants carry `#[serde(skip)]` on the entire variant (e.g. `SttComplete`,
//!    `LlmComplete`, `TtsComplete`, `TurnStarted`, `TurnEnded`).
//!
//! 2. **Storage format** — server-side sinks (Postgres `call_events` table,
//!    future audit logs) need to persist *all* structured observability events,
//!    including the ones hidden from clients.
//!
//! [`SinkEvent`] is a fully serializable mirror of the bus events that matter
//! for storage.  It does not share the `#[serde(skip)]` constraints of `Event`,
//! so every variant serializes cleanly to JSON.
//!
//! # What to use where
//!
//! | Concern | Use |
//! |---|---|
//! | Sending events to frontend clients (WebSocket) | [`Event`] directly — `#[serde(skip)]` hides internal fields automatically |
//! | Emitting OTel/Langfuse spans | [`Event`] directly — field access is cheaper than cloning into `SinkEvent` |
//! | Writing rows to Postgres / audit logs | [`SinkEvent`] via [`from_event`] |

use crate::event::Event;
use crate::turn_tracker::TurnMetrics;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SinkEvent {
    SessionReady,
    StateChanged {
        state: String,
    },
    Interrupt,
    SessionEnded,
    Transcript {
        role: String,
        text: String,
    },
    ToolActivity {
        tool_name: String,
        status: String,
    },
    AgentEvent {
        kind: String,
    },
    Error {
        source: String,
        message: String,
    },
    TurnMetrics(TurnMetrics),
    TurnStarted {
        turn_number: u64,
    },
    TurnEnded {
        turn_number: u64,
        was_interrupted: bool,
        turn_duration_ms: Option<f64>,
        user_agent_latency_ms: Option<f64>,
        vad_silence_ms: Option<f64>,
    },
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
    LlmComplete {
        provider: String,
        model: String,
        input_json: String,
        output_json: String,
        tools_json: Option<String>,
        temperature: f64,
        max_tokens: u32,
        duration_ms: f64,
        ttfb_ms: Option<f64>,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: Option<u32>,
        span_label: String,
    },
    TtsComplete {
        provider: String,
        model: String,
        text: String,
        voice_id: String,
        character_count: usize,
        duration_ms: f64,
        ttfb_ms: Option<f64>,
        text_aggregation_ms: Option<f64>,
    },
}

pub fn from_event(event: &Event) -> Option<SinkEvent> {
    Some(match event {
        Event::SessionReady => SinkEvent::SessionReady,
        Event::StateChanged { state } => SinkEvent::StateChanged {
            state: state.clone(),
        },
        Event::Interrupt => SinkEvent::Interrupt,
        Event::SessionEnded => SinkEvent::SessionEnded,
        Event::Transcript { role, text } => SinkEvent::Transcript {
            role: role.clone(),
            text: text.clone(),
        },
        Event::ToolActivity {
            tool_name,
            status,
            ..
        } => SinkEvent::ToolActivity {
            tool_name: tool_name.clone(),
            status: status.clone(),
        },
        Event::AgentEvent { kind } => SinkEvent::AgentEvent { kind: kind.clone() },
        Event::Error { source, message } => SinkEvent::Error {
            source: source.clone(),
            message: message.clone(),
        },
        Event::TurnMetrics(m) => SinkEvent::TurnMetrics(m.clone()),
        Event::TurnStarted { turn_number } => SinkEvent::TurnStarted {
            turn_number: *turn_number,
        },
        Event::TurnEnded {
            turn_number,
            was_interrupted,
            turn_duration_ms,
            user_agent_latency_ms,
            vad_silence_ms,
        } => SinkEvent::TurnEnded {
            turn_number: *turn_number,
            was_interrupted: *was_interrupted,
            turn_duration_ms: *turn_duration_ms,
            user_agent_latency_ms: *user_agent_latency_ms,
            vad_silence_ms: *vad_silence_ms,
        },
        Event::SttComplete {
            provider,
            model,
            transcript,
            is_final,
            language,
            duration_ms,
            ttfb_ms,
            vad_enabled,
        } => SinkEvent::SttComplete {
            provider: provider.clone(),
            model: model.clone(),
            transcript: transcript.clone(),
            is_final: *is_final,
            language: language.clone(),
            duration_ms: *duration_ms,
            ttfb_ms: *ttfb_ms,
            vad_enabled: *vad_enabled,
        },
        Event::LlmComplete(data) => SinkEvent::LlmComplete {
            provider: data.provider.clone(),
            model: data.model.clone(),
            input_json: data.input_json.clone(),
            output_json: data.output_json.clone(),
            tools_json: data.tools_json.clone(),
            temperature: data.temperature,
            max_tokens: data.max_tokens,
            duration_ms: data.duration_ms,
            ttfb_ms: data.ttfb_ms,
            prompt_tokens: data.prompt_tokens,
            completion_tokens: data.completion_tokens,
            cache_read_tokens: data.cache_read_tokens,
            span_label: data.span_label.clone(),
        },
        Event::TtsComplete {
            provider,
            model,
            text,
            voice_id,
            character_count,
            duration_ms,
            ttfb_ms,
            text_aggregation_ms,
        } => SinkEvent::TtsComplete {
            provider: provider.clone(),
            model: model.clone(),
            text: text.clone(),
            voice_id: voice_id.clone(),
            character_count: *character_count,
            duration_ms: *duration_ms,
            ttfb_ms: *ttfb_ms,
            text_aggregation_ms: *text_aggregation_ms,
        },
        Event::AgentAudio { .. } | Event::Trace { .. } | Event::UserAudio { .. } | Event::TranscriptChunk { .. } => return None,
    })
}

pub fn event_type_name(event: &SinkEvent) -> &'static str {
    match event {
        SinkEvent::SessionReady => "session_ready",
        SinkEvent::StateChanged { .. } => "state_changed",
        SinkEvent::Interrupt => "interrupt",
        SinkEvent::SessionEnded => "session_ended",
        SinkEvent::Transcript { .. } => "transcript",
        SinkEvent::ToolActivity { .. } => "tool_activity",
        SinkEvent::AgentEvent { .. } => "agent_event",
        SinkEvent::Error { .. } => "error",
        SinkEvent::TurnMetrics(_) => "turn_metrics",
        SinkEvent::TurnStarted { .. } => "turn_started",
        SinkEvent::TurnEnded { .. } => "turn_ended",
        SinkEvent::SttComplete { .. } => "stt_complete",
        SinkEvent::LlmComplete { .. } => "llm_complete",
        SinkEvent::TtsComplete { .. } => "tts_complete",
    }
}

pub fn event_category_name(event: &SinkEvent) -> &'static str {
    match event {
        SinkEvent::SessionReady
        | SinkEvent::StateChanged { .. }
        | SinkEvent::Interrupt
        | SinkEvent::SessionEnded => "session",
        SinkEvent::Transcript { .. } => "transcript",
        SinkEvent::ToolActivity { .. } => "tool",
        SinkEvent::AgentEvent { .. } => "agent",
        SinkEvent::Error { .. } => "error",
        SinkEvent::TurnMetrics(_) => "metrics",
        SinkEvent::TurnStarted { .. }
        | SinkEvent::TurnEnded { .. }
        | SinkEvent::SttComplete { .. }
        | SinkEvent::LlmComplete { .. }
        | SinkEvent::TtsComplete { .. } => "observability",
    }
}
