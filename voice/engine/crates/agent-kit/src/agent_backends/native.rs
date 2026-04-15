//! Native multimodal agent backend — wraps a `RealtimeProvider` (e.g. Gemini Live).
//!
//! Unlike `DefaultAgentBackend` (which runs a text-based STT → LLM → TTS pipeline),
//! `NativeMultimodalBackend` streams raw PCM audio directly between the user's
//! microphone/speaker and the model. There is no STT or TTS stage.
//!
//! # Pipeline
//!
//! ```text
//! User mic (WebRTC PCM)
//!         │
//!         ▼
//! push_audio(pcm) ────────────────➤ RealtimeProvider (Gemini Live WebSocket)
//!                                           │
//!         ┌─────────────────────────────────┘
//!         │ recv_event() → RealtimeEvent
//!         ▼
//!  ┌──────────────────────────┐
//!  │ BotAudioChunk(pcm)       │──→ AgentEvent::BotAudio  (direct to WebRTC out)
//!  │ ToolCall { .. }          │──→ Execute via HttpExecutor → send_tool_result()
//!  │ InputTranscription       │──→ AgentEvent::InputTranscript  (for UI / logs)
//!  │ OutputTranscription      │──→ AgentEvent::OutputTranscript (for UI / logs)
//!  │ TurnComplete             │──→ AgentEvent::Finished
//!  │ Error                    │──→ AgentEvent::Error
//!  └──────────────────────────┘
//! ```
//!
//! # Context management
//!
//! Gemini Live maintains conversation context server-side inside the WebSocket
//! session.  The backend stores the `session_resumption_handle` returned by the
//! provider and passes it when reconnecting after a drop, transparently restoring
//! context without re-uploading any history.
//!
//! # VAD
//!
//! Call `push_audio` for every raw PCM frame from WebRTC — audio is forwarded
//! continuously.  If you are using local (client-side) VAD, also call
//! `signal_vad` so the provider can send `ActivityStart`/`ActivityEnd` markers
//! instead of relying on Gemini's server-side VAD.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent_backends::{AgentBackend, AgentBackendConfig, AgentEvent};
use crate::providers::realtime::{RealtimeConfig, RealtimeEvent, RealtimeProvider, VadState};
use crate::providers::LlmProviderError;
use crate::swarm::{build_node_tool_schemas, AgentGraphDef};
use crate::ScriptEngine;

// ── AgentEvent extensions for native audio ──────────────────────────────────
//
// The existing `AgentEvent` enum does not have a `BotAudio` or transcript
// variant. We add them via a new `NativeAgentEvent` wrapper that the reactor
// should match against when using a native backend.

/// Events emitted by `NativeMultimodalBackend` during a live session.
///
/// Extends the common `AgentEvent` vocabulary with audio-specific variants.
#[derive(Debug, Clone)]
pub enum NativeAgentEvent {
    /// Raw PCM audio from the model (→ send directly to WebRTC playback).
    BotAudio(Vec<i16>),

    /// Transcription of what the user said (from the model's recognition stream).
    InputTranscript { text: String, is_final: bool },

    /// Transcription of what the model is saying (from the model's TTS stream).
    OutputTranscript { text: String, is_final: bool },

    /// A tool has started executing.
    ToolCallStarted { id: String, name: String },

    /// A tool has finished.
    ToolCallCompleted { id: String, name: String, success: bool },

    /// The agent called hang_up — session should end gracefully.
    HangUp { reason: String },

    /// The model finished speaking (turn boundary).
    TurnComplete {
        /// Prompt token count reported by the provider (0 = not available).
        prompt_tokens: u32,
        /// Completion token count reported by the provider (0 = not available).
        completion_tokens: u32,
    },

    /// A non-fatal model-level error (e.g. content filtered, invalid tool call).
    /// Transport/WebSocket failures are signalled by returning `None` from `recv()`
    /// so the session layer can trigger a clean reconnect.
    Error(String),
}

// ── Internal tool result ─────────────────────────────────────────────────────

struct ToolResult {
    call_id: String,
    tool_name: String,
    result: Value,
    success: bool,
}

// ── NativeMultimodalBackend ──────────────────────────────────────────────────

