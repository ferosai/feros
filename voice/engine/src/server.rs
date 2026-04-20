//! VoiceServer — embeddable server for voice sessions.
//!
//! Can be started standalone (`main.rs`) or embedded from Python via PyO3.
//!
//! # Security Model
//!
//! All endpoints require a valid **session token** — an HMAC-SHA256 signature
//! of the session ID, produced by the caller during session registration.
//! Rust validates tokens statelessly by recomputing the HMAC with a shared secret.
//!
//! There are no unauthenticated "inline" endpoints. All sessions must be
//! pre-registered by the embedder (which handles authentication).
//!
//! Supports two transports:
//! - **WebSocket**: Clients connect at `/ws/voice/{session_id}?token=...`.
//! - **WebRTC** (feature `webrtc`): Clients POST an SDP offer to `/rtc/offer/{session_id}?token=...`.
//!
//! Both paths produce a `TransportHandle` that feeds the `VoiceSession`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use tokio::sync::{broadcast, mpsc};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use voice_trace::{Event, Tracer};
use voice_transport::websocket::WebSocketTransport;
use voice_transport::{TransportCommand, TransportHandle};

#[cfg(any(feature = "webrtc", feature = "telephony"))]
use axum::routing::post;
#[cfg(feature = "webrtc")]
use axum::Json;
#[cfg(feature = "webrtc")]
use voice_transport::webrtc::{
    ice_provider_from_config, IceConfig, IceProvider, WebRtcConnection, WebRtcTransport,
};
#[cfg(feature = "webrtc")]
use voice_transport::TransportEvent;

#[cfg(feature = "telephony")]
#[cfg(feature = "telephony")]
use voice_transport::telephony::{
    TelephonyConfig, TelephonyCredentials as TelephonyTransportCredentials, TelephonyEncoding,
    TelephonyProviderKind, TelephonyTransport,
};

use crate::providers::llm::LlmProvider;
use crate::providers::stt::SttProviderConfig;
use crate::providers::tts::TtsProviderConfig;
use crate::session::{SessionConfig, VoiceSession};
use agent_kit::agent_backends::SharedSecretMap;

// ── Provider Config Types ───────────────────────────────────────

/// External service provider configuration — URLs and credentials.
///
/// Each provider (STT, LLM, TTS) gets a URL and any auth fields it needs.
/// Currently only LLM requires an API key; STT/TTS fields can be added
/// here as commercial providers are integrated.
#[derive(Clone, Debug, Default)]
pub struct ProviderConfig {
    pub stt_url: String,
    /// STT provider identifier (e.g. "faster-whisper", "deepgram").
    /// When empty, falls back to the builtin speech-inference provider.
    pub stt_provider: String,
    /// STT model name (e.g. "large-v3", "nova-3").
    pub stt_model: String,
    /// STT API key (required for cloud providers like Deepgram).
    pub stt_api_key: String,
    pub llm_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    /// LLM provider identifier (e.g. "groq", "openai", "anthropic", "deepseek", "gemini").
    /// When empty or unrecognized, falls back to the OpenAI-compatible provider.
    pub llm_provider: String,
    pub tts_url: String,
    /// TTS provider identifier (e.g. "builtin", "cartesia", "elevenlabs", "openai", "deepgram").
    pub tts_provider: String,
    /// TTS model name (e.g. "sonic-english", "aura-asteria-en", "tts-1").
    pub tts_model: String,
    /// TTS API key (required for cloud providers like Cartesia, ElevenLabs, etc.).
    pub tts_api_key: String,
    /// TTS voice ID (e.g. `"aura-2-thalia-en"` for Deepgram, `"alloy"` for OpenAI).
    /// For Deepgram, this IS the API model parameter.
    pub tts_voice_id: String,
}

/// Telephony provider credentials.
#[derive(Clone, Debug, Default)]
pub struct TelephonyCredentials {
    pub twilio_account_sid: String,
    pub twilio_auth_token: String,
    pub telnyx_api_key: String,
    pub telnyx_connection_id: String,
}

// ── Registered Session ──────────────────────────────────────────

/// Configuration for a pre-registered voice session.
///
/// Python registers these before the client connects.
/// Sessions expire after `SESSION_TTL_SECS` (default 60s) if not consumed.
#[derive(Clone)]
pub struct RegisteredSession {
    pub config: SessionConfig,
    pub providers: ProviderConfig,
    /// Pre-resolved secrets for this session.
    ///
    /// Populated by voice-server at registration time from the vault.
    /// voice-engine never touches the vault directly — it reads from here.
    /// The background refresh task (if any) updates this map in-place.
    pub secrets: Option<SharedSecretMap>,
    /// Background task that periodically refreshes secrets from the vault.
    ///
    /// Wrapped in `Arc<Mutex>` so `RegisteredSession` remains `Clone`.
    /// The first handler to consume the session takes the handle out.
    /// It must be aborted when the session ends.
    pub secret_refresh_handle:
        Option<std::sync::Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
    /// When this session was registered (for TTL expiry).
    pub created_at: Instant,
    /// Optional pre-created Tracer for this session.
    ///
    /// Wrapped in `Arc<Mutex>` so `RegisteredSession` remains `Clone`.
    /// Voice-server creates the Tracer, attaches recording subscribers
    /// (filesystem, S3, etc.), then stores it here. Voice-engine uses it
    /// instead of creating a new one, so subscribers don't miss early events.
    /// The first handler to consume the session takes the Tracer out via
    /// `arc.lock().ok()?.take()`. Subsequent handlers (e.g. hybrid mode)
    /// get `None` and create a fresh Tracer internally.
    pub tracer: Option<std::sync::Arc<std::sync::Mutex<Option<voice_trace::Tracer>>>>,
    /// Per-session telephony credentials (Twilio/Telnyx).
    ///
    /// Populated by voice-server at inbound webhook time from the phone number's
    /// `provider_credentials_encrypted` field. Used by `handle_telephony_session`
    /// to build `TelephonyConfig` instead of falling back to the global
    /// `ServerState.telephony_creds` (which is always empty in new deployments).
    pub telephony_creds: Option<TelephonyCredentials>,
    /// Webhook base URL for provider callbacks (e.g., transfers).
    ///
    /// Populated by voice-server during session registration from the database.
    pub webhook_base_url: Option<String>,
    /// The E.164 phone number that received this call (e.g. `"+18005551234"`).
    ///
    /// Required for supervised conference-based transfer: the transport places
    /// a new outbound call *from* this number to the destination. Populated by
    /// voice-server from the Twilio/Telnyx webhook `To` parameter.
    pub from_number: Option<String>,
}

impl RegisteredSession {
    /// Extract the pre-created Tracer (with any recording subscribers attached).
    ///
    /// Takes the Tracer out of its `Arc<Mutex<Option<_>>>` wrapper so that only
    /// the first handler to consume the session gets the Tracer. Subsequent
    /// handlers (e.g. hybrid mode fallback) get `None` and create one internally.
    pub fn take_tracer(&self) -> Option<Tracer> {
        self.tracer.as_ref().and_then(|arc| arc.lock().ok()?.take())
    }

