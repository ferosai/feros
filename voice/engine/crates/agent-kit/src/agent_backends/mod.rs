//! Agent backend trait and implementations.
//!
//! The [`AgentBackend`] trait is the reactor's single LLM/agent interface.
//! It absorbs swarm routing, per-agent config resolution, tool schema
//! building, tool execution, conversation management, and turn bookkeeping
//! so the reactor never needs to import `SwarmState`, `AgentGraphDef`,
//! `HttpExecutor`, `ChatMessage`, or `build_agent_tool_schemas`.

pub mod default;
pub mod native;

use async_trait::async_trait;

use crate::providers::LlmProviderError;
use serde::{Deserialize, Serialize};

use crate::swarm::{HANG_UP_TOOL_NAME, ON_HOLD_TOOL_NAME, TRANSFER_TOOL_NAME};

// ── Public Types ────────────────────────────────────────────────

// ── Chat Message ────────────────────────────────────────────────

/// A message in the OpenAI chat format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// ── Hub ↔ Orchestrator Protocol ─────────────────────────────────

/// Commands the Hub sends to the agent orchestrator.
///
/// The protocol is intentionally minimal — the orchestrator owns
/// tool execution, conversation history, and LLM streaming internally.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// User spoke — add to history, run the agent graph.
    ProcessUtterance(String),
    /// System directive (idle nudge, turn-completion reprompt).
    Nudge(String),
    /// Barge-in — abort current stream, clear all state.
    Cancel,
}

/// Events the agent orchestrator sends back to the Hub.
///
/// The Hub only needs speech tokens for TTS and status updates
/// for WebSocket client feedback.
#[derive(Debug, Clone)]
pub enum AgentOutput {
    /// A streamed text token for TTS.
    Token(String),
    /// Agent finished producing speech — stream complete.
    Finished,
    /// Status update for client UI (tool activity, thinking, etc.).
    Status {
        node: String,
        state: String,
        metadata: Option<serde_json::Value>,
    },
}

// ── Backward-compatible aliases ─────────────────────────────────
// These keep existing code compiling during the transition.
// TODO: Remove once all consumers are migrated.

pub type LlmCommand = AgentCommand;
pub type LlmOutput = AgentOutput;

/// Events emitted by the agent backend during a turn.
///
/// The backend runs the full agentic loop internally (LLM → tool calls →
/// execute → feed result → next LLM round). Observers (voice engine,
/// telemetry, etc.) consume these events without touching the agent's
/// internals.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A text token from the LLM stream.
    Token(String),
    /// A tool execution has started.
    ToolCallStarted {
        id: String,
        name: String,
        side_effect: bool,
    },
    /// A tool execution has completed.
    ToolCallCompleted {
        id: String,
        name: String,
        success: bool,
        error_message: Option<String>,
    },
    /// The entire agentic turn is complete (all tool rounds resolved).
    /// Contains the final assistant text content (if any) for the UI.
    Finished { content: Option<String> },
    /// Recoverable error.
    Error(String),
    /// The agent has decided to end the call.
    HangUp {
        reason: String,
        content: Option<String>,
    },
    /// The user asked to hold / pause — suppress idle shutdown until they return.
    OnHold { duration_secs: u32 },
    /// An LLM generation has completed (for observability).
    ///
    /// Emitted by the agent backend so the reactor can forward it
    /// to the event bus without the backend depending on voice-trace.
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
        /// Span label for Langfuse: `"llm"`, `"llm_tool_req"`, or `"llm_tool_resp"`.
        span_label: String,
    },
}

// ── Tool Call Hook ──────────────────────────────────────────────

/// Action returned by the before-interceptor to control tool execution.
#[derive(Debug, Clone)]
pub enum BeforeToolCallAction {
    /// Proceed with normal tool execution.
    Proceed,
    /// Skip execution entirely and use this string as the tool result.
    Stub(String),
}

/// Action returned by the after-interceptor to optionally override the result.
#[derive(Debug, Clone)]
pub enum AfterToolCallAction {
    /// Use the actual execution result as-is.
    PassThrough,
    /// Replace the result with this string.
    Override(String),
}

/// Hook for intercepting tool calls before and after execution.
///
/// Used primarily for testing — lets the caller stub tool results,
/// simulate failures, or override actual results without changing
/// the agent's tool definitions.
///
/// Hooks are **synchronous** (not async) because the primary consumer
/// is Python via PyO3, where the GIL makes async callbacks painful.
/// The interceptor logic is expected to be fast (return a string or decide
/// to proceed).
pub trait ToolInterceptor: Send + Sync {
    /// Called before a tool is executed.
    ///
    /// Return `Stub(result)` to skip execution and inject a canned result.
    /// Return `Proceed` to run the tool normally.
    fn before_tool_call(&self, tool_name: &str, arguments: &str) -> BeforeToolCallAction;

    /// Called after tool execution completes (only when `before_tool_call`
    /// returned `Proceed`).
    ///
    /// Return `Override(result)` to replace the actual result.
    /// Return `PassThrough` to use the real result.
    fn after_tool_call(
        &self,
        tool_name: &str,
        arguments: &str,
        result: &str,
    ) -> AfterToolCallAction;
}

