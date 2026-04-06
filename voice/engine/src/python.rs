//! Python bindings for the voice-engine library.
//!
//! Exposes `VoiceServer`, `AgentRunner`, and `SessionConfig` to Python via the `voice_engine`
//! native module. The server runs its own WebSocket endpoint — Python's only
//! role is to authenticate, fetch agent config from the DB, and register
//! sessions. Audio and events flow directly between the browser and Rust.
//!
//! Also exposes `AgentRunner` — a headless agent backend for testing.
//! Python test code can pass in hook callables to intercept tool calls.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tracing::info;

use agent_kit::agent_backends::SecretMap;
use agent_kit::agent_backends::{
    AfterToolCallAction, AgentEvent, BeforeToolCallAction, ToolInterceptor,
};
use agent_kit::artifact_store::{ArtifactInterceptor, ArtifactStore};
use agent_kit::swarm::AgentGraphDef;
use agent_kit::{
    AgentBackend, AgentBackendConfig, AnthropicProvider, DeepSeekProvider, DefaultAgentBackend,
    GeminiProvider, GroqProvider, OpenAiCompatProvider, OpenAiProvider,
};

use crate::language_config::{
    stt_model_supports_language as _stt_model_supports_language,
    tts_model_supports_language as _tts_model_supports_language, ELEVENLABS_MULTILINGUAL_MODELS,
    STT_MODEL_CATALOG, SUPPORTED_LANGUAGES, TTS_MODEL_CATALOG,
};
use crate::server::{ProviderConfig, RegisteredSession, ServerState, TelephonyCredentials};
use crate::session::SessionConfig;

// ── Python-exposed SessionConfig ────────────────────────────────

#[pyclass(name = "SessionConfig")]
#[derive(Clone)]
pub struct PySessionConfig {
    inner: SessionConfig,
}

#[pymethods]
impl PySessionConfig {
    #[new]
    #[pyo3(signature = (
        agent_id = String::new(),
        temperature = 0.7,
        max_tokens = 32768,
        input_sample_rate = 48000,
        output_sample_rate = 24000,
        models_dir = String::from("./dsp_models"),
        smart_turn_threshold = 0.5,
        denoise_enabled = true,
        denoise_backend = String::from("rnnoise"),
        smart_turn_enabled = true,
        turn_completion_enabled = true,
        idle_timeout_secs = 5,
        idle_max_nudges = 2,
        min_barge_in_words = 2,
        barge_in_timeout_ms = 800,
        graph_json = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        agent_id: String,
        temperature: f64,
        max_tokens: u32,
        input_sample_rate: u32,
        output_sample_rate: u32,
        models_dir: String,
        smart_turn_threshold: f32,
        denoise_enabled: bool,
        denoise_backend: String,
        smart_turn_enabled: bool,
        turn_completion_enabled: bool,
        idle_timeout_secs: u32,
        idle_max_nudges: u32,
        min_barge_in_words: u32,
        barge_in_timeout_ms: u32,
        graph_json: Option<String>,
    ) -> PyResult<Self> {
        // Parse v3 agent graph JSON (single AgentGraphDef) if provided
        let agent_graph: Option<AgentGraphDef> = match graph_json {
            Some(json_str) => {
                let graph: AgentGraphDef = serde_json::from_str(&json_str)
                    .map_err(|e| PyRuntimeError::new_err(format!("Invalid graph JSON: {}", e)))?;
                Some(graph)
            }
            None => None,
        };

        Ok(Self {
            inner: {
                let mut cfg = SessionConfig {
                    agent_id,
                    temperature,
                    max_tokens,
                    input_sample_rate,
                    output_sample_rate,
                    models_dir,
                    smart_turn_threshold,
                    denoise_enabled,
                    denoise_backend: match denoise_backend.as_str() {
                        "dfn3" => crate::audio_ml::denoiser::DenoiserBackend::DeepFilterNet3,
                        "rnnoise" => crate::audio_ml::denoiser::DenoiserBackend::RNNoise,
                        "dtln" => crate::audio_ml::denoiser::DenoiserBackend::Dtln,
                        _ => crate::audio_ml::denoiser::DenoiserBackend::default(), // RNNoise
                    },
                    smart_turn_enabled,
                    turn_completion_enabled,
                    idle_timeout_secs,
                    idle_max_nudges,
                    min_barge_in_words,
                    barge_in_timeout_ms,
                    agent_graph: agent_graph.clone(),
                    // Defaults — overridden from graph below
                    ..Default::default()
                };

                // Apply all settings from graph (graph is source of truth for v3_graph)
                if let Some(ref graph) = agent_graph {
                    // Top-level agent-wide settings
                    if let Some(ref lang) = graph.language {
                        if !lang.is_empty() {
                            cfg.language = lang.clone();
                        }
                    }
                    if let Some(ref vid) = graph.voice_id {
                        if !vid.is_empty() {
                            cfg.voice_id = vid.clone();
                        }
                    }

                    // Native Multimodal Override now in config.finalize()

                    // Recording configuration
                    if let Some(ref recording) = graph.recording {
                        cfg.recording = map_recording_config(recording);
                    }

                    // Entry node settings
                    if let Some(entry_node) = graph.nodes.get(&graph.entry) {
                        cfg.system_prompt = entry_node.system_prompt.clone();
                        if let Some(ref g) = entry_node.greeting {
                            cfg.greeting = Some(g.clone());
                        }
                        // Node-level voice_id overrides global
                        if let Some(ref vid) = entry_node.voice_id {
                            if !vid.is_empty() {
                                cfg.voice_id = vid.clone();
                            }
                        }
                    }
                }

                cfg.finalize()
            },
        })
    }

    #[getter]
    fn agent_id(&self) -> &str {
        &self.inner.agent_id
    }
}