    /// Extract the secret refresh task handle, if one was registered.
    ///
    /// The caller must abort this handle when the session ends.
    pub fn take_refresh_handle(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.secret_refresh_handle
            .as_ref()
            .and_then(|arc| arc.lock().ok()?.take())
    }
}

impl Drop for RegisteredSession {
    fn drop(&mut self) {
        // Abort the background secret refresh task when the session is dropped.
        //
        // This covers all early-exit paths where `run_session_with_transport` is
        // never reached (and thus never calls `h.abort()`):
        //   - TTL cleanup task evicts unconnected sessions from the DashMap
        //   - WebRTC ICE connection timeout / failure
        //   - Telephony session_id mismatch (start-message timeout → random UUID)
        //
        // Note: `take_refresh_handle` removes the handle from the inner Option,
        // so for the normal path — where the caller already took the handle and
        // called `h.abort()` inside `run_session_with_transport` — this no-ops.
        if let Some(arc) = &self.secret_refresh_handle {
            if let Ok(mut guard) = arc.lock() {
                if let Some(h) = guard.take() {
                    h.abort();
                }
            }
        }
    }
}

impl std::fmt::Debug for RegisteredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredSession")
            .field("config", &self.config.agent_id)
            .field("secrets", &self.secrets.is_some())
            .field("created_at", &self.created_at)
            .field("tracer", &self.tracer.is_some())
            .finish()
    }
}

// ── Session Summary ──────────────────────────────────────────────

/// Minimal summary of a completed voice session.
///
/// Returned by `run_session_with_transport` for logging purposes.
/// Recording and DB writes are handled by recording subscriber callbacks,
/// not by this summary.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub agent_id: String,
    /// Seconds elapsed from session start to end.
    pub duration_secs: u32,
}

/// JSON control message from the client.
#[derive(Deserialize)]
struct ClientMessage {
    #[serde(rename = "type")]
    msg_type: String,
}

#[cfg(feature = "webrtc")]
#[derive(serde::Deserialize)]
struct RtcCandidateRequest {
    candidate: String,
}

/// Query parameters for token-authenticated endpoints.
#[derive(Deserialize)]
struct TokenQuery {
    /// HMAC-SHA256 session token produced by the caller during session registration.
    #[serde(default)]
    token: String,
    /// Session ID (used by endpoints that don't have it in the path, e.g. /rtc/ice-servers).
    #[serde(default)]
    #[allow(dead_code)]
    session_id: Option<String>,
}

type HmacSha256 = Hmac<Sha256>;

// ── Voice Server ────────────────────────────────────────────────

// ── Hybrid Session Entry ─────────────────────────────────────────

/// Bound state for a hybrid WebRTC+WS voice session.
///
/// In hybrid mode the WebRTC connection carries audio and the UI WebSocket
/// carries only UI events (transcripts, voice state, tool activity).
/// When the UI WebSocket closes (e.g. user clicks "Stop"), `forward_ws_events`
/// sends `TransportCommand::Close` on `control_tx`.  This drops the
/// transport's `audio_tx`, causing the reactor's `audio_rx` to return `None`
/// and triggering the normal transport-closed shutdown path — preserving the
/// full clean-shutdown sequence (SessionEnded → recording → DB write).
pub(crate) struct HybridSession {
    /// Broadcast sender for UI events subscribed to by the WS forward task.
    event_tx: broadcast::Sender<voice_trace::Event>,
    /// Transport control sender used to stop the WebRTC session when the
    /// UI WebSocket disconnects (happy-path user stop).
    control_tx: mpsc::UnboundedSender<TransportCommand>,
}

/// Shared server state — thread-safe via `Arc`.
#[derive(Clone)]
pub struct ServerState {
    /// Pre-registered session configs (session_id → config).
    pub sessions: Arc<DashMap<String, RegisteredSession>>,
    /// Active hybrid (WebRTC+WS) sessions keyed by session_id.
    ///
    /// Populated by `rtc_create_session` after the transport is built.
    /// Consumed by `forward_ws_events` on UI WebSocket close to send a
    /// clean shutdown signal to the reactor via `TransportCommand::Close`.
    pub(crate) hybrid_sessions: Arc<DashMap<String, HybridSession>>,

    /// Fallback provider URLs (used when session is configured inline).
    pub default_providers: ProviderConfig,
    /// Telephony credentials (Twilio/Telnyx API keys, connection IDs).
    pub telephony_creds: TelephonyCredentials,
    /// Telnyx Ed25519 public key for webhook signature validation.
    /// Server-global — all Telnyx webhooks are verified with this single key.
    /// Empty string disables signature validation (dev mode).
    pub telnyx_public_key: String,
    /// Shared secret for HMAC-SHA256 session token validation.
    /// Python signs `HMAC(secret, session_id)` → Rust re-derives and compares.
    pub auth_secret_key: Arc<String>,
    /// How long (seconds) a registered-but-unconnected session lives.
    pub session_ttl_secs: u64,
    /// ICE provider for STUN/TURN server configuration (WebRTC only).
    #[cfg(feature = "webrtc")]
    pub ice_provider: Arc<dyn IceProvider>,
    /// ICE configuration (STUN server, TURN settings — WebRTC only).
    #[cfg(feature = "webrtc")]
    pub ice_config: IceConfig,
}

