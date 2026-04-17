//! Agent Kit — conversation management, LLM streaming, and autonomous tool execution.
//!
//! The backend drives the full agentic loop (LLM ↔ tools) internally,
//! emitting lifecycle events via [`AgentBackend::recv`].
//! 
//! Tools can be natively implemented system tools (e.g., hang_up) or dynamically 
//! defined as QuickJS scripts inside an `AgentGraphDef`.

pub mod agent_backends;
pub mod artifact_store;
pub(crate) mod context_summarizer;
pub mod micro_tasks;
pub mod tool_executor;
pub mod providers;
pub mod quickjs_engine;
pub mod swarm;


// Re-exports for convenience
pub use agent_backends::{
    default::DefaultAgentBackend, is_escalate_call_tool, is_hang_up_tool, is_transfer_tool,
    AfterToolCallAction, AgentBackend, AgentBackendConfig, AgentCommand, AgentEvent, AgentOutput,
    BeforeToolCallAction, ChatMessage, LlmCommand, LlmOutput, SecretMap, SharedSecretMap,
    ToolInterceptor,
};

pub use artifact_store::{ArtifactInterceptor, ArtifactStore};
pub use providers::{
    LlmCallConfig, LlmEvent, LlmProvider, LlmProviderError, ToolCallEvent,
};
pub use providers::config::{NamedProviderConfig, ProviderConfig};
pub use providers::fallback::FallbackProvider;
pub use providers::{
    anthropic::AnthropicProvider, deepseek::DeepSeekProvider, fireworks::FireworksProvider,
    gemini::GeminiProvider, groq::GroqProvider, openai::OpenAiProvider,
    openai_compat::OpenAiCompatProvider, together::TogetherProvider,
};

pub use quickjs_engine::{QuickJsToolEngine as ScriptEngine, ToolError};
pub use swarm::{AgentGraphDef, AudioFormat, AudioLayout, NodeDef, RecordingConfig, ToolDef};