/// Agent backend that wraps a persistent `RealtimeProvider` (e.g. Gemini Live).
///
/// Drives native audio-to-audio turns: push PCM in, receive PCM + tool events
/// out. Tool results are executed via the standard `HttpExecutor` / QuickJS
/// pipeline and the result is sent back up the WebSocket to resume the turn.
pub struct NativeMultimodalBackend {
    provider: Box<dyn RealtimeProvider>,
    script_engine: Option<Arc<ScriptEngine>>,
    /// Base generation config (used for future token-budget enforcement).
    #[allow(dead_code)]
    config: AgentBackendConfig,
    system_prompt: Option<String>,
    voice_id: String,

    // Tool execution channel.
    tool_result_tx: mpsc::UnboundedSender<ToolResult>,
    tool_result_rx: mpsc::UnboundedReceiver<ToolResult>,

    // Resolved tool schemas for the current session.
    tool_schemas: Vec<Value>,

    // Whether the session WebSocket is open.
    connected: bool,

    // Cached resumption handle from the last successful session.
    last_resumption_handle: Option<String>,
}

impl NativeMultimodalBackend {
    /// Create a new backend.
    ///
    /// * `provider` — a `RealtimeProvider` impl (e.g. `GeminiLiveProvider`).
    /// * `agent_graph` — optional agent graph for tool schema resolution.
    /// * `config` — base generation config (temperature, max_tokens, secrets).
    /// * `voice_id` — provider-specific voice name (e.g. `"Charon"` for Gemini).
    pub fn new(
        provider: Box<dyn RealtimeProvider>,
        agent_graph: Option<&AgentGraphDef>,
        config: AgentBackendConfig,
        voice_id: String,
    ) -> Self {
        let script_engine = agent_graph.and_then(|g| {
            if g.tools.is_empty() {
                None
            } else {
                Some(Arc::new(ScriptEngine::with_tools_and_sandbox(
                    g.tools.clone(),
                    std::env::temp_dir(),
                    config.secrets.clone(),
                )))
            }
        });

        // Build tool schemas from the entry node (Gemini Live gets all tools
        // for the entire session upfront — we use the entry node's tool set).
        let tool_schemas: Vec<Value> = agent_graph
            .and_then(|g| {
                g.nodes.get(&g.entry)
                    .map(|node| build_node_tool_schemas(node, &g.tools))
            })
            .unwrap_or_default();

        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            provider,
            script_engine,
            config,
            system_prompt: None,
            voice_id,
            tool_result_tx: tx,
            tool_result_rx: rx,
            tool_schemas,
            connected: false,
            last_resumption_handle: None,
        }
    }

    /// Open (or re-open) the WebSocket connection to the realtime backend.
    ///
    /// Passes the last saved `session_resumption_handle` so Gemini can restore
    /// the prior session state transparently without re-uploading context.
    pub async fn connect(&mut self) -> Result<(), LlmProviderError> {
        let rc = RealtimeConfig {
            system_instruction: self.system_prompt.clone(),
            voice_id: self.voice_id.clone(),
            tools: if self.tool_schemas.is_empty() {
                None
            } else {
                Some(self.tool_schemas.clone())
            },
            session_resumption_handle: self.last_resumption_handle.clone(),
        };

        self.provider.connect(&rc).await?;
        self.connected = true;
        Ok(())
    }

    /// Push a raw PCM audio frame from the user's microphone into the model.
    ///
    /// Call this for every audio frame that arrives from the WebRTC track,
    /// regardless of VAD state. The model (or the server-side VAD) will decide
    /// when to respond.
    pub async fn push_audio(&mut self, pcm: &[i16]) -> Result<(), LlmProviderError> {
        self.provider.push_user_audio(pcm).await
    }

    /// Optionally signal client-side VAD state to the model.
    ///
    /// Only required when you have disabled Gemini's server-side VAD and are
    /// using your own VAD (e.g. Silero). When the local VAD flips to
    /// `SpeechStarted`, call `signal_vad(VadState::Started)` to trigger
    /// `ActivityStart` on the wire (both interruptions and turn starts).
    pub async fn signal_vad(&mut self, state: VadState) -> Result<(), LlmProviderError> {
        self.provider.trigger_vad(state).await
    }

    /// Interrupt the model mid-response (barge-in).
    ///
    /// Sends `ActivityStart` to Gemini, which instructs the server to stop
    /// emitting audio immediately and start listening again.
    pub async fn interrupt(&mut self) -> Result<(), LlmProviderError> {
        self.provider.interrupt().await
    }

    /// Poll for the next event from the model.
    ///
    /// Returns `None` immediately if neither the model nor any in-flight tool
    /// has produced a new event. Calling code should either `await` this
    /// continuously inside a select! loop, or use `recv_event_timeout` if
    /// mixed with other futures.
    ///
    /// On `NativeAgentEvent::ToolCall`, execution begins automatically and
    /// the result is sent back to the model via `send_tool_result`. The event
    /// is surfaced as `ToolCallStarted` → execution → `ToolCallCompleted`.
    pub async fn recv(&mut self) -> Option<NativeAgentEvent> {
        tokio::select! {
            // ── Tool results from background executor ───────────────
            Some(result) = self.tool_result_rx.recv() => {
                // Forward result to the model.
                let result_val = result.result.clone();
                if let Err(e) = self.provider.send_tool_result(&result.call_id, &result.tool_name, result_val).await {
                    warn!("[native-backend] Failed to send tool result: {}", e);
                }
                // Update last resumption handle.
                if let Some(h) = self.provider.session_resumption_handle() {
                    self.last_resumption_handle = Some(h);
                }
                return Some(NativeAgentEvent::ToolCallCompleted {
                    id: result.call_id,
                    name: result.tool_name,
                    success: result.success,
                });
            }

            // ── Events from the model ────────────────────────────────
            event = self.provider.recv_event() => {
                // Update resumption handle opportunistically on every recv.
                if let Some(h) = self.provider.session_resumption_handle() {
                    self.last_resumption_handle = Some(h);
                }
                match event {
                    Err(e) => {
                        // Transport error — the WebSocket is dead. Return None so the
                        // session.rs event loop hits its `None => reconnect` branch
                        // instead of spinning at 100% CPU re-polling a closed socket.
                        warn!("[native-backend] Provider transport error: {}", e);
                        self.connected = false;
                        return None;
                    }
                    Ok(e) => self.map_event(e).await,
                }
            }
        }
    }

    /// Map a `RealtimeEvent` into a `NativeAgentEvent`, launching tool execution
    /// as a side-effect when a ToolCall is received.
    async fn map_event(&mut self, event: RealtimeEvent) -> Option<NativeAgentEvent> {
        match event {
            RealtimeEvent::BotAudioChunk(samples) => {
                Some(NativeAgentEvent::BotAudio(samples))
            }

            RealtimeEvent::InputTranscription { text, is_final } => {
                if is_final {
                    info!("[native-backend] User said: {:?}", text);
                }
                Some(NativeAgentEvent::InputTranscript { text, is_final })
            }

            RealtimeEvent::OutputTranscription { text, is_final } => {
                Some(NativeAgentEvent::OutputTranscript { text, is_final })
            }

            RealtimeEvent::TurnComplete { prompt_tokens, completion_tokens } => {
                info!("[native-backend] Turn complete (prompt={}, completion={})", prompt_tokens, completion_tokens);
                Some(NativeAgentEvent::TurnComplete { prompt_tokens, completion_tokens })
            }

            RealtimeEvent::Error(msg) => {
                warn!("[native-backend] Model error: {}", msg);
                Some(NativeAgentEvent::Error(msg))
            }

            RealtimeEvent::ToolCall { call_id, name, arguments } => {
                info!("[native-backend] ToolCall: {} (id={})", name, call_id);

                // Intercept synthetic runtime tools before dispatching to the script engine.
                // This mirrors the pattern in DefaultAgentBackend::handle_streaming_phase.
                if name == crate::swarm::HANG_UP_TOOL_NAME {
                    let reason = serde_json::from_str::<serde_json::Value>(&arguments)
                        .ok()
                        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
                        .unwrap_or_else(|| "agent_initiated".to_string());
                    info!("[native-backend] hang_up intercepted (reason={})", reason);
                    // Send a synthetic success result back to the model so it doesn't retry.
                    let _ = self.provider.send_tool_result(
                        &call_id,
                        &name,
                        serde_json::json!({"result": "Hang up initiated."}),
                    ).await;
                    return Some(NativeAgentEvent::HangUp { reason });
                }

                self.dispatch_tool(call_id, name, arguments).await
            }
        }
    }

    /// Dispatch a tool call to the ScriptEngine and return the `ToolCallStarted` event.
    ///
    /// The tool runs in a blocking Tokio thread. Completion arrives asynchronously
    /// through `tool_result_rx` in the next `recv()` call.
    async fn dispatch_tool(
        &mut self,
        call_id: String,
        name: String,
        arguments: String,
    ) -> Option<NativeAgentEvent> {
        let tx = self.tool_result_tx.clone();
        let call_id_clone = call_id.clone();
        let name_clone = name.clone();
        let engine = self.script_engine.clone();

        tokio::spawn(async move {
            let (result_val, success) = if let Some(engine) = engine
                .as_ref()
                .filter(|e| e.get(&name_clone).is_some())
            {
                let engine_clone = Arc::clone(engine);
                let tn = name_clone.clone();
                let ag = arguments.clone();
                match tokio::task::spawn_blocking(move || engine_clone.execute(&tn, &ag)).await {
                    Ok(Ok(r)) => {
                        let parsed = serde_json::from_str(&r)
                            .unwrap_or_else(|_| serde_json::json!({"result": r}));

                        let mut is_success = true;
                        if let Some(err_val) = parsed.get("error") {
                            if !matches!(err_val, serde_json::Value::Null | serde_json::Value::Bool(false)) {
                                is_success = false;
                            }
                        }

                        (parsed, is_success)
                    }
                    Ok(Err(e)) => (serde_json::json!({"error": format!("Tool error: {e}")}), false),
                    Err(e) => (serde_json::json!({"error": format!("Tool panicked: {e}")}), false),
                }
            } else {
                (
                    serde_json::json!({ "error": format!("Tool '{}' is not defined in the agent graph.", name_clone) }),
                    false,
                )
            };

            let _ = tx.send(ToolResult {
                call_id: call_id_clone,
                tool_name: name_clone,
                result: result_val,
                success,
            });
        });

        Some(NativeAgentEvent::ToolCallStarted {
            id: call_id,
            name,
        })
    }
}