impl ServerState {
    /// Build a new `ServerState`, performing all shared initialization:
    ///
    /// 1. Install rustls crypto provider
    /// 2. Parse ICE config and create provider (WebRTC only)
    /// 3. Allocate session/tracer maps
    ///
    /// Callers only supply the service URLs — everything else is handled here.
    ///
    /// **Note:** Callers must ensure `.env` is loaded (via `dotenvy::dotenv()`)
    /// *before* calling this constructor so ICE env vars are visible.
    ///
    /// # Panics
    ///
    /// Panics if `IceConfig` cannot be parsed from environment variables.
    pub fn new(
        providers: ProviderConfig,
        telephony: TelephonyCredentials,
        telnyx_public_key: String,
        auth_secret_key: String,
    ) -> Self {
        // Install rustls (idempotent — no-op if already installed)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Parse ICE config from env
        #[cfg(feature = "webrtc")]
        let ice_config: IceConfig = envy::from_env().expect("Failed to parse ICE config from env");

        // Parse session TTL from env (default 60s)
        let session_ttl_secs: u64 = std::env::var("SESSION_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);

        if auth_secret_key.is_empty() {
            warn!("AUTH__SECRET_KEY is empty — session token validation is disabled.");
        }

        Self {
            sessions: Arc::new(DashMap::new()),
            hybrid_sessions: Arc::new(DashMap::new()),

            default_providers: providers,
            telephony_creds: telephony,
            telnyx_public_key,
            auth_secret_key: Arc::new(auth_secret_key),
            session_ttl_secs,
            #[cfg(feature = "webrtc")]
            ice_provider: Arc::from(ice_provider_from_config(&ice_config)),
            #[cfg(feature = "webrtc")]
            ice_config,
        }
    }

    /// Produce an HMAC-SHA256 session token for `session_id`.
    ///
    /// Returns a hex-encoded `HMAC(secret, session_id)` that `validate_token`
    /// will accept. Used by `voice-server` when registering sessions for
    /// incoming telephony calls (replacing the previous Python-side signing).
    ///
    /// Returns an empty string when no secret is configured (dev mode).
    pub fn sign_token(&self, session_id: &str) -> String {
        if self.auth_secret_key.is_empty() {
            return String::new();
        }
        let Ok(mut mac) = HmacSha256::new_from_slice(self.auth_secret_key.as_bytes()) else {
            error!("Failed to create HMAC instance for sign_token");
            return String::new();
        };
        mac.update(session_id.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Validate an HMAC-SHA256 session token.
    ///
    /// Returns `true` if the token matches `HMAC(secret, session_id)`,
    /// or if no secret is configured (dev mode — logs a warning on first call).
    pub fn validate_token(&self, session_id: &str, token: &str) -> bool {
        if self.auth_secret_key.is_empty() {
            // No secret configured — skip validation (dev mode)
            return true;
        }

        if token.is_empty() {
            return false;
        }

        let Ok(mut mac) = HmacSha256::new_from_slice(self.auth_secret_key.as_bytes()) else {
            error!("Failed to create HMAC instance — rejecting token");
            return false;
        };
        mac.update(session_id.as_bytes());

        // Decode the hex token and verify (constant-time comparison)
        let Ok(token_bytes) = hex::decode(token) else {
            return false;
        };
        mac.verify_slice(&token_bytes).is_ok()
    }
}

/// Build the voice-engine axum [`Router`] without binding a listener.
///
/// This is the library entry-point for embedders (e.g. `voice-server`) that
/// want to merge voice-engine's routes into their own router.
/// `run_server` calls this internally.
pub fn build_router(state: ServerState) -> Router {
    #[allow(unused_mut)]
    let mut app = Router::new()
        .route("/ws/voice/{session_id}", get(ws_handler_registered))
        .route("/health", get(|| async { "ok" }));

    // Add WebRTC signaling routes when the feature is enabled
    #[cfg(feature = "webrtc")]
    {
        app = app
            .route("/rtc/offer/{session_id}", post(rtc_offer_registered))
            .route(
                "/rtc/candidate/{session_id}",
                post(rtc_candidate_registered),
            )
            .route("/rtc/ice-servers", get(rtc_get_ice_servers));
        info!("WebRTC signaling enabled at /rtc/offer/{{session_id}}, /rtc/candidate/{{session_id}}, /rtc/ice-servers");
    }

    // Add telephony WebSocket routes when the feature is enabled.
    // HTTP handshake routes (/incoming, /twiml, /call) are handled by the
    // Python admin API, which loads agent config from DB and pre-registers
    // sessions with us via register_session(). Twilio then connects its
    // media stream WebSocket directly to these WS endpoints.
    #[cfg(feature = "telephony")]
    {
        app = app.route("/telephony/twilio", get(telephony_twilio_handler));

        #[cfg(feature = "telnyx")]
        {
            app = app.route("/telephony/telnyx", get(telephony_telnyx_handler));
            info!("Telephony WS enabled at /telephony/twilio, /telephony/telnyx");
        }
        #[cfg(not(feature = "telnyx"))]
        {
            info!("Telephony WS enabled at /telephony/twilio");
        }
    }

    // CORS is applied by embedders (e.g. voice-server) or in run_server() for standalone mode.
    app.layer(TraceLayer::new_for_http()).with_state(state)
}

/// Start the voice server, binding `addr` and serving until shutdown.
///
/// Returns when the server is shut down.
pub async fn run_server(addr: SocketAddr, state: ServerState) {
    // Spawn session TTL cleanup task
    {
        let state = state.clone();
        let ttl = state.session_ttl_secs;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let before = state.sessions.len();
                state.sessions.retain(|id, s| {
                    let alive = s.created_at.elapsed().as_secs() < ttl;
                    if !alive {
                        info!("Session {} expired (TTL={}s)", id, ttl);
                    }
                    alive
                });
                let removed = before - state.sessions.len();
                if removed > 0 {
                    info!("Cleaned up {} expired session(s)", removed);
                }
            }
        });
    }

    let app = build_router(state);

    info!("🚀 Voice engine listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
    // NOTE: axum::serve blocks indefinitely; callers should run with graceful
    // shutdown when they need deterministic teardown hooks.
}

// ── WebSocket Handlers ──────────────────────────────────────────

/// Handler for pre-registered sessions: `/ws/voice/{session_id}?token=...`.
///
/// Validates the HMAC session token, then looks up the session config
/// from the `DashMap` and starts immediately.
async fn ws_handler_registered(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(params): Query<TokenQuery>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    // Validate session token
    if !state.validate_token(&session_id, &params.token) {
        warn!("Invalid session token for WS session {}", session_id);
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_registered_socket(socket, session_id, state))
        .into_response()
}

/// Handle a pre-registered session — config already in the DashMap.
///
/// If a WebRTC session has already claimed this session (stored a Tracer),
/// the WS becomes a read-only UI event forwarder.
/// Otherwise, the WS creates the full session (pure WS mode).
async fn handle_registered_socket(socket: WebSocket, session_id: String, state: ServerState) {
    // Wait briefly for the WebRTC handler to store its event sender.
    // The frontend opens WS and sends the RTC offer nearly simultaneously.
    let event_tx = {
        let mut found = None;
        for _ in 0..50 {
            // 50 × 100ms = 5s max wait
            if let Some(entry) = state.hybrid_sessions.get(&session_id) {
                found = Some(entry.value().event_tx.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        found
    };

    if let Some(event_tx) = event_tx {
        // Hybrid mode: WebRTC owns the session, WS only forwards UI events
        info!(
            "WS attached to WebRTC session {} — forwarding UI events",
            session_id
        );
        forward_ws_events(socket, event_tx, session_id, state).await;
    } else {
        // Pure WS mode: no WebRTC session → create the full session
        let registered = match state.sessions.remove(&session_id) {
            Some((_, reg)) => reg,
            None => {
                warn!("No registered session for id={}", session_id);
                return;
            }
        };

        let reg_tracer = registered.take_tracer();
        let secrets = registered.secrets.clone();
        let refresh_handle = registered.take_refresh_handle();

        info!("Starting pre-registered WS session: {}", session_id);
        run_voice_session(
            session_id,
            socket,
            registered.config.clone(),
            &registered.providers,
            secrets,
            refresh_handle,
            reg_tracer,
        )
        .await;
    }
}

/// Forward UI events from a broadcast sender to a WebSocket.
///
/// Used in hybrid mode: WebRTC carries audio, WS carries UI events.
async fn forward_ws_events(
    socket: WebSocket,
    event_tx: broadcast::Sender<voice_trace::Event>,
    session_id: String,
    state: ServerState,
) {
    use futures_util::SinkExt;
    use std::collections::HashSet;
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Subscribe with category filtering — only receive events we forward.
    let mut ws_events = voice_trace::FilteredReceiver::new(
        event_tx.subscribe(),
        HashSet::from([
            voice_trace::EventCategory::Session,
            voice_trace::EventCategory::Transcript,
            voice_trace::EventCategory::Tool,
            voice_trace::EventCategory::Agent,
            voice_trace::EventCategory::Metrics,
            voice_trace::EventCategory::Error,
        ]),
    );

    // Forward task: event bus → WS (JSON only, no audio)
    let tx_session_id = session_id.clone();
    let forward_task = tokio::spawn(async move {
        while let Some(event) = ws_events.recv().await {
            // SessionEnded is the canonical signal that the session is fully
            // complete. Break immediately *after* forwarding it so the client
            // sees the event, then we send the Close frame below.
            //
            // This is necessary because the HybridSession DashMap entry holds
            // a live clone of the broadcast::Sender, which keeps the channel
            // open even after the tracer drops. Without this guard the loop
            // would only exit once the DashMap entry is removed (several
            // seconds later), leaving the client connected long after hang_up.
            //
            // Safe for both Reactor and native paths — both unconditionally
            // emit SessionEnded at the end of their event loops.
            let is_session_ended = matches!(event, voice_trace::Event::SessionEnded);
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if ws_tx.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!("Failed to serialize event: {}", e);
                }
            }
            if is_session_ended {
                break;
            }
        }
        // SessionEnded was forwarded (or the channel closed). Send a WebSocket
        // Close frame so the client socket tears down immediately rather than
        // waiting for the client to disconnect on its own. This causes
        // ws_rx.next() in the receive loop below to return None, which lets
        // the cleanup code run and properly updates the UI state.
        let _ = ws_tx.send(Message::Close(None)).await;
        info!("WS event forwarder ended for {}", tx_session_id);
    });

    // Receive task: read WS messages from client (session.end, etc.)
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(cm) = serde_json::from_str::<ClientMessage>(&text) {
                    if cm.msg_type.as_str() == "session.end" {
                        info!("Client requested session end via WS");
                        break;
                    }
                }
            }
            Message::Close(_) => {
                info!("WS closed by client for {}", session_id);
                break;
            }
            _ => {}
        }
    }

    info!(
        "WS event receive loop ended for {}. Cleaning up session.",
        session_id
    );
    forward_task.abort();
    // Signal the WebRTC session to shut down cleanly.
    // Sending Close drops audio_tx → reactor's audio_rx returns None →
    // reactor runs cancel_pipeline() + emits SessionEnded → recording saved.
    if let Some((_, session)) = state.hybrid_sessions.remove(&session_id) {
        info!(
            "Removed hybrid session {}. Sending Close command.",
            session_id
        );
        let _ = session.control_tx.send(TransportCommand::Close);
    } else {
        warn!(
            "Failed to remove hybrid session {} — it may have been closed already.",
            session_id
        );
    }
}

