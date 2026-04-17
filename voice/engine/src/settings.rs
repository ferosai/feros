//! Centralized application settings — parsed from environment variables.
//!
//! Works like Python's pydantic `BaseSettings`: define a struct with
//! `#[derive(Deserialize)]` and `serde(default)`, then call
//! [`Settings::from_env()`] to populate it from env vars.
//!
//! # Design
//!
//! Each crate owns its own config type:
//! - **`voice-transport`**: [`IceConfig`](voice_transport::webrtc::IceConfig) —
//!   STUN/TURN settings
//! - **`agent-kit`**: agent/runtime-specific configs
//!
//! This `Settings` struct holds only what the **binary** itself needs:
//! server bind address, service URLs, etc. Crate-specific configs live
//! in their own crates and are composed at the binary level.
//!
//! # Environment Variables
//!
//! | Variable | Default | Description |
//! |----------|---------|-------------|
//! | `SERVER__LISTEN_HOST` | `0.0.0.0` | Server bind address |
//! | `SERVER__LISTEN_PORT` | `8300` | Server bind port |
//! | `STT_BASE_URL` | `http://localhost:8100` | STT service URL |
//! | `STT_PROVIDER` | *(empty)* | STT provider (`faster-whisper`, `deepgram`, etc.) |
//! | `STT_MODEL` | *(empty)* | STT model name (`large-v3`, `nova-3`, etc.) |
//! | `STT_API_KEY` | *(empty)* | STT API key (required for cloud providers like Deepgram) |
//! | `LLM_BASE_URL` | `http://localhost:11434/v1` | LLM service URL (include `/v1` for OpenAI-compat) |
//! | `LLM_API_KEY` | *(empty)* | LLM API key |
//! | `LLM_MODEL` | `llama3.2` | LLM model name |
//! | `LLM_PROVIDER` | *(empty)* | LLM provider (`groq`, `openai`, etc.) |
//! | `TTS_BASE_URL` | `http://localhost:8200` | TTS service URL |
//! | `TTS_PROVIDER` | *(empty)* | TTS provider (`builtin`, `cartesia`, `elevenlabs`, `openai`, `deepgram`) |
//! | `TTS_MODEL` | *(empty)* | TTS model name |
//! | `TTS_API_KEY` | *(empty)* | TTS API key (required for cloud providers) |
//! | `TWILIO_ACCOUNT_SID` | *(empty)* | Twilio Account SID |
//! | `TWILIO_AUTH_TOKEN` | *(empty)* | Twilio Auth Token |
//! | `TELNYX_API_KEY` | *(empty)* | Telnyx API Key |

use std::sync::OnceLock;

use serde::Deserialize;

/// Feature flags for `DefaultAgentBackend` background tasks.
///
/// Parsed from environment variables via `envy`. Add this to the table in
/// the module-level doc when adding new flags.
///
/// | Variable | Default | Description |
/// |---|---|---|
/// | `AGENT__CONTEXT_SUMMARIZER` | `true` | Compress conversation history when it grows long |
/// | `AGENT__TOOL_FILLER` | `false` | Speak a filler phrase while side-effecting tools run |
#[derive(Debug, Clone, Deserialize)]
pub struct AgentTaskSettings {
    #[serde(rename = "agent__context_summarizer", default = "default_true")]
    pub agent_context_summarizer: bool,

    #[serde(rename = "agent__tool_filler", default)]
    pub agent_tool_filler: bool,
}

static AGENT_TASK_SETTINGS: OnceLock<AgentTaskSettings> = OnceLock::new();