// ── Python-exposed ServerConfig ─────────────────────────────────

/// Configuration object for starting the voice server.
///
/// Groups all server-level settings (bind address, provider URLs,
/// telephony credentials) into a single object instead of N positional args.
#[pyclass(name = "ServerConfig")]
#[derive(Clone)]
pub struct PyServerConfig {
    pub host: String,
    pub port: u16,
    pub default_stt_url: String,
    pub default_stt_provider: String,
    pub default_stt_model: String,
    pub default_stt_api_key: String,
    pub default_llm_url: String,
    pub default_llm_api_key: String,
    pub default_llm_model: String,
    pub default_llm_provider: String,
    pub default_tts_url: String,
    pub default_tts_provider: String,
    pub default_tts_model: String,
    pub default_tts_api_key: String,
    pub default_twilio_account_sid: String,
    pub default_twilio_auth_token: String,
    pub default_telnyx_api_key: String,
    /// Shared secret for HMAC-SHA256 session token validation.
    /// When set, all endpoints require a valid token = HMAC(secret, session_id).
    /// When empty, token validation is skipped (dev mode).
    pub auth_secret_key: String,
}

#[pymethods]
impl PyServerConfig {
    #[new]
    #[pyo3(signature = (
        host = String::from("0.0.0.0"),
        port = 8300,
        default_stt_url = String::from("http://localhost:8100"),
        default_stt_provider = String::new(),
        default_stt_model = String::new(),
        default_stt_api_key = String::new(),
        default_llm_url = String::from("http://localhost:11434/v1"),
        default_llm_api_key = String::new(),
        default_llm_model = String::from("llama3.2"),
        default_llm_provider = String::new(),
        default_tts_url = String::from("http://localhost:8200"),
        default_tts_provider = String::new(),
        default_tts_model = String::new(),
        default_tts_api_key = String::new(),
        default_twilio_account_sid = String::new(),
        default_twilio_auth_token = String::new(),
        default_telnyx_api_key = String::new(),
        auth_secret_key = String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        host: String,
        port: u16,
        default_stt_url: String,
        default_stt_provider: String,
        default_stt_model: String,
        default_stt_api_key: String,
        default_llm_url: String,
        default_llm_api_key: String,
        default_llm_model: String,
        default_llm_provider: String,
        default_tts_url: String,
        default_tts_provider: String,
        default_tts_model: String,
        default_tts_api_key: String,
        default_twilio_account_sid: String,
        default_twilio_auth_token: String,
        default_telnyx_api_key: String,
        auth_secret_key: String,
    ) -> Self {
        Self {
            host,
            port,
            default_stt_url,
            default_stt_provider,
            default_stt_model,
            default_stt_api_key,
            default_llm_url,
            default_llm_api_key,
            default_llm_model,
            default_llm_provider,
            default_tts_url,
            default_tts_provider,
            default_tts_model,
            default_tts_api_key,
            default_twilio_account_sid,
            default_twilio_auth_token,
            default_telnyx_api_key,
            auth_secret_key,
        }
    }
}

// ── Python-exposed VoiceServer ──────────────────────────────────

#[pyclass(name = "VoiceServer")]
pub struct PyVoiceServer {
    state: ServerState,
    #[allow(dead_code)] // kept alive to hold the tokio runtime
    runtime: Arc<tokio::runtime::Runtime>,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    port: u16,
}