// Inline session handler removed for security — all sessions must be
// pre-registered by the embedder before connecting.

/// Core session loop for WebSocket — creates transport handle and runs.
///
/// If `pre_tracer` is `Some`, it is used directly (it may already have
/// recording subscribers attached by voice-server). Otherwise a fresh `Tracer`
/// is created internally.
async fn run_voice_session(
    session_id: String,
    socket: WebSocket,
    config: SessionConfig,
    providers: &ProviderConfig,
    secrets: Option<SharedSecretMap>,
    refresh_handle: Option<tokio::task::JoinHandle<()>>,
    pre_tracer: Option<Tracer>,
) {
    let tracer = pre_tracer.unwrap_or_default();

    #[cfg(feature = "otel")]
    voice_trace::sinks::otel::spawn_otel_subscriber(&tracer);

    let input_sample_rate = config.input_sample_rate;
    let transport = WebSocketTransport::accept(socket, &tracer, input_sample_rate);

    run_session_with_transport(
        session_id,
        config,
        transport,
        providers,
        secrets,
        refresh_handle,
        tracer,
    )
    .await;
}

/// Create an empty `SharedSecretMap` (used when no vault secrets are provided).
fn empty_shared_secrets() -> SharedSecretMap {
    Arc::new(std::sync::RwLock::new(
        agent_kit::agent_backends::SecretMap::new(),
    ))
}

/// Build the correct LLM provider based on the provider name.
///
/// Maps known provider names to their vendor-specific rig implementations.
/// Falls back to rig's OpenAI client (with a custom base_url) for unknown
/// providers, which handles arbitrary OpenAI-compatible endpoints
/// (Ollama, vLLM, LiteLLM, etc.).
fn build_llm_provider(providers: &ProviderConfig) -> Box<dyn LlmProvider> {
    /// Try to construct a rig provider; on error, log and fall through.
    macro_rules! try_rig_provider {
        ($name:expr, $ctor:expr) => {
            match $ctor {
                Ok(p) => return Box::new(p),
                Err(e) => warn!(
                    "Failed to create {} provider, falling back to OpenAI-compat: {}",
                    $name, e
                ),
            }
        };
    }

    let base_url = Some(providers.llm_url.as_str());
    tracing::info!(
        "[llm] Building provider: name={:?} model={:?} url={:?}",
        providers.llm_provider,
        providers.llm_model,
        providers.llm_url,
    );
    match providers.llm_provider.as_str() {
        // Groq uses a 2-arg constructor (no base_url override)
        "groq" => try_rig_provider!(
            "Groq",
            agent_kit::GroqProvider::new(&providers.llm_api_key, &providers.llm_model)
        ),
        "openai" => try_rig_provider!(
            "OpenAI",
            agent_kit::OpenAiProvider::new(&providers.llm_api_key, base_url, &providers.llm_model)
        ),
        "anthropic" => try_rig_provider!(
            "Anthropic",
            agent_kit::AnthropicProvider::new(
                &providers.llm_api_key,
                base_url,
                &providers.llm_model,
            )
        ),
        "deepseek" => try_rig_provider!(
            "DeepSeek",
            agent_kit::DeepSeekProvider::new(
                &providers.llm_api_key,
                base_url,
                &providers.llm_model,
            )
        ),
        "gemini" => try_rig_provider!(
            "Gemini",
            agent_kit::GeminiProvider::new(&providers.llm_api_key, base_url, &providers.llm_model,)
        ),
        "openrouter" => {
            let or_url = agent_kit::providers::openai_compat::normalize_openrouter_url(Some(
                providers.llm_url.as_str(),
            ));
            try_rig_provider!(
                "OpenRouter",
                agent_kit::OpenAiCompatProvider::new(
                    &providers.llm_api_key,
                    Some(&or_url),
                    &providers.llm_model,
                )
            )
        }
        // Together AI uses a dedicated provider that suppresses reasoning tokens
        // (reasoning_effort: "none", reasoning.enabled: false) for voice compatibility.
        // Thinking tokens like <think>…</think> must never reach TTS.
        "together" => try_rig_provider!(
            "Together AI",
            agent_kit::TogetherProvider::new(&providers.llm_api_key, &providers.llm_model)
        ),
        "fireworks" => try_rig_provider!(
            "Fireworks AI",
            agent_kit::FireworksProvider::new(&providers.llm_api_key, &providers.llm_model)
        ),
        _ => {} // Fall through to OpenAI-compat default
    }

    // Default: OpenAI-compatible via rig's Chat Completions client with custom base_url.
    // Uses /v1/chat/completions (not /responses) — works with Ollama, vLLM, LiteLLM, etc.
    let compat_url = if providers.llm_url.is_empty() {
        None // rig-core's openai client defaults to api.openai.com
    } else {
        Some(providers.llm_url.as_str())
    };

    match agent_kit::OpenAiCompatProvider::new(
        &providers.llm_api_key,
        compat_url,
        &providers.llm_model,
    ) {
        Ok(p) => Box::new(p),
        Err(e) => {
            error!(
                "Failed to create OpenAI-compat provider (url={}, model={}): {}",
                providers.llm_url, providers.llm_model, e
            );
            panic!("Cannot create any LLM provider — aborting session");
        }
    }
}