// ── Stub AgentBackend impl ───────────────────────────────────────────────────
//
// `NativeMultimodalBackend` doesn't use the `AgentBackend` trait naturally
// because it is audio-driven rather than text-driven. We provide a no-op impl
// so the existing factory/config code that returns `Box<dyn AgentBackend>` can
// still compile while we refactor the Reactor to optionally accept a native backend.

#[async_trait]
impl AgentBackend for NativeMultimodalBackend {
    fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = Some(prompt);
    }

    fn add_user_message(&mut self, _text: String) {
        // Not used — context is managed server-side in the WebSocket session.
    }

    fn add_assistant_message(&mut self, _text: String) {
        // Not used — context is managed server-side in the WebSocket session.
    }

    async fn start_turn(&mut self) -> Result<(), LlmProviderError> {
        // Ensure connected.
        if !self.connected {
            self.connect().await?;
        }
        // In native multimodal mode, the "turn" begins when audio starts flowing.
        // No additional setup needed here.
        Ok(())
    }

    async fn recv(&mut self) -> Option<AgentEvent> {
        // Delegate to our native recv() and wrap the events.
        let native_event = NativeMultimodalBackend::recv(self).await?;
        // Translate native events to the generic AgentEvent interface.
        // Events that have no AgentEvent equivalent are returned as None.
        Some(match native_event {
            NativeAgentEvent::TurnComplete { .. } => AgentEvent::Finished { content: None },
            NativeAgentEvent::Error(e) => AgentEvent::Error(e),
            NativeAgentEvent::ToolCallStarted { id, name } => AgentEvent::ToolCallStarted {
                id,
                name,
                side_effect: true,
            },
            NativeAgentEvent::ToolCallCompleted { id, name, success } => {
                AgentEvent::ToolCallCompleted {
                    id,
                    name,
                    success,
                    error_message: None,
                }
            }
            NativeAgentEvent::HangUp { reason } => AgentEvent::HangUp {
                reason,
                // content is None here because the stub AgentBackend path does not
                // have access to bot_transcript_buf. The run_native_multimodal loop
                // in session.rs flushes bot_transcript_buf directly when it handles
                // NativeAgentEvent::HangUp — callers using the native recv() path
                // directly (the normal production path) get full transcript coverage.
                content: None,
            },

            // BotAudio, InputTranscript, OutputTranscript have no AgentEvent equivalent yet.
            // Consumers that need them should use `NativeMultimodalBackend::recv()` directly.
            _ => return None,
        })
    }

    fn cancel(&mut self) -> Option<AgentEvent> {
        // Fire-and-forget interrupt (we cannot await here per the trait signature).
        // The actual interrupt is sent next time the event loop polls push_audio / recv.
        None
    }

    fn handle_transfer(&mut self, _target_agent: &str) -> bool {
        false // Swarm transfer is not supported in native multimodal mode.
    }

    fn is_active(&self) -> bool {
        self.connected
    }
}
