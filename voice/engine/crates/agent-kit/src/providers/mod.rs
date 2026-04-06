//! LLM provider abstraction — pluggable backends for chat completion.
//!
//! The `LlmProvider` trait defines the interface that all LLM backends
//! must implement. The Reactor and LlmStage are provider-agnostic —
//! they only see this trait.
//!
//! Built-in implementations live in the `providers` module (OpenAI,
//! Anthropic, Gemini, DeepSeek, Groq — all via rig-core).

use async_trait::async_trait;
use tokio::sync::mpsc;

use serde::{Deserialize, Serialize};

use crate::agent_backends::ChatMessage;

// ── Call Configuration ──────────────────────────────────────────

/// Per-call configuration for an LLM completion request.
///
/// Bundled into a struct so callers don't need 4+ positional params.
#[derive(Debug, Clone)]
pub struct LlmCallConfig {
    pub temperature: f64,
    pub max_tokens: u32,
    /// Model override (e.g., per-agent in swarm mode).
    /// If `None`, use the provider's default model.
    pub model: Option<String>,
}

// ── LLM Stream Events ──────────────────────────────────────────

/// An event yielded during LLM streaming: either a text token or a tool call.
#[derive(Debug)]
pub enum LlmEvent {
    Token(String),
    ToolCall(ToolCallEvent),
    /// Token usage from the LLM response (sent at stream end if available).
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        /// Cached prompt tokens read (prompt caching). 0 if not reported.
        cached_input_tokens: u32,
    },
    /// Mid-stream error from the provider (network failure, rate limit, etc.).
    ///
    /// Sending this through the channel — rather than silently closing it —
    /// lets the backend convert it to `AgentEvent::Error` so the reactor can
    /// log the failure and arm its idle timer instead of going permanently silent.
    Error(String),
}

/// A tool invocation requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEvent {
    pub name: String,
    pub arguments: String,
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

// ── Error Type ──────────────────────────────────────────────────

/// Errors from LLM providers.
#[derive(Debug)]
pub enum LlmProviderError {
    /// Network, DNS, or HTTP transport error.
    Transport(String),
    /// Authentication failure (401/403).
    Auth(String),
    /// Provider-specific error (rate limit, model not found, etc.).
    Provider(String),
}

impl std::fmt::Display for LlmProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "transport: {}", msg),
            Self::Auth(msg) => write!(f, "auth: {}", msg),
            Self::Provider(msg) => write!(f, "provider: {}", msg),
        }
    }
}

impl std::error::Error for LlmProviderError {}

// ── Provider Trait ──────────────────────────────────────────────

/// A pluggable LLM backend that streams chat completions.
///
/// Implementations handle vendor-specific HTTP protocol, authentication,
/// and response parsing. The `LlmStage` and Reactor never know which
/// vendor is being used.
///
/// The trait is intentionally narrow — just "give me a streaming completion."
/// Conversation management, tool execution, and swarm routing remain in
/// the `LlmStage` and Reactor.
///
/// # Cancellation
///
/// Dropping the returned `Receiver` cancels the stream. Implementations
/// must spawn a background task that exits on send error — this is how
/// Rust ownership provides zero-cost cancellation.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Human-readable provider name for observability (e.g. "groq", "openai").
    ///
    /// Used in Langfuse traces as the `gen_ai.system` attribute.
    fn provider_name(&self) -> &str {
        "unknown"
    }

    /// Stream a chat completion.
    ///
    /// Returns a channel of `LlmEvent`s (tokens and tool calls).
    /// The implementation spawns a background task to read the vendor's
    /// stream and forward events. Dropping the `Receiver` cancels it.
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[serde_json::Value]>,
        config: &LlmCallConfig,
    ) -> Result<mpsc::Receiver<LlmEvent>, LlmProviderError>;
}

/// Helper to collect all text tokens from a streaming LLM completion into a single string.
/// Useful for background micro-tasks like summarization that do not stream to the user.
pub async fn collect_text(
    provider: &dyn LlmProvider,
    messages: &[ChatMessage],
    config: &LlmCallConfig,
) -> Result<String, LlmProviderError> {
    let mut rx = provider.stream_completion(messages, None, config).await?;
    let mut text = String::new();
    while let Some(event) = rx.recv().await {
        if let LlmEvent::Token(t) = event {
            text.push_str(&t);
        }
    }
    Ok(text)
}

pub mod config;
pub mod fallback;
pub(crate) mod rig_streaming;
pub mod openai;
pub mod openai_compat;
pub mod anthropic;
pub mod gemini;
pub mod deepseek;
pub mod groq;
pub mod together;
pub mod fireworks;
pub mod realtime;
pub mod gemini_live;