/// Shared session loop — transport-agnostic.
///
/// Accepts any `TransportHandle` (WebSocket, WebRTC, etc.), starts the
/// `VoiceSession`, waits for completion, and cleans up.
///
/// Secrets are pre-resolved by voice-server and passed in. If `secrets` is
/// `None` (e.g. PyO3 path without vault), an empty map is used.
///
/// `refresh_handle` is an optional background task that periodically refreshes
/// the secrets from the vault. It is aborted when the session ends.
///
/// Returns a `SessionSummary` with metadata about the completed session.
async fn run_session_with_transport(
    session_id: String,
    mut config: SessionConfig,
    transport: TransportHandle,
    providers: &ProviderConfig,
    secrets: Option<SharedSecretMap>,
    refresh_handle: Option<tokio::task::JoinHandle<()>>,
    tracer: Tracer,
) -> SessionSummary {
    let agent_id = config.agent_id.clone();

    let secrets = secrets.unwrap_or_else(empty_shared_secrets);

    // Propagate STT/TTS provider names and model names for observability labels.
    if !providers.stt_provider.is_empty() {
        config.stt_provider = providers.stt_provider.clone();
    }
    if !providers.stt_model.is_empty() {
        config.stt_model = providers.stt_model.clone();
    }
    if !providers.tts_provider.is_empty() {
        config.tts_provider = providers.tts_provider.clone();
    }
    if !providers.tts_model.is_empty() {
        config.tts_model = providers.tts_model.clone();
    }
    // Populate tts_voice_id from the agent-level voice_id so TTS providers
    // that need the full voice name (Deepgram: voice IS the model param) can use it.
    let mut providers_with_voice = providers.clone();
    if providers_with_voice.tts_voice_id.is_empty() && !config.voice_id.is_empty() {
        providers_with_voice.tts_voice_id = config.voice_id.clone();
    }
    let providers = &providers_with_voice;

    // Set STT timeout based on provider's known tail latency characteristics.
    // Overrides the generic 1000ms default with a per-provider measured P99.
    config.stt_p99_latency_ms =
        crate::providers::stt::default_stt_p99_latency_ms(&config.stt_provider);

    let llm: Box<dyn LlmProvider> = build_llm_provider(providers);

    // Config is finalized earlier, native_multimodal should be populated from agent_graph down in session.rs or python.rs before start.

    // Build typed STT provider config.
    // Language normalization (dialect → base ISO-639-1) is handled inside
    // build_stt_provider() per-provider, so pass the raw configured language here.
    let stt_config = SttProviderConfig {
        provider: providers.stt_provider.clone(),
        base_url: providers.stt_url.clone(),
        api_key: providers.stt_api_key.clone(),
        language: config.language.clone(),
        model: providers.stt_model.clone(),
    };

    // Build typed TTS provider config
    let tts_config = TtsProviderConfig {
        provider: providers.tts_provider.clone(),
        base_url: providers.tts_url.clone(),
        api_key: providers.tts_api_key.clone(),
        model: providers.tts_model.clone(),
        output_sample_rate: config.output_sample_rate,
        language: config.language.clone(),
        voice_id: providers.tts_voice_id.clone(),
    };

    // Warn early if the TTS model is known to not support the configured language.
    // This catches misconfiguration before it reaches the provider and produces
    // silent failures or garbled audio.
    if !config.language.is_empty() && config.language != "en" {
        use crate::language_config::tts_model_supports_language;
        if !tts_model_supports_language(
            &providers.tts_provider,
            &providers.tts_model,
            &config.language,
        ) {
            tracing::warn!(
                provider = %providers.tts_provider,
                model    = %providers.tts_model,
                language = %config.language,
                "[session] TTS model does not support this language — \
                 audio output may be incorrect. \
                 Switch to a multilingual model (e.g. sonic-2, eleven_flash_v2_5)."
            );
        }
    }

    let started_at = std::time::Instant::now();

    // Grab the event bus sender before tracer is moved into the session.
    // Used to emit a best-effort error if session startup fails.
    let error_bus = tracer.subscribe_sender();

    let mut session = match VoiceSession::start(
        session_id.clone(),
        config,
        secrets,
        llm,
        stt_config,
        tts_config,
        tracer,
        transport,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let message = e.to_string();
            // Best-effort: if the WS forwarder is already subscribed it will
            // receive this; otherwise the event is silently dropped.
            let _ = error_bus.send(Event::Error {
                source: "session".into(),
                message: message.clone(),
            });
            error!("Failed to start voice session: {}", message);
            // Abort the refresh task if session failed to start
            if let Some(h) = refresh_handle {
                h.abort();
            }
            return SessionSummary {
                session_id,
                agent_id,
                duration_secs: 0,
            };
        }
    };

    session.wait_for_completion().await;
    session.close().await;

    // Stop the background secret refresh task
    if let Some(h) = refresh_handle {
        h.abort();
    }

    let duration_secs = started_at.elapsed().as_secs() as u32;

    // Recording writes are handled by the recording subscriber's on_complete
    // callback (voice-server/src/recording.rs), not here. The subscriber
    // writes files and DB records asynchronously after SessionEnded.
    info!("Voice session ended (duration={}s)", duration_secs);

    SessionSummary {
        session_id,
        agent_id,
        duration_secs,
    }
}

// ── WebRTC Handlers ─────────────────────────────────────────────

#[cfg(feature = "webrtc")]
mod rtc_handlers {
    use super::*;

    /// JSON body for the WebRTC offer endpoint.
    #[derive(Deserialize)]
    pub(super) struct RtcOfferRequest {
        /// The SDP offer from the browser.
        pub offer: serde_json::Value,
        /// Optional inline session config (same fields as session.start).
        #[serde(default)]
        pub system_prompt: Option<String>,
        #[serde(default)]
        pub voice_id: Option<String>,
        #[serde(default)]
        pub language: Option<String>,
        #[serde(default)]
        pub greeting: Option<String>,
        #[serde(default)]
        pub agent_id: Option<String>,
    }

    impl RtcOfferRequest {
        /// Apply the inline fields to a `SessionConfig`.
        pub fn apply_to(&self, config: &mut SessionConfig) {
            if let Some(sp) = &self.system_prompt {
                config.system_prompt = sp.clone();
            }
            if let Some(v) = &self.voice_id {
                config.voice_id = v.clone();
            }
            if let Some(l) = &self.language {
                config.language = l.clone();
            }
            if let Some(g) = &self.greeting {
                config.greeting = Some(g.clone());
            }
            if let Some(a) = &self.agent_id {
                config.agent_id = a.clone();
            }
        }
    }
}

