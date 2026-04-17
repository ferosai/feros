//! LLM stage — thin wrapper around `AgentBackend`.
//!
//! The Reactor calls `start()` to kick off an LLM generation and then
//! polls `recv()` in its central `select!` for tokens, tool lifecycle
//! events, and the `Finished` sentinel.
//!
//! All swarm routing, per-agent config resolution, conversation management,
//! tool execution, and turn bookkeeping are encapsulated inside the
//! `AgentBackend` implementation.

use crate::types::LlmEvent;
use agent_kit::agent_backends::{AgentBackend, AgentEvent};
use voice_trace::LlmCompletionData;

/// Convert an `AgentEvent` to an `LlmEvent`.
///
/// Shared by `recv()` and `cancel()` to avoid duplicating the
/// 12-field `LlmComplete` destructuring.
fn agent_event_to_llm(event: AgentEvent) -> LlmEvent {
    match event {
        AgentEvent::Token(t) => LlmEvent::Token(t),
        AgentEvent::ToolCallStarted {
            id,
            name,
            side_effect,
        } => LlmEvent::ToolCallStarted {
            id,
            name,
            side_effect,
        },
        AgentEvent::ToolCallCompleted {
            id,
            name,
            success,
            error_message,
        } => LlmEvent::ToolCallCompleted {
            id,
            name,
            success,
            error_message,
        },
        AgentEvent::Finished { content } => LlmEvent::Finished { content },
        AgentEvent::Error(e) => LlmEvent::Error(e),
        AgentEvent::HangUp { reason, content } => LlmEvent::HangUp { reason, content },
        AgentEvent::OnHold { duration_secs } => LlmEvent::OnHold { duration_secs },
        AgentEvent::EscalateCall {
            destination,
            reason,
        } => LlmEvent::EscalateCall {
            destination,
            reason,
        },
        AgentEvent::LlmComplete {
            provider,
            model,
            input_json,
            output_json,
            tools_json,
            temperature,
            max_tokens,
            duration_ms,
            ttfb_ms,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            span_label,
        } => LlmEvent::LlmComplete(LlmCompletionData {
            provider,
            model,
            input_json,
            output_json,
            tools_json,
            temperature,
            max_tokens,
            duration_ms,
            ttfb_ms,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            span_label,
        }),
    }
}

/// LLM stage: drives an LLM token stream via an opaque `AgentBackend`.
pub struct LlmStage {
    backend: Box<dyn AgentBackend>,
}

impl LlmStage {
    pub fn new(backend: Box<dyn AgentBackend>) -> Self {
        Self { backend }
    }

    /// Set the system prompt (called once at init).
    pub fn set_system_prompt(&mut self, prompt: String) {
        self.backend.set_system_prompt(prompt);
    }

    /// Add a user message to the backend's conversation history.
    pub fn add_user_message(&mut self, text: String) {
        self.backend.add_user_message(text);
    }

    /// Add an assistant message (e.g. greeting) to the backend's history.
    pub fn add_assistant_message(&mut self, text: String) {
        self.backend.add_assistant_message(text);
    }

    /// Start an LLM generation using the backend's internal conversation.
    ///
    /// The backend drives the full agentic loop (LLM ↔ tools) internally.
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.backend
            .start_turn()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Poll for the next LLM event.
    /// Returns `None` when no active stream or stream is complete.
    /// This is what the Reactor polls in `select!`.
    pub async fn recv(&mut self) -> Option<LlmEvent> {
        let event = self.backend.recv().await?;
        Some(agent_event_to_llm(event))
    }

    /// Cancel current generation (including in-flight tool tasks).
    ///
    /// Returns an optional `LlmEvent::LlmComplete` for the partial generation
    /// so the reactor can forward it to the event bus before resetting.
    pub fn cancel(&mut self) -> Option<LlmEvent> {
        let agent_event = self.backend.cancel()?;
        match agent_event_to_llm(agent_event) {
            llm_event @ LlmEvent::LlmComplete(_) => Some(llm_event),
            _ => None,
        }
    }

    /// Handle a `transfer_to` tool call (swarm routing).
    /// Returns `true` if the transfer succeeded.
    #[allow(dead_code)]
    pub fn handle_transfer(&mut self, target_agent: &str) -> bool {
        self.backend.handle_transfer(target_agent)
    }

    /// True if the agent turn is currently active.
    pub fn is_active(&self) -> bool {
        self.backend.is_active()
    }
}
