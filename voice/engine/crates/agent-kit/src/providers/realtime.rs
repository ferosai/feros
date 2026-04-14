use async_trait::async_trait;
use serde_json::Value;

use super::LlmProviderError;

/// State of the Voice Activity Detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    /// The user has started speaking.
    Started,
    /// The user has stopped speaking.
    Stopped,
}

/// An event yielded by the realtime multimodal backend.
#[derive(Debug)]
pub enum RealtimeEvent {
    /// Raw PCM audio chunk (usually 16kHz or 24kHz depending on configuration) emitted by the model.
    BotAudioChunk(Vec<i16>),
    
    /// The model requested a tool invocation over the WebSocket.
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    
    /// Optional: The transcript of what the user said (from the model's STT).
    InputTranscription { text: String, is_final: bool },
    
    /// Optional: The transcript of what the model is saying.
    OutputTranscription { text: String, is_final: bool },
    
    /// A turn has finished fully (no more audio or tools).
    TurnComplete {
        /// Prompt token count from Gemini `usage_metadata` (0 if not provided).
        prompt_tokens: u32,
        /// Completion token count from Gemini `usage_metadata` (0 if not provided).
        completion_tokens: u32,
    },
    
    Error(String),
}

/// A configuration object for realtime providers (e.g. Gemini Live, OpenAI Realtime).
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    pub system_instruction: Option<String>,
    pub voice_id: String,
    pub tools: Option<Vec<Value>>,
    /// For providers that support connection resumption.
    pub session_resumption_handle: Option<String>,
}

/// A pluggable multmodal realtime backend.
///
/// Unlike `LlmProvider` which takes an array of text messages and returns a text stream,
/// a `RealtimeProvider` maintains a persistent WebSocket connection through which
/// raw audio is streamed bidirectionally.
#[async_trait]
pub trait RealtimeProvider: Send + Sync {
    /// Reconnect or open the persistent connection.
    /// Handles like `session_resumption_handle` can be provided in `config`.
    async fn connect(&mut self, config: &RealtimeConfig) -> Result<(), LlmProviderError>;

    /// Gets the current session resumption handle, if the provider supports it.
    fn session_resumption_handle(&self) -> Option<String> {
        None
    }

    /// Push raw PCM audio to the model (typically from WebRTC).
    /// Providers assume a specific sample rate, usually defined per-provider or via config.
    async fn push_user_audio(&mut self, pcm_data: &[i16]) -> Result<(), LlmProviderError>;
    
    /// Ping the model that local VAD has triggered (if we are using client VAD).
    async fn trigger_vad(&mut self, state: VadState) -> Result<(), LlmProviderError>;
    
    /// Wait for the next downstream event (audio chunk, tool call, text).
    async fn recv_event(&mut self) -> Result<RealtimeEvent, LlmProviderError>;
    
    /// Submit the result of a tool back up the WebSocket.
    async fn send_tool_result(&mut self, call_id: &str, tool_name: &str, result: Value) -> Result<(), LlmProviderError>;

    /// Force a disruption/cut-off of the current bot generation (barge-in).
    async fn interrupt(&mut self) -> Result<(), LlmProviderError>;
}