impl AgentTaskSettings {
    /// Parse (or return the cached) settings from environment variables.
    ///
    /// Parsed exactly once per process. Subsequent calls return the same
    /// instance without re-reading the environment.
    ///
    /// # `.env` file loading
    ///
    /// This method calls `envy::from_env()` directly and does **not** load a
    /// `.env` file itself. In development, `.env` is loaded by
    /// [`Settings::from_env()`] via `dotenvy`. Because the binary always calls
    /// `Settings::from_env()` during startup (before any session starts),
    /// variables defined in `.env` will be present in the process environment
    /// by the time `AgentTaskSettings::get()` is first invoked.
    ///
    /// If you call `AgentTaskSettings::get()` before `Settings::from_env()`
    /// (e.g. in a standalone binary or test harness), load the `.env` file
    /// yourself first with `let _ = dotenvy::dotenv();`.
    pub fn get() -> &'static Self {
        AGENT_TASK_SETTINGS.get_or_init(|| {
            envy::from_env::<Self>().unwrap_or_else(|e| {
                tracing::warn!(
                    "[settings] Failed to parse AgentTaskSettings from env: {e} — using defaults"
                );
                Self::default()
            })
        })
    }
}

impl Default for AgentTaskSettings {
    fn default() -> Self {
        Self {
            agent_context_summarizer: true,
            agent_tool_filler: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Application settings — the binary's own config.
///
/// Does **not** include crate-level configs (ICE, observability sinks).
/// Those are owned by their respective crates and parsed separately.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    // ── Server ───────────────────────────────────────────────────
    /// Bind address for the HTTP/WS server.
    #[serde(rename = "server__listen_host", default = "default_host")]
    pub listen_host: String,

    /// Bind port for the HTTP/WS server.
    #[serde(rename = "server__listen_port", default = "default_port")]
    pub listen_port: u16,

    // ── Service URLs ─────────────────────────────────────────────
    /// STT (Speech-to-Text) service base URL.
    #[serde(default = "default_stt_url")]
    pub stt_base_url: String,

    /// STT provider identifier (`faster-whisper`, `deepgram`, etc.).
    #[serde(default)]
    pub stt_provider: String,

    /// STT model name (`large-v3`, `nova-3`, etc.).
    #[serde(default)]
    pub stt_model: String,

    /// STT API key (required for cloud providers like Deepgram).
    #[serde(default)]
    pub stt_api_key: String,

    /// LLM service base URL (must include `/v1` for OpenAI-compatible endpoints).
    #[serde(default = "default_llm_url")]
    pub llm_base_url: String,

    /// LLM API key (empty = no auth).
    #[serde(default)]
    pub llm_api_key: String,

    /// LLM model name.
    #[serde(default = "default_llm_model")]
    pub llm_model: String,

    /// LLM provider identifier (`groq`, `openai`, `anthropic`, etc.).
    /// Empty = falls back to OpenAI-compatible provider (works with any OpenAI-compat endpoint).
    #[serde(default)]
    pub llm_provider: String,

    /// TTS (Text-to-Speech) service base URL.
    #[serde(default = "default_tts_url")]
    pub tts_base_url: String,

    /// TTS provider identifier (`builtin`, `cartesia`, `elevenlabs`, `openai`, `deepgram`).
    #[serde(default)]
    pub tts_provider: String,

    /// TTS model name.
    #[serde(default)]
    pub tts_model: String,

    /// TTS API key (required for cloud providers).
    #[serde(default)]
    pub tts_api_key: String,

    // ── Telephony Credentials (optional) ─────────────────────────
    /// Twilio Account SID (for call control: hangup, transfer).
    #[serde(default)]
    pub twilio_account_sid: String,

    /// Twilio Auth Token (for REST API authentication).
    #[serde(default)]
    pub twilio_auth_token: String,

    /// Telnyx API Key (for call control).
    #[serde(default)]
    pub telnyx_api_key: String,
}

impl Settings {
    /// Parse settings from environment variables.
    ///
    /// Loads `.env` file (if present) first, then reads env vars.
    /// Missing vars get their `serde(default)` fallback values.
    pub fn from_env() -> Result<Self, envy::Error> {
        // Load .env file (silently ignore if absent)
        let _ = dotenvy::dotenv();
        envy::from_env::<Self>()
    }
}

// ── Default value functions ──────────────────────────────────────

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8300
}

fn default_stt_url() -> String {
    "http://localhost:8100".to_string()
}

fn default_llm_url() -> String {
    "http://localhost:11434/v1".to_string()
}

fn default_llm_model() -> String {
    "llama3.2".to_string()
}

fn default_tts_url() -> String {
    "http://localhost:8200".to_string()
}