/// Secrets map: provider key → encrypted value.
/// Values are `SecretString` — auto-zeroized on drop, redacted in Debug/Display.
pub type SecretMap = std::collections::HashMap<String, secrecy::SecretString>;

/// Thread-safe, swappable secret store.
///
/// Uses `RwLock` so that the background refresh task can update secrets
/// (write lock) while QuickJS `secret()` calls read concurrently (read lock).
/// This fixes stale OAuth tokens during long-running voice sessions.
pub type SharedSecretMap = std::sync::Arc<std::sync::RwLock<SecretMap>>;

/// Configuration for creating a `DefaultAgentBackend`.
#[derive(Clone)]
pub struct AgentBackendConfig {
    pub temperature: f64,
    pub max_tokens: u32,
    /// Maximum tool-call rounds per pipeline turn before giving up.
    pub max_tool_rounds: u32,
    /// Decrypted agent credentials, injected into the QuickJS script sandbox
    /// so scripts can call `secret("provider")` / `secret("provider.field")`.
    ///
    /// Wrapped in `Arc<RwLock<…>>` so the background token-refresh task can
    /// swap in fresh credentials mid-session, while QuickJS `secret()` reads
    /// always see the latest value via a non-blocking read lock.
    pub secrets: SharedSecretMap,
    /// Summarize long tool results before feeding them to the main LLM.
    pub tool_summarizer: bool,
    /// Compress conversation history when it grows too long.
    pub context_summarizer: bool,
    /// Speak a brief filler phrase while side-effecting tools run.
    /// Default: `false` (adds one extra LLM call per side-effect tool).
    pub tool_filler: bool,
}

impl std::fmt::Debug for AgentBackendConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBackendConfig")
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("max_tool_rounds", &self.max_tool_rounds)
            .field(
                "secrets",
                &format!(
                    "<{} keys>",
                    self.secrets.read().map(|s| s.len()).unwrap_or(0)
                ),
            )
            .field("tool_summarizer", &self.tool_summarizer)
            .field("context_summarizer", &self.context_summarizer)
            .field("tool_filler", &self.tool_filler)
            .finish()
    }
}

impl Default for AgentBackendConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_tokens: 32768,
            max_tool_rounds: 5,
            secrets: std::sync::Arc::new(std::sync::RwLock::new(SecretMap::new())),
            tool_summarizer: false,
            context_summarizer: false,
            tool_filler: false,
        }
    }
}

// ── Trait ────────────────────────────────────────────────────────

/// The reactor's sole agent interface.
///
/// Encapsulates provider selection, swarm routing, per-agent config
/// resolution, conversation management, tool execution, and turn
/// bookkeeping.
///
/// The backend **owns** the conversation history and the tool executor.
/// The reactor tells it "user said X" and observes lifecycle events.
#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// Set the system prompt (called once at init).
    fn set_system_prompt(&mut self, prompt: String);

    /// Add a user message to conversation history.
    fn add_user_message(&mut self, text: String);

    /// Add an assistant message to conversation history (e.g. a greeting).
    fn add_assistant_message(&mut self, text: String);

    /// Start a streaming LLM turn using internal conversation state.
    ///
    /// The backend drives the full agentic loop: if the LLM requests
    /// tool calls, they are executed internally and the results are fed
    /// back for another round — all yielded as events via `recv()`.
    async fn start_turn(&mut self) -> Result<(), LlmProviderError>;

    /// Poll for the next event from the active turn.
    ///
    /// Returns lifecycle events (`Token`, `ToolCallStarted`,
    /// `ToolCallCompleted`, `Finished`, `Error`). Returns `None` when
    /// the turn is idle.
    async fn recv(&mut self) -> Option<AgentEvent>;

    /// Cancel the active turn (including any in-flight tool tasks).
    ///
    /// Returns an optional `LlmComplete` event for the partial generation
    /// so the caller can forward it to observability before resetting.
    fn cancel(&mut self) -> Option<AgentEvent>;

    /// Handle a `transfer_to` tool call (swarm routing).
    /// Returns `true` if the transfer succeeded.
    fn handle_transfer(&mut self, target_agent: &str) -> bool;

    /// Returns `true` if the agent turn is currently active.
    fn is_active(&self) -> bool;

    /// Return the last `n` user/assistant messages from conversation history.
    ///
    /// Intended for testing, observability hooks, or future pipeline consumers
    /// that need recent context without maintaining a shadow copy.
    /// Only returns messages with role `"user"` or `"assistant"` (skips system,
    /// tool, etc.).
    fn recent_messages(&self, _n: usize) -> Vec<crate::agent_backends::ChatMessage> {
        Vec::new()
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Check if a tool call is the synthetic `transfer_to` tool.
pub fn is_transfer_tool(name: &str) -> bool {
    name == TRANSFER_TOOL_NAME
}

/// Check if a tool call is the synthetic `hang_up` tool.
pub fn is_hang_up_tool(name: &str) -> bool {
    name == HANG_UP_TOOL_NAME
}

/// Check if a tool call is the synthetic `on_hold` tool.
pub fn is_on_hold_tool(name: &str) -> bool {
    name == ON_HOLD_TOOL_NAME
}