#[pymethods]
impl PyVoiceServer {
    /// Start the voice WebSocket server.
    ///
    /// Args:
    ///     config: ServerConfig with all server-level settings.
    ///
    /// Returns:
    ///     VoiceServer instance with the WS server running in the background.
    #[staticmethod]
    fn start(config: &PyServerConfig) -> PyResult<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("voice-engine")
                .build()
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?,
        );

        // Load .env if present (must happen before ServerState::new reads env)
        let _ = dotenvy::dotenv();

        // Build server state (handles rustls and ICE init internally)
        let providers = ProviderConfig {
            stt_url: config.default_stt_url.clone(),
            stt_provider: config.default_stt_provider.clone(),
            stt_model: config.default_stt_model.clone(),
            stt_api_key: config.default_stt_api_key.clone(),
            llm_url: config.default_llm_url.clone(),
            llm_api_key: config.default_llm_api_key.clone(),
            llm_model: config.default_llm_model.clone(),
            llm_provider: config.default_llm_provider.clone(),
            tts_url: config.default_tts_url.clone(),
            tts_provider: config.default_tts_provider.clone(),
            tts_model: config.default_tts_model.clone(),
            tts_api_key: config.default_tts_api_key.clone(),
            tts_voice_id: String::new(), // populated per-session from SessionConfig.voice_id
        };
        let telephony = TelephonyCredentials {
            twilio_account_sid: config.default_twilio_account_sid.clone(),
            twilio_auth_token: config.default_twilio_auth_token.clone(),
            telnyx_api_key: config.default_telnyx_api_key.clone(),
        };
        let state = ServerState::new(providers, telephony, config.auth_secret_key.clone());

        let addr: SocketAddr = format!("{}:{}", config.host, config.port)
            .parse()
            .map_err(|e| PyRuntimeError::new_err(format!("Invalid address: {}", e)))?;

        let state_clone = state.clone();
        let server_handle = runtime.spawn(async move {
            crate::server::run_server(addr, state_clone).await;
        });

        info!("VoiceServer started on {}:{}", config.host, config.port);

        Ok(Self {
            state,
            runtime,
            server_handle: Some(server_handle),
            port: config.port,
        })
    }

    /// Register a session config for a given session ID.
    ///
    /// After calling this, the client can connect to:
    ///     ws://host:{port}/ws/voice/{session_id}
    ///
    /// The config is consumed (removed from the map) once the client connects.
    #[pyo3(signature = (session_id, config, stt_url, llm_url, llm_api_key, llm_model, tts_url, llm_provider="", stt_provider="", tts_provider="", stt_model="", tts_model="", stt_api_key="", tts_api_key=""))]
    #[allow(clippy::too_many_arguments)]
    fn register_session(
        &self,
        session_id: &str,
        config: &PySessionConfig,
        stt_url: &str,
        llm_url: &str,
        llm_api_key: &str,
        llm_model: &str,
        tts_url: &str,
        llm_provider: &str,
        stt_provider: &str,
        tts_provider: &str,
        stt_model: &str,
        tts_model: &str,
        stt_api_key: &str,
        tts_api_key: &str,
    ) -> PyResult<()> {
        let mut session_config = config.inner.clone();

        // Apply per-provider STT p99 latency, mirroring run_session_with_transport.
        if !stt_provider.is_empty() {
            session_config.stt_provider = stt_provider.to_string();
            session_config.stt_p99_latency_ms =
                crate::providers::stt::default_stt_p99_latency_ms(stt_provider);
        }

        let tts_voice_id = session_config.voice_id.clone();
        self.state.sessions.insert(
            session_id.to_string(),
            RegisteredSession {
                config: session_config,
                providers: ProviderConfig {
                    stt_url: stt_url.to_string(),
                    stt_provider: stt_provider.to_string(),
                    stt_model: stt_model.to_string(),
                    stt_api_key: stt_api_key.to_string(),
                    llm_url: llm_url.to_string(),
                    llm_api_key: llm_api_key.to_string(),
                    llm_model: llm_model.to_string(),
                    llm_provider: llm_provider.to_string(),
                    tts_url: tts_url.to_string(),
                    tts_provider: tts_provider.to_string(),
                    tts_model: tts_model.to_string(),
                    tts_api_key: tts_api_key.to_string(),
                    tts_voice_id,
                },
                secrets: None,
                secret_refresh_handle: None,
                created_at: std::time::Instant::now(),
                tracer: None,
                telephony_creds: None,
            },
        );
        info!(
            "Registered session: {} (llm={}, stt={}, tts={})",
            session_id, llm_provider, stt_provider, tts_provider
        );
        Ok(())
    }

    /// Return the port the server is running on.
    #[getter]
    fn port(&self) -> u16 {
        self.port
    }

    /// Return number of pending (unconsumed) registered sessions.
    fn pending_sessions(&self) -> usize {
        self.state.sessions.len()
    }

    /// Stop the server.
    fn stop(&mut self) -> PyResult<()> {
        if let Some(handle) = self.server_handle.take() {
            handle.abort();
            info!("VoiceServer stopped");
        }
        Ok(())
    }
}