/// Returns ICE server configuration for the browser's `RTCPeerConnection`.
///
/// Requires a valid session token to prevent unauthorized TURN credential harvesting.
/// The browser calls this before creating its peer connection so it can
/// include STUN/TURN servers. Credentials are ephemeral and per-session.
#[cfg(feature = "webrtc")]
async fn rtc_get_ice_servers(
    Query(params): Query<TokenQuery>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    // Validate token if a secret is configured
    if !state.auth_secret_key.is_empty() {
        let Some(sid) = params.session_id.as_deref().filter(|s| !s.is_empty()) else {
            warn!("Missing session_id query param for /rtc/ice-servers (auth required)");
            return (
                StatusCode::BAD_REQUEST,
                "Missing session_id query parameter",
            )
                .into_response();
        };
        if !state.validate_token(sid, &params.token) {
            warn!("Invalid session token for /rtc/ice-servers");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    match state.ice_provider.resolve().await {
        Ok(servers) => {
            axum::response::Json(serde_json::json!({ "iceServers": servers })).into_response()
        }
        Err(e) => {
            warn!("ICE provider failed, returning STUN-only fallback: {}", e);
            axum::response::Json(serde_json::json!({
                "iceServers": [{
                    "urls": ["stun:stun.cloudflare.com:3478"]
                }]
            }))
            .into_response()
        }
    }
}

/// Handler for pre-registered WebRTC sessions: `POST /rtc/offer/{session_id}?token=...`.
#[cfg(feature = "webrtc")]
async fn rtc_offer_registered(
    Path(session_id): Path<String>,
    Query(params): Query<TokenQuery>,
    State(state): State<ServerState>,
    Json(body): Json<rtc_handlers::RtcOfferRequest>,
) -> impl IntoResponse {
    // Validate session token
    if !state.validate_token(&session_id, &params.token) {
        warn!("Invalid session token for RTC session {}", session_id);
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Peek at the config without removing it yet — removal is deferred until
    // after WebRtcConnection::from_offer succeeds, so the WS handler can still
    // fall back to pure-WS mode if the RTC setup fails.
    let registered = match state.sessions.get(&session_id) {
        Some(entry) => entry.clone(),
        None => {
            warn!("No registered session for id={}", session_id);
            return axum::response::Json(serde_json::json!({
                "error": "session not found"
            }))
            .into_response();
        }
    };

    info!("Starting pre-registered WebRTC session: {}", session_id);

    // Note: we only `.get()` the session here (not `.remove()`), so the WS handler
    // can still fall back to pure-WS mode if WebRTC setup fails.
    let reg_tracer = registered.take_tracer();
    let secrets = registered.secrets.clone();
    let refresh_handle = registered.take_refresh_handle();

    rtc_create_session(
        session_id,
        body,
        registered.config.clone(),
        &registered.providers,
        secrets,
        refresh_handle,
        reg_tracer,
        state,
    )
    .await
    .into_response()
}

// Inline WebRTC handler removed for security — all sessions must be
// pre-registered by the embedder before connecting.

/// Handler for WebRTC Trickle ICE candidates: `POST /rtc/candidate/{session_id}?token=...`.
#[cfg(feature = "webrtc")]
async fn rtc_candidate_registered(
    axum::extract::Path(session_id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<TokenQuery>,
    axum::extract::State(state): axum::extract::State<ServerState>,
    axum::extract::Json(body): axum::extract::Json<RtcCandidateRequest>,
) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;
    if !state.validate_token(&session_id, &params.token) {
        warn!("Invalid session token for RTC candidate {}", session_id);
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Some(session) = state.hybrid_sessions.get(&session_id) {
        if session
            .control_tx
            .send(voice_transport::TransportCommand::AddIceCandidate(
                body.candidate,
            ))
            .is_err()
        {
            warn!(
                "Session {} control channel closed when sending ICE candidate",
                session_id
            );
            return StatusCode::GONE.into_response();
        }
        StatusCode::OK.into_response()
    } else {
        warn!("Session {} not found for ICE candidate", session_id);
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Create a WebRTC session from an SDP offer.
///
/// 1. Applies inline config from the POST body
/// 2. Creates a `WebRtcConnection` (str0m + UDP socket)
/// 3. Produces a `TransportHandle`
/// 4. Starts the `VoiceSession` in a background task
/// 5. Returns the SDP answer as JSON
#[cfg(feature = "webrtc")]
async fn rtc_create_session(
    session_id: String,
    body: rtc_handlers::RtcOfferRequest,
    mut config: SessionConfig,
    providers: &ProviderConfig,
    secrets: Option<SharedSecretMap>,
    refresh_handle: Option<tokio::task::JoinHandle<()>>,
    pre_tracer: Option<Tracer>,
    state: ServerState,
) -> axum::response::Json<serde_json::Value> {
    // Apply inline config from the POST body
    body.apply_to(&mut config);

    // WebRTC uses Opus at 48kHz — force matching output rate
    config.output_sample_rate = 48_000;

    // Create the WebRTC connection from the SDP offer
    let (connection, answer) =
        match WebRtcConnection::from_offer(body.offer, &state.ice_config.stun_server).await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to create WebRTC connection: {}", e);
                return axum::response::Json(serde_json::json!({
                    "error": format!("WebRTC setup failed: {}", e)
                }));
            }
        };

    // Connection succeeded — now consume the session config so the WS handler
    // can't also start a full session with it (it'll enter hybrid mode instead).
    state.sessions.remove(&session_id);

    // Use the pre-registered Tracer if available (may have recording subscribers),
    // otherwise create a fresh one.
    let tracer = pre_tracer.unwrap_or_default();

    #[cfg(feature = "otel")]
    voice_trace::sinks::otel::spawn_otel_subscriber(&tracer);

    // Build the transport handle from the connection.
    // Must happen before inserting into state.hybrid_sessions so we can clone control_tx.
    let transport = WebRtcTransport::from_connection(connection, &tracer);

    // Register the hybrid session so the WS handler can:
    //   (a) subscribe to UI events via event_tx, and
    //   (b) send TransportCommand::Close via control_tx when the WS closes.
    state.hybrid_sessions.insert(
        session_id.clone(),
        HybridSession {
            event_tx: tracer.subscribe_sender(),
            control_tx: transport.control_tx.clone(),
        },
    );

    // Start the session in a background task
    // (the HTTP response must return the SDP answer immediately)
    let providers = providers.clone();

    tokio::spawn(async move {
        let mut transport = transport;
        // Wait for ICE to connect before starting the session.
        let connected = {
            let rx = &mut transport.control_rx;
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(20));
            tokio::pin!(deadline);
            let mut ok = false;
            loop {
                tokio::select! {
                    biased;
                    evt = rx.recv() => {
                        match evt {
                            Some(TransportEvent::Connected) => { ok = true; break; }
                            Some(_) => continue,
                            None => break,
                        }
                    }
                    _ = &mut deadline => {
                        error!("WebRTC ICE connection timed out (20s)");
                        break;
                    }
                }
            }
            ok
        };

        if !connected {
            error!("WebRTC connection failed — aborting session");
            state.hybrid_sessions.remove(&session_id);
            return;
        }

        info!("WebRTC ICE connected — starting voice session");

        run_session_with_transport(
            session_id.clone(),
            config,
            transport,
            &providers,
            secrets,
            refresh_handle,
            tracer,
        )
        .await;

        // Clean up the hybrid session entry when the session ends naturally.
        state.hybrid_sessions.remove(&session_id);
    });

    // Return the SDP answer to the browser
    axum::response::Json(serde_json::json!({
        "answer": answer
    }))
}

// ── Telephony Handlers ──────────────────────────────────────────

#[cfg(feature = "telephony")]
mod telephony_handlers {
    use super::*;

    /// Query parameters for the telephony webhook URL.
    ///
    /// Users configure these in their Twilio/Telnyx webhook settings:
    /// `wss://server/telephony/twilio?account_sid=AC...&auth_token=...`
    #[derive(Debug, Deserialize)]
    pub(super) struct TelephonyQueryParams {
        /// Twilio: account SID. Telnyx: unused.
        #[serde(default)]
        pub account_sid: Option<String>,
        /// Twilio: auth_token. Telnyx: api_key.
        #[serde(default)]
        pub api_key: Option<String>,
        /// Twilio alias for api_key (Twilio convention uses `auth_token`).
        #[serde(default)]
        pub auth_token: Option<String>,
        /// Pre-registered session ID (optional).
        #[serde(default)]
        pub session_id: Option<String>,
        /// Inbound encoding override (default: PCMU).
        #[serde(default)]
        pub inbound_encoding: Option<String>,
        /// Outbound encoding override (default: PCMU).
        #[serde(default)]
        pub outbound_encoding: Option<String>,
        /// HMAC session token for authentication.
        #[serde(default)]
        pub token: Option<String>,
    }

    impl TelephonyQueryParams {
        /// The API key — tries `api_key` first, falls back to `auth_token`.
        pub fn resolved_api_key(&self) -> Option<String> {
            self.api_key.clone().or_else(|| self.auth_token.clone())
        }

        /// Parse encoding string to enum.
        pub fn parse_encoding(s: &Option<String>) -> TelephonyEncoding {
            match s.as_deref() {
                Some("PCMA") | Some("pcma") | Some("alaw") => TelephonyEncoding::Pcma,
                _ => TelephonyEncoding::Pcmu,
            }
        }
    }
}

#[cfg(feature = "telephony")]
async fn telephony_twilio_handler(
    ws: WebSocketUpgrade,
    axum::extract::Query(params): axum::extract::Query<telephony_handlers::TelephonyQueryParams>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    // Validate session token
    if let Some(ref sid) = params.session_id {
        let token = params.token.as_deref().unwrap_or("");
        if !state.validate_token(sid, token) {
            warn!("Invalid session token for telephony/twilio session {}", sid);
            return StatusCode::UNAUTHORIZED.into_response();
        }
    } else if !state.auth_secret_key.is_empty() {
        // Secret is configured but no session_id provided — reject
        warn!("Telephony/twilio request missing session_id (auth required)");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| {
        handle_telephony_session(socket, TelephonyProviderKind::Twilio, params, state)
    })
    .into_response()
}

#[cfg(all(feature = "telephony", feature = "telnyx"))]
async fn telephony_telnyx_handler(
    ws: WebSocketUpgrade,
    axum::extract::Query(params): axum::extract::Query<telephony_handlers::TelephonyQueryParams>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    // Validate session token
    if let Some(ref sid) = params.session_id {
        let token = params.token.as_deref().unwrap_or("");
        if !state.validate_token(sid, token) {
            warn!("Invalid session token for telephony/telnyx session {}", sid);
            return StatusCode::UNAUTHORIZED.into_response();
        }
    } else if !state.auth_secret_key.is_empty() {
        // Secret is configured but no session_id provided — reject
        warn!("Telephony/telnyx request missing session_id (auth required)");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| {
        handle_telephony_session(socket, TelephonyProviderKind::Telnyx, params, state)
    })
    .into_response()
}

/// Shared telephony session handler.
///
/// 1. Reads initial WS messages to extract stream/call IDs
/// 2. Builds a `TelephonyConfig` from query params + initial messages
/// 3. Creates a `TelephonyTransport` → `TransportHandle`
/// 4. Forces 8 kHz sample rates
/// 5. Runs the voice session
#[cfg(feature = "telephony")]
async fn handle_telephony_session(
    socket: WebSocket,
    kind: TelephonyProviderKind,
    params: telephony_handlers::TelephonyQueryParams,
    state: ServerState,
) {
    use telephony_handlers::TelephonyQueryParams;

    let provider_name = match kind {
        TelephonyProviderKind::Twilio => "Twilio",
        #[cfg(feature = "telnyx")]
        TelephonyProviderKind::Telnyx => "Telnyx",
    };
    info!(
        "[telephony] New {} connection (session_id={:?})",
        provider_name, params.session_id
    );

    // Peek at the registered session to get per-session telephony credentials
    // (populated at inbound webhook time from phone_numbers.provider_credentials_encrypted).
    // This must happen before TelephonyTransport::accept() which needs the credentials.
    let session_telephony_creds: Option<TelephonyCredentials> =
        params.session_id.as_ref().and_then(|sid| {
            state
                .sessions
                .get(sid)
                .and_then(|reg| reg.telephony_creds.clone())
        });

    // Resolve credentials: query params → session-level → ServerState global fallback.
    // Build a concrete TelephonyCredentials from the provider kind + resolved values.
    let resolved_credentials = match kind {
        TelephonyProviderKind::Twilio => {
            let auth_token = params
                .resolved_api_key()
                .or_else(|| {
                    session_telephony_creds
                        .as_ref()
                        .map(|c| c.twilio_auth_token.clone())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    let t = &state.telephony_creds.twilio_auth_token;
                    if t.is_empty() {
                        None
                    } else {
                        Some(t.clone())
                    }
                })
                .unwrap_or_default();
            let account_sid = params
                .account_sid
                .clone()
                .or_else(|| {
                    session_telephony_creds
                        .as_ref()
                        .map(|c| c.twilio_account_sid.clone())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    let s = &state.telephony_creds.twilio_account_sid;
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.clone())
                    }
                })
                .unwrap_or_default();
            TelephonyTransportCredentials::Twilio {
                account_sid,
                auth_token,
            }
        }
        #[cfg(feature = "telnyx")]
        TelephonyProviderKind::Telnyx => {
            let api_key = params
                .resolved_api_key()
                .or_else(|| {
                    session_telephony_creds
                        .as_ref()
                        .map(|c| c.telnyx_api_key.clone())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    let k = &state.telephony_creds.telnyx_api_key;
                    if k.is_empty() {
                        None
                    } else {
                        Some(k.clone())
                    }
                })
                .unwrap_or_default();
            let connection_id = session_telephony_creds
                .as_ref()
                .map(|c| c.telnyx_connection_id.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| state.telephony_creds.telnyx_connection_id.clone());
            TelephonyTransportCredentials::Telnyx {
                api_key,
                connection_id,
            }
        }
    };

    // Resolve webhook base URL from registered session (populated by voice-server from DB).
    // Resolve from_number (the called/To number) for conference-based supervised transfer.
    // We peek the session here before the remove later — the remove happens after transport init
    // so the peek is safe; both fields are Copy-friendly (Option<String>).
    let resolved_public_url = params
        .session_id
        .as_ref()
        .and_then(|sid| state.sessions.get(sid))
        .and_then(|reg| reg.webhook_base_url.clone())
        .filter(|u| !u.is_empty());

    let resolved_from_number = params
        .session_id
        .as_ref()
        .and_then(|sid| state.sessions.get(sid))
        .and_then(|reg| reg.from_number.clone())
        .filter(|s| !s.is_empty());

    // Build the telephony config from resolved credentials.
    let config = TelephonyConfig {
        credentials: resolved_credentials,
        public_url: resolved_public_url,
        from_number: resolved_from_number,
        stream_id: std::sync::Mutex::new(String::new()), // populated by recv loop from "start" event
        call_id: std::sync::Mutex::new(None), // populated by recv loop from "start" event
        inbound_encoding: TelephonyQueryParams::parse_encoding(&params.inbound_encoding),
        outbound_encoding: TelephonyQueryParams::parse_encoding(&params.outbound_encoding),
        auto_hang_up: true,
        sample_rate: 8000,
    };

    // Start the transport (spawns recv/forward loops)
    // IMPORTANT: We need the transport forward loop to subscribe to the *same*
    // event bus the reactor will emit on. The reactor uses `session_tracer`
    // which may be a pre-created reg_tracer from voice-server (with recording
    // subscribers already attached).  We solve this by:
    //   1. Creating a local_tracer now (used if no reg_tracer exists).
    //   2. Giving the transport a oneshot that delivers the broadcast::Sender
    //      from whichever tracer wins, so the forward loop can subscribe to
    //      the correct bus AFTER session resolution.
    let local_tracer = Tracer::new();

    #[cfg(feature = "otel")]
    voice_trace::sinks::otel::spawn_otel_subscriber(&local_tracer);

    let (bus_tx_oneshot_tx, bus_tx_oneshot_rx) = tokio::sync::oneshot::channel();
    let (transport, session_id_rx) = TelephonyTransport::accept(socket, config, bus_tx_oneshot_rx);

    // Resolve session_id
    let effective_session_id = if let Some(sid) = params.session_id.clone() {
        sid
    } else {
        // Wait for the provider "start" message
        match tokio::time::timeout(std::time::Duration::from_secs(10), session_id_rx).await {
            Ok(Ok((Some(sid), _cid))) => {
                info!("[telephony] Got session_id from customParameters: {}", sid);
                sid
            }
            Ok(Ok((None, _cid))) => {
                warn!("[telephony] No session_id from query params or customParameters, using inline session");
                uuid::Uuid::new_v4().to_string()
            }
            _ => {
                warn!("[telephony] Timed out waiting for start message");
                uuid::Uuid::new_v4().to_string()
            }
        }
    };

    // Look up pre-registered session or use defaults
    let (
        session_id,
        mut session_config,
        providers,
        secrets,
        refresh_handle,
        reg_tracer,
        _reg_from_number,
    ) = {
        if let Some((_, reg)) = state.sessions.remove(&effective_session_id) {
            // Extract the Tracer out of the Arc<Mutex<Option<...>>> wrapper.
            let reg_tracer = reg
                .tracer
                .as_ref()
                .and_then(|arc| arc.lock().ok().and_then(|mut guard| guard.take()));
            let secrets = reg.secrets.clone();
            let refresh_handle = reg.take_refresh_handle();
            let from_number = reg.from_number.clone();
            (
                effective_session_id.clone(),
                reg.config.clone(),
                reg.providers.clone(),
                secrets,
                refresh_handle,
                reg_tracer,
                from_number,
            )
        } else {
            warn!(
                "[telephony] No registered session for id={}",
                effective_session_id
            );
            (
                effective_session_id.clone(),
                SessionConfig::default(),
                state.default_providers.clone(),
                None, // No secrets for unregistered telephony sessions
                None, // No refresh handle
                None, // No pre-created Tracer
                None, // No from_number for unregistered sessions
            )
        }
    };

    // Force 8 kHz for telephony
    session_config.input_sample_rate = 8000;
    session_config.output_sample_rate = 8000;

    info!(
        "[telephony] Session {} starting ({:?})",
        session_id,
        params.session_id.as_deref().unwrap_or("customParam")
    );

    // session_tracer is the authoritative event bus for this session.
    // The transport forward loop will subscribe to it via the oneshot.
    let session_tracer = reg_tracer.unwrap_or(local_tracer);

    // Deliver the broadcast::Sender to the forward loop so it subscribes
    // to the correct event bus (the one the reactor will emit on).
    let _ = bus_tx_oneshot_tx.send(session_tracer.subscribe_sender());

    let summary = run_session_with_transport(
        session_id.clone(),
        session_config,
        transport,
        &providers,
        secrets,
        refresh_handle,
        session_tracer,
    )
    .await;

    info!(
        "[telephony] Session {} ended (agent={}, duration={}s)",
        summary.session_id, summary.agent_id, summary.duration_secs
    );
}

/// Public entry point for voice-server's path-based Twilio WebSocket handler.
///
/// Used when session_id + token were passed as **path segments** (not query params)
/// to avoid Cloudflare tunnels stripping query parameters from WebSocket upgrades.
/// Authentication has already been validated by the caller (voice-server).
#[cfg(feature = "telephony")]
pub async fn telephony_handle_twilio_socket(
    socket: axum::extract::ws::WebSocket,
    session_id: String,
    telephony_creds: Option<TelephonyCredentials>,
    state: ServerState,
) {
    use telephony_handlers::TelephonyQueryParams;
    // Build a minimal params with the pre-validated session_id.
    // Credentials come from the registered session (populated at inbound webhook time).
    let params = TelephonyQueryParams {
        account_sid: telephony_creds
            .as_ref()
            .map(|c| c.twilio_account_sid.clone()),
        api_key: None,
        auth_token: telephony_creds
            .as_ref()
            .map(|c| c.twilio_auth_token.clone()),
        session_id: Some(session_id),
        inbound_encoding: None,
        outbound_encoding: None,
        token: None, // already validated by voice-server before calling us
    };
    handle_telephony_session(socket, TelephonyProviderKind::Twilio, params, state).await;
}

/// Public entry point for voice-server's path-based Telnyx WebSocket handler.
#[cfg(all(feature = "telephony", feature = "telnyx"))]
pub async fn telephony_handle_telnyx_socket(
    socket: axum::extract::ws::WebSocket,
    session_id: String,
    telephony_creds: Option<TelephonyCredentials>,
    state: ServerState,
) {
    use telephony_handlers::TelephonyQueryParams;
    // Telnyx only needs session_id. api_key is populated from telephony_creds.
    let params = TelephonyQueryParams {
        account_sid: None,
        api_key: telephony_creds.map(|c| c.telnyx_api_key),
        auth_token: None,
        session_id: Some(session_id),
        inbound_encoding: None,
        outbound_encoding: None,
        token: None,
    };
    handle_telephony_session(socket, TelephonyProviderKind::Telnyx, params, state).await;
}