impl Drop for PyVoiceServer {
    fn drop(&mut self) {
        if let Some(handle) = self.server_handle.take() {
            handle.abort();
        }
    }
}

// ── Script validation ─────────────────────────────────────────

/// Validate a JavaScript tool script using QuickJS parser (syntax-only, no execution).
#[pyfunction]
fn validate_javascript(script: &str) -> Vec<String> {
    use rquickjs::{Context, Runtime};
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    let result = ctx.with(|ctx: rquickjs::Ctx| {
        // Evaluate as a module with COMPILE_ONLY. This parses the syntax safely and strictly,
        // preventing any execution, infinite loops, or template injection breakouts.
        // We wrap it in a function body so tool scripts with `return` are syntactically valid.
        let wrapped = format!("function __validate__() {{\n{}\n}}", script);
        match rquickjs::Module::declare(ctx, "validate", wrapped) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("JS Syntax Error: {}", e)),
        }
    });
    match result {
        Ok(()) => vec![],
        Err(msg) => vec![msg],
    }
}

// ── Tool Call Hook (Python → Rust bridge) ───────────────────────

/// Wraps Python callables as a `ToolInterceptor`.
///
/// - `before_fn(name: str, args: str) -> str | None`
///   Return a string to stub the tool, or `None` to proceed normally.
/// - `after_fn(name: str, args: str, result: str) -> str | None`
///   Return a string to override the result, or `None` to pass through.
struct PyToolInterceptor {
    before_fn: Option<PyObject>,
    after_fn: Option<PyObject>,
}

fn build_runner_llm_provider(
    llm_provider: &str,
    llm_api_key: &str,
    llm_url: &str,
    llm_model: &str,
) -> PyResult<Box<dyn agent_kit::LlmProvider>> {
    let base_url = Some(llm_url);

    macro_rules! try_provider {
        ($label:expr, $ctor:expr) => {
            match $ctor {
                Ok(provider) => return Ok(Box::new(provider)),
                Err(err) => {
                    tracing::warn!(
                        "[AgentRunner] Failed to create {} provider, falling back to OpenAI-compat: {}",
                        $label,
                        err
                    );
                }
            }
        };
    }

    match llm_provider {
        "groq" => try_provider!("Groq", GroqProvider::new(llm_api_key, llm_model)),
        "openai" => try_provider!(
            "OpenAI",
            OpenAiProvider::new(llm_api_key, base_url, llm_model)
        ),
        "anthropic" => try_provider!(
            "Anthropic",
            AnthropicProvider::new(llm_api_key, base_url, llm_model)
        ),
        "deepseek" => try_provider!(
            "DeepSeek",
            DeepSeekProvider::new(llm_api_key, base_url, llm_model)
        ),
        "gemini" => try_provider!(
            "Gemini",
            GeminiProvider::new(llm_api_key, base_url, llm_model)
        ),
        _ => {}
    }

    Ok(Box::new(
        OpenAiCompatProvider::new(llm_api_key, base_url, llm_model).map_err(|e| {
            PyRuntimeError::new_err(format!(
                "Failed to create OpenAI-compatible LLM provider: {}",
                e
            ))
        })?,
    ))
}

// Safety: PyObject is Send. We acquire the GIL before touching it.
unsafe impl Send for PyToolInterceptor {}
unsafe impl Sync for PyToolInterceptor {}

impl ToolInterceptor for PyToolInterceptor {
    fn before_tool_call(&self, tool_name: &str, arguments: &str) -> BeforeToolCallAction {
        let Some(ref py_fn) = self.before_fn else {
            return BeforeToolCallAction::Proceed;
        };
        Python::with_gil(|py| match py_fn.call1(py, (tool_name, arguments)) {
            Ok(result) => {
                if result.is_none(py) {
                    BeforeToolCallAction::Proceed
                } else {
                    match result.extract::<String>(py) {
                        Ok(s) => BeforeToolCallAction::Stub(s),
                        Err(_) => BeforeToolCallAction::Proceed,
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[interceptor] before_tool_call raised: {}", e);
                BeforeToolCallAction::Proceed
            }
        })
    }

    fn after_tool_call(
        &self,
        tool_name: &str,
        arguments: &str,
        result: &str,
    ) -> AfterToolCallAction {
        let Some(ref py_fn) = self.after_fn else {
            return AfterToolCallAction::PassThrough;
        };
        Python::with_gil(|py| match py_fn.call1(py, (tool_name, arguments, result)) {
            Ok(ret) => {
                if ret.is_none(py) {
                    AfterToolCallAction::PassThrough
                } else {
                    match ret.extract::<String>(py) {
                        Ok(s) => AfterToolCallAction::Override(s),
                        Err(_) => AfterToolCallAction::PassThrough,
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[interceptor] after_tool_call raised: {}", e);
                AfterToolCallAction::PassThrough
            }
        })
    }
}

// ── Composite Interceptor (ArtifactInterceptor + optional PyToolInterceptor) ──

/// Composes an optional `PyToolInterceptor` with an `ArtifactInterceptor`.
///
/// `before_tool_call`: py interceptor runs first — if it returns `Stub`, use it;
/// otherwise fall back to `ArtifactInterceptor`. This preserves evaluation sandbox
/// semantics where the Python hook must see *all* tool calls.
///
/// `after_tool_call`: delegates to py interceptor only (`ArtifactInterceptor` always
/// returns `PassThrough`).
struct CompositeHook {
    py_hook: Option<PyToolInterceptor>,
    artifact: ArtifactInterceptor,
}

impl ToolInterceptor for CompositeHook {
    fn before_tool_call(&self, tool_name: &str, arguments: &str) -> BeforeToolCallAction {
        if let Some(ref py) = self.py_hook {
            let action = py.before_tool_call(tool_name, arguments);
            if matches!(action, BeforeToolCallAction::Stub(_)) {
                return action;
            }
        }
        self.artifact.before_tool_call(tool_name, arguments)
    }

    fn after_tool_call(
        &self,
        tool_name: &str,
        arguments: &str,
        result: &str,
    ) -> AfterToolCallAction {
        if let Some(ref py) = self.py_hook {
            py.after_tool_call(tool_name, arguments, result)
        } else {
            AfterToolCallAction::PassThrough
        }
    }
}

// ── Python-exposed AgentRunner ──────────────────────────────────

/// Lightweight handle that can cancel an in-flight `AgentRunner` turn
/// from a different thread without holding a mutable borrow on the runner.
#[pyclass(name = "CancelHandle")]
struct PyCancelHandle {
    notify: Arc<tokio::sync::Notify>,
}

#[pymethods]
impl PyCancelHandle {
    /// Signal the runner to abort the current turn.
    ///
    /// Safe to call from any thread. If no turn is in progress the signal
    /// is consumed on the next `recv_event()` call.
    fn cancel(&self) {
        self.notify.notify_one();
    }
}

/// A headless agent runner for testing.
///
/// Wraps `DefaultAgentBackend` with a real LLM provider but lets
/// Python test code intercept tool calls via hook callables.
///
/// Usage:
/// ```python
/// runner = AgentRunner(
///     llm_url="https://api.openai.com/v1",
///     llm_api_key="sk-...",
///     llm_model="gpt-4o-mini",
///     system_prompt="You are a hotel receptionist...",
///     graph_json='{"entry": "receptionist", ...}',
///     before_tool_call=lambda name, args: "Error: 500" if name == "x" else None,
/// )
/// runner.start_turn("Check availability")
/// while (event := runner.recv_event()) is not None:
///     print(event)
/// ```
#[pyclass(name = "AgentRunner")]
pub struct PyAgentRunner {
    backend: DefaultAgentBackend,
    runtime: Arc<tokio::runtime::Runtime>,
    cancel_notify: Arc<tokio::sync::Notify>,
}

impl PyAgentRunner {
    fn start_turn_internal(&mut self, py: Python<'_>, text: &str) -> PyResult<()> {
        self.backend.add_user_message(text.to_string());

        // Release the GIL while Rust runtime executes the turn. Tool-call
        // hooks may call back into Python on worker threads and need to
        // acquire the GIL; holding it here can deadlock.
        py.allow_threads(|| {
            self.runtime.block_on(async {
                self.backend
                    .start_turn()
                    .await
                    .map_err(|e| format!("Failed to start turn: {}", e))
            })
        })
        .map_err(PyRuntimeError::new_err)
    }

    fn recv_event_internal(&mut self, py: Python<'_>) -> Option<AgentEvent> {
        let notify = self.cancel_notify.clone();
        py.allow_threads(|| {
            self.runtime.block_on(async {
                tokio::select! {
                    event = self.backend.recv() => event,
                    _ = notify.notified() => {
                        self.backend.cancel();
                        None
                    }
                }
            })
        })
    }

    fn event_to_pyobject(py: Python<'_>, event: AgentEvent) -> PyResult<PyObject> {
        let dict = pyo3::types::PyDict::new(py);
        match event {
            AgentEvent::Token(t) => {
                dict.set_item("type", "token")?;
                dict.set_item("text", t)?;
            }
            AgentEvent::ToolCallStarted {
                id,
                name,
                side_effect,
            } => {
                dict.set_item("type", "tool_call_started")?;
                dict.set_item("id", id)?;
                dict.set_item("name", name)?;
                dict.set_item("side_effect", side_effect)?;
            }
            AgentEvent::ToolCallCompleted {
                id,
                name,
                success,
                error_message,
            } => {
                dict.set_item("type", "tool_call_completed")?;
                dict.set_item("id", id)?;
                dict.set_item("name", name)?;
                dict.set_item("success", success)?;
                dict.set_item("error_message", error_message)?;
            }
            AgentEvent::Finished { content } => {
                dict.set_item("type", "finished")?;
                dict.set_item("text", content)?;
            }
            AgentEvent::Error(e) => {
                dict.set_item("type", "error")?;
                dict.set_item("text", e)?;
            }
            AgentEvent::HangUp { reason, content } => {
                dict.set_item("type", "hang_up")?;
                dict.set_item("reason", reason)?;
                dict.set_item("content", content)?;
            }
            AgentEvent::LlmComplete {
                provider,
                model,
                duration_ms,
                ttfb_ms,
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                ..
            } => {
                dict.set_item("type", "llm_complete")?;
                dict.set_item("provider", provider)?;
                dict.set_item("model", model)?;
                dict.set_item("duration_ms", duration_ms)?;
                dict.set_item("ttfb_ms", ttfb_ms)?;
                dict.set_item("prompt_tokens", prompt_tokens)?;
                dict.set_item("completion_tokens", completion_tokens)?;
                dict.set_item("cache_read_tokens", cache_read_tokens)?;
            }
            AgentEvent::OnHold { duration_secs } => {
                dict.set_item("type", "on_hold")?;
                dict.set_item("duration_secs", duration_secs)?;
            }
        }

        Ok(dict.into())
    }
}

#[pymethods]
impl PyAgentRunner {
    #[new]
    #[pyo3(signature = (
        llm_url,
        llm_api_key,
        llm_model,
        system_prompt,
        llm_provider = "",
        graph_json = None,
        before_tool_call = None,
        after_tool_call = None,
        temperature = 0.7,
        max_tokens = 32768,
        greeting = None,
        secrets = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        llm_url: &str,
        llm_api_key: &str,
        llm_model: &str,
        system_prompt: &str,
        llm_provider: &str,
        graph_json: Option<&str>,
        before_tool_call: Option<PyObject>,
        after_tool_call: Option<PyObject>,
        temperature: f64,
        max_tokens: u32,
        greeting: Option<String>,
        secrets: Option<HashMap<String, String>>,
    ) -> PyResult<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("agent-runner")
                .build()
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?,
        );

        // Parse agent graph if provided
        let agent_graph: Option<AgentGraphDef> = match graph_json {
            Some(json_str) => {
                let graph: AgentGraphDef = serde_json::from_str(json_str)
                    .map_err(|e| PyRuntimeError::new_err(format!("Invalid graph JSON: {}", e)))?;
                Some(graph)
            }
            None => None,
        };

        // Build LLM provider
        let provider = build_runner_llm_provider(llm_provider, llm_api_key, llm_url, llm_model)?;

        // Build backend config — convert secrets at the PyO3 boundary
        let secret_map: SecretMap = secrets
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();
        let config = AgentBackendConfig {
            temperature,
            max_tokens,
            max_tool_rounds: 5,
            secrets: Arc::new(std::sync::RwLock::new(secret_map)),
            ..Default::default()
        };

        let mut backend = DefaultAgentBackend::new(provider.into(), agent_graph, config);

        // Always attach CompositeHook: py interceptor (if provided) runs first,
        // then ArtifactInterceptor handles save/read/list_artifacts as fallback.
        let py_hook = if before_tool_call.is_some() || after_tool_call.is_some() {
            Some(PyToolInterceptor {
                before_fn: before_tool_call,
                after_fn: after_tool_call,
            })
        } else {
            None
        };
        let composite = CompositeHook {
            py_hook,
            artifact: ArtifactInterceptor::new(ArtifactStore::new()),
        };
        backend = backend.with_interceptor(Arc::new(composite));

        // Set system prompt
        backend.set_system_prompt(system_prompt.to_string());

        // Add greeting to conversation if provided
        if let Some(ref g) = greeting {
            if !g.trim().is_empty() {
                backend.add_assistant_message(g.clone());
            }
        }

        info!(
            "[AgentRunner] Initialized (provider={}, model={}, graph={})",
            llm_provider,
            llm_model,
            graph_json.is_some()
        );

        Ok(Self {
            backend,
            runtime,
            cancel_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Send a user message and run a full agent turn.
    ///
    /// Returns a list of event dicts, each with a `type` key:
    /// - `{"type": "token", "text": "..."}`
    /// - `{"type": "tool_call_started", "id": "...", "name": "..."}`
    /// - `{"type": "tool_call_completed", "id": "...", "name": "...", "success": true|false, "error_message": "..." or None}`
    /// - `{"type": "finished", "text": "..." or None}`
    /// - `{"type": "hang_up", "reason": "...", "content": "..." or None}`
    /// - `{"type": "error", "text": "..."}`
    fn send(&mut self, py: Python<'_>, text: &str) -> PyResult<Vec<PyObject>> {
        self.start_turn_internal(py, text)?;

        let mut py_events = Vec::new();
        while let Some(event) = self.recv_event_internal(py) {
            py_events.push(Self::event_to_pyobject(py, event)?);
        }

        Ok(py_events)
    }

    /// Return a lightweight handle that can cancel the current turn from
    /// another thread without borrowing the runner mutably.
    fn cancel_handle(&self) -> PyCancelHandle {
        PyCancelHandle {
            notify: self.cancel_notify.clone(),
        }
    }

    /// Start a user turn and stream events incrementally via `recv_event()`.
    #[pyo3(name = "start_turn")]
    fn py_start_turn(&mut self, py: Python<'_>, text: &str) -> PyResult<()> {
        self.start_turn_internal(py, text)
    }

    /// Receive the next event for the in-flight turn.
    ///
    /// Returns `None` once the turn has fully completed.
    #[pyo3(name = "recv_event")]
    fn py_recv_event(&mut self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        self.recv_event_internal(py)
            .map(|event| Self::event_to_pyobject(py, event))
            .transpose()
    }
}

// ── Language config exports ─────────────────────────────────────

/// Return the supported language list as Python dicts.
///
/// Shape: `[{"code": "en", "label": "English"}, ...]`
///
/// The Python admin API serves these from `GET /api/agents/languages`.
#[pyfunction]
fn get_supported_languages(py: Python<'_>) -> PyResult<PyObject> {
    let list = pyo3::types::PyList::empty(py);
    for lang in SUPPORTED_LANGUAGES {
        let d = pyo3::types::PyDict::new(py);
        d.set_item("code", lang.code)?;
        d.set_item("label", lang.label)?;
        d.set_item("elevenlabs_code", lang.elevenlabs_code)?;
        d.set_item("deepgram_code", lang.deepgram_code)?;
        d.set_item("cartesia_code", lang.cartesia_code)?;
        list.append(d)?;
    }
    Ok(list.into())
}

/// Return ElevenLabs model IDs that accept the `language_code` URL parameter.
#[pyfunction]
fn get_elevenlabs_multilingual_models() -> Vec<String> {
    ELEVENLABS_MULTILINGUAL_MODELS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Return the full TTS model catalog as Python dicts.
///
/// Each dict has the shape:
/// ```python
/// {
///   "provider": "cartesia-ws",
///   "model_id": "sonic-2",
///   "label": "Cartesia Sonic 2",
///   "supported_languages": ["en", "es", ...],
///   "language_voices": [
///     {"language_code": "en", "voice_id": "...", "voice_label": "..."}
///   ]
/// }
/// ```
///
/// Used by `GET /api/agents/tts-models` to populate the UI model picker
/// and validate model/language compatibility at PATCH time.
#[pyfunction]
fn get_tts_model_catalog(py: Python<'_>) -> PyResult<PyObject> {
    let list = pyo3::types::PyList::empty(py);
    for spec in TTS_MODEL_CATALOG {
        let d = pyo3::types::PyDict::new(py);
        d.set_item("provider", spec.provider)?;
        d.set_item("model_id", spec.model_id)?;
        d.set_item("label", spec.label)?;
        d.set_item("supported_languages", spec.supported_languages.to_vec())?;
        let voices = pyo3::types::PyList::empty(py);
        for v in spec.language_voices {
            let vd = pyo3::types::PyDict::new(py);
            vd.set_item("language_code", v.language_code)?;
            vd.set_item("voice_id", v.voice_id)?;
            vd.set_item("voice_label", v.voice_label)?;
            voices.append(vd)?;
        }
        d.set_item("language_voices", voices)?;
        list.append(d)?;
    }
    Ok(list.into())
}

/// Return `True` if the (provider, model_id) combination can synthesize `language`.
///
/// English and empty language codes always return `True`.
/// Models not in the catalog return `False` for non-English languages so that
/// misconfigured agents are caught early.
///
/// Used by `PATCH /api/agents/{id}/config` to generate model_warning.
#[pyfunction]
fn check_tts_model_language(provider: &str, model_id: &str, language: &str) -> bool {
    _tts_model_supports_language(provider, model_id, language)
}

/// Return the full STT model catalog as Python dicts.
#[pyfunction]
fn get_stt_model_catalog(py: Python<'_>) -> PyResult<PyObject> {
    let list = pyo3::types::PyList::empty(py);
    for spec in STT_MODEL_CATALOG {
        let d = pyo3::types::PyDict::new(py);
        d.set_item("provider", spec.provider)?;
        d.set_item("model_id", spec.model_id)?;
        d.set_item("label", spec.label)?;
        d.set_item("supported_languages", spec.supported_languages.to_vec())?;
        list.append(d)?;
    }
    Ok(list.into())
}

/// Return `True` if the (provider, model_id) combination can transcribe `language`.
#[pyfunction]
fn check_stt_model_language(provider: &str, model_id: &str, language: &str) -> bool {
    _stt_model_supports_language(provider, model_id, language)
}

// ── Module registration ─────────────────────────────────────────

/// The `voice_engine` Python module.
#[pymodule]
fn voice_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize tracing (if not already initialized)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "voice_engine=info,agent_kit=info".into()),
        )
        .try_init();

    m.add_class::<PySessionConfig>()?;
    m.add_class::<PyServerConfig>()?;
    m.add_class::<PyVoiceServer>()?;
    m.add_class::<PyAgentRunner>()?;
    m.add_class::<PyCancelHandle>()?;
    m.add_function(wrap_pyfunction!(get_supported_languages, m)?)?;
    m.add_function(wrap_pyfunction!(get_elevenlabs_multilingual_models, m)?)?;
    m.add_function(wrap_pyfunction!(get_tts_model_catalog, m)?)?;
    m.add_function(wrap_pyfunction!(check_tts_model_language, m)?)?;
    m.add_function(wrap_pyfunction!(get_stt_model_catalog, m)?)?;
    m.add_function(wrap_pyfunction!(check_stt_model_language, m)?)?;

    m.add_function(wrap_pyfunction!(validate_javascript, m)?)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    #[test]
    fn hang_up_event_serializes_content() {
        Python::with_gil(|py| {
            let event = AgentEvent::HangUp {
                reason: "task_complete".into(),
                content: Some("Goodbye.".into()),
            };

            let obj = PyAgentRunner::event_to_pyobject(py, event).expect("event should serialize");
            let dict = obj.bind(py).downcast::<PyDict>().expect("dict payload");

            let event_type = dict
                .get_item("type")
                .expect("type lookup")
                .expect("type value")
                .extract::<String>()
                .expect("string type");
            let reason = dict
                .get_item("reason")
                .expect("reason lookup")
                .expect("reason value")
                .extract::<String>()
                .expect("string reason");
            let content = dict
                .get_item("content")
                .expect("content lookup")
                .expect("content value")
                .extract::<Option<String>>()
                .expect("optional content");

            assert_eq!(event_type, "hang_up");
            assert_eq!(reason, "task_complete");
            assert_eq!(content.as_deref(), Some("Goodbye."));
        });
    }
}

fn map_recording_config(
    proto_config: &proto::agent::RecordingConfig,
) -> agent_kit::RecordingConfig {
    let mut base = agent_kit::RecordingConfig::default();
    base.enabled = proto_config.enabled;

    match proto_config.audio_layout() {
        proto::agent::AudioLayout::Stereo => base.audio_layout = agent_kit::AudioLayout::Stereo,
        proto::agent::AudioLayout::Mono => base.audio_layout = agent_kit::AudioLayout::Mono,
        proto::agent::AudioLayout::Unspecified => {}
    }

    if proto_config.sample_rate != 0 {
        base.sample_rate = proto_config.sample_rate;
    }

    match proto_config.audio_format() {
        proto::agent::AudioFormat::Opus => base.audio_format = agent_kit::AudioFormat::Opus,
        proto::agent::AudioFormat::Wav => base.audio_format = agent_kit::AudioFormat::Wav,
        proto::agent::AudioFormat::Unspecified => {}
    }

    if proto_config.max_duration_secs != 0 {
        base.max_duration_secs = proto_config.max_duration_secs;
    }

    base.save_transcript = proto_config.save_transcript;
    base.include_tool_details = proto_config.include_tool_details;
    base.include_llm_metadata = proto_config.include_llm_metadata;

    if !proto_config.output_uri.is_empty() {
        base.output_uri = proto_config.output_uri.clone();
    }

    base
}
