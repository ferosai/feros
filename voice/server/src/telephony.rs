//! Telephony webhook handlers — Twilio and Telnyx (HTTP only).
//!
//! At number-assignment time Python embeds the agent_id in the webhook URL:
//!   https://voice.domain.com/telephony/twilio/incoming/{agent_id}
//!   https://voice.domain.com/telephony/telnyx/inbound/{agent_id}
//!
//! When Twilio/Telnyx POSTs here, the agent_id is in the path — no DB
//! lookup on the phone_numbers table needed.
//!
//! The actual WebSocket media-stream endpoints (/telephony/twilio GET,
//! /telephony/telnyx GET) are served by voice-engine's build_router() and
//! are NOT duplicated here.

use std::sync::Arc;
use std::time::Instant;

use crate::cred::{EncryptionEngine, VaultHandle};
use crate::observability;
use axum::{
    extract::{ws::WebSocket, Form, Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use voice_engine::{
    server::{ProviderConfig, RegisteredSession, ServerState, TelephonyCredentials},
    session::SessionConfig,
};

use crate::db::{self, AgentConfig};
use crate::utils::to_ws_url;

// ── Shared app state ─────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub engine: ServerState,
    pub pool: PgPool,
    /// Public base URL of this server — embedded in TwiML/TeXML WebSocket URLs.
    pub public_url: String,
    /// Encryption engine for decrypting stored credentials.
    pub encryption: Arc<EncryptionEngine>,
    /// Vault handle — None if vault is disabled (no `integrations` feature or startup failure).
    pub vault: Option<Arc<VaultHandle>>,
    /// Recording destination URI (e.g. `file://./recordings` or `s3://bucket/prefix`).
    /// Injected into every session's RecordingConfig.output_uri so voice-trace knows
    /// where to write and voice-server recording.rs can derive the public URL.
    pub recording_output_uri: String,
    /// Shared integration provider registry (used for pre-session token verification).
    #[cfg(feature = "integrations")]
    pub registry: Arc<integrations::ProviderRegistry>,
    /// Global observability adapter settings.
    pub observability: db::ObservabilitySettings,
}

impl AppState {
    /// Create a scoped vault token for the given agent, or return None if
    /// no vault is running.
    pub fn vault_token_for(&self, agent_uuid: Uuid) -> Option<String> {
        self.vault
            .as_ref()
            .map(|v| v.create_scoped_token(agent_uuid, crate::secrets::VAULT_TOKEN_TTL))
    }
}

// ── Router (HTTP webhooks only) ───────────────────────────────────

pub fn http_router(state: AppState) -> Router {
    let router = Router::new()
        .route(
            "/telephony/twilio/incoming/{agent_id}",
            post(twilio_incoming),
        )
        .route("/telephony/twilio/status", post(twilio_status))
        // WebSocket media stream — session_id and token as path params to avoid
        // Cloudflare stripping query parameters from WebSocket upgrade requests.
        .route(
            "/telephony/twilio/stream/{session_id}/{token}",
            get(twilio_ws_stream),
        );

    #[cfg(feature = "telnyx")]
    let router = router
        .route("/telephony/telnyx/inbound/{agent_id}", post(telnyx_inbound))
        .route(
            "/telephony/telnyx/stream/{session_id}/{token}",
            get(telnyx_ws_stream),
        );

    router.with_state(state)
}

/// `POST /telephony/twilio/status`
async fn twilio_status() -> impl IntoResponse {
    StatusCode::OK
}

// ── Twilio ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TwilioIncoming {
    #[allow(dead_code)]
    call_sid: String,
    /// The phone number that was called (E.164). Used to look up per-number credentials.
    to: String,
}

/// `POST /telephony/twilio/incoming/{agent_id}`
async fn twilio_incoming(
    Path(agent_id): Path<String>,
    State(state): State<AppState>,
    Form(params): Form<TwilioIncoming>,
) -> impl IntoResponse {
    info!(agent_id, call_sid = %params.call_sid, "Twilio incoming call");

    let Ok(agent_uuid) = agent_id.parse::<Uuid>() else {
        warn!("Invalid agent_id in Twilio webhook URL: {agent_id}");
        return twiml_reject().into_response();
    };

    let Some(agent) = db::agent_by_id(&state.pool, &state.encryption, agent_uuid).await else {
        warn!("Agent {agent_id} not found or has no active version");
        return twiml_reject().into_response();
    };

    let session_id = Uuid::new_v4().to_string();
    let token = state.engine.sign_token(&session_id);
    let vault_token = state.vault_token_for(agent_uuid);

    // Look up per-number credentials from phone_numbers.provider_credentials_encrypted
    let telephony_creds =
        crate::db::phone_number_credentials(&state.pool, &state.encryption, &params.to).await;
    if telephony_creds.is_none() {
        warn!(phone_number = %params.to, "No credentials found for phone number — hangup will fail");
    }

    register_session(
        &state,
        &session_id,
        &agent,
        vault_token,
        telephony_creds,
        "inbound",
        Some(params.to.clone()),
    )
    .await;

    // Path-based WS URL: no query params, no & escaping, no Cloudflare-stripping.
    let ws_url = format!(
        "{}/telephony/twilio/stream/{}/{}",
        to_ws_url(&state.public_url),
        session_id,
        token
    );

    let twiml = twiml_stream(&ws_url);

    info!(
        ws_url,
        session_id, agent_id, "Twilio session registered — returning TwiML"
    );
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/xml")],
        twiml,
    )
        .into_response()
}

/// `GET /telephony/twilio/stream/{session_id}/{token}`
///
/// WebSocket media stream endpoint. Using path params instead of query params
/// because Cloudflare tunnels strip query params from WebSocket upgrade requests.
async fn twilio_ws_stream(
    ws: WebSocketUpgrade,
    Path((session_id, token)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    info!(%session_id, "Twilio WebSocket stream handshake");

    if !state.engine.validate_token(&session_id, &token) {
        warn!(%session_id, "Twilio WS: invalid session token");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(reg) = state.engine.sessions.get(&session_id) else {
        warn!(%session_id, "Twilio WS: no registered session");
        return StatusCode::NOT_FOUND.into_response();
    };
    let telephony_creds = reg.telephony_creds.clone();
    drop(reg);

    ws.on_upgrade(move |socket| {
        handle_twilio_ws_stream(socket, session_id, telephony_creds, state.engine)
    })
}

async fn handle_twilio_ws_stream(
    socket: WebSocket,
    session_id: String,
    telephony_creds: Option<TelephonyCredentials>,
    engine: ServerState,
) {
    info!(%session_id, "Twilio WebSocket upgraded — handing off to voice-engine");
    voice_engine::server::telephony_handle_twilio_socket(
        socket,
        session_id,
        telephony_creds,
        engine,
    )
    .await;
}

/// Escape a string for embedding as an XML attribute value.
/// Replaces `&` → `&amp;`, `<` → `&lt;`, `"` → `&quot;`.
fn xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Build a TwiML `<Stream>` document for the given WebSocket URL.
fn twiml_stream(ws_url: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="{}" />
  </Connect>
</Response>"#,
        xml_attr(ws_url)
    )
}

#[cfg(feature = "telnyx")]
async fn telnyx_ws_stream(
    ws: WebSocketUpgrade,
    Path((session_id, token)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Validate session token
    if !state.engine.validate_token(&session_id, &token) {
        warn!("Invalid session token for telnyx stream {}", session_id);
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Pass the socket to voice-engine
    ws.on_upgrade(move |socket| async move {
        // Resolve telephony credentials from the registered session
        let telephony_creds = state
            .engine
            .sessions
            .get(&session_id)
            .and_then(|reg| reg.telephony_creds.clone());

        voice_engine::server::telephony_handle_telnyx_socket(
            socket,
            session_id,
            telephony_creds,
            state.engine,
        )
        .await;
    })
    .into_response()
}

// ── Telnyx ──────────────────────────────────────────────────────

#[cfg(feature = "telnyx")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TelnyxTeXMLInbound {
    call_sid: String,
    /// The phone number that was called (E.164). Used to look up per-number credentials.
    to: Option<String>,
    #[allow(dead_code)]
    from: Option<String>,
}

/// `POST /telephony/telnyx/inbound/{agent_id}`
#[cfg(feature = "telnyx")]
async fn telnyx_inbound(
    Path(agent_id): Path<String>,
    State(state): State<AppState>,
    Form(params): Form<TelnyxTeXMLInbound>,
) -> impl IntoResponse {
    let call_id = &params.call_sid;
    info!(agent_id, call_id, "Telnyx inbound call");

    let Ok(agent_uuid) = agent_id.parse::<Uuid>() else {
        warn!("Invalid agent_id in Telnyx webhook URL: {agent_id}");
        return StatusCode::OK.into_response();
    };

    let Some(agent) = db::agent_by_id(&state.pool, &state.encryption, agent_uuid).await else {
        warn!("Agent {agent_id} not found or has no active version");
        return StatusCode::OK.into_response();
    };

    let session_id = Uuid::new_v4().to_string();
    let token = state.engine.sign_token(&session_id);
    let vault_token = state.vault_token_for(agent_uuid);

    // Look up per-number credentials from phone_numbers.provider_credentials_encrypted
    let telephony_creds = if let Some(ref to_number) = params.to {
        let creds =
            crate::db::phone_number_credentials(&state.pool, &state.encryption, to_number).await;
        if creds.is_none() {
            warn!(phone_number = %to_number, "No credentials found for phone number — hangup will fail");
        }
        creds
    } else {
        warn!(
            agent_id,
            "Telnyx TeXML webhook missing 'To' field — cannot look up credentials"
        );
        None
    };

    register_session(
        &state,
        &session_id,
        &agent,
        vault_token,
        telephony_creds,
        "inbound",
        params.to.clone(),
    )
    .await;

    // Path-based WS URL: no query params, no & escaping, no Cloudflare-stripping.
    let ws_url = format!(
        "{}/telephony/telnyx/stream/{}/{}",
        to_ws_url(&state.public_url),
        session_id,
        token
    );

    let texml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="{ws_url}" />
  </Connect>
</Response>"#
    );

    info!(session_id, agent_id, "Telnyx session registered");
    (StatusCode::OK, [("Content-Type", "application/xml")], texml).into_response()
}

// ── Helpers ──────────────────────────────────────────────────────

fn twiml_reject() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Content-Type", "application/xml")],
        r#"<?xml version="1.0" encoding="UTF-8"?><Response><Reject /></Response>"#,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn register_session(
    state: &AppState,
    session_id: &str,
    agent: &AgentConfig,
    vault_token: Option<String>,
    telephony_creds: Option<TelephonyCredentials>,
    direction: &str,
    from_number: Option<String>,
) {
    let providers = ProviderConfig {
        stt_url: agent.stt_base_url.clone(),
        stt_provider: agent.stt_provider.clone(),
        stt_model: agent.stt_model.clone(),
        stt_api_key: agent.stt_api_key.clone(),
        llm_url: agent.llm_base_url.clone(),
        llm_api_key: agent.llm_api_key.clone(),
        llm_model: agent.llm_model.clone(),
        llm_provider: agent.llm_provider.clone(),
        tts_url: agent.tts_base_url.clone(),
        tts_provider: agent.tts_provider.clone(),
        tts_model: agent.tts_model.clone(),
        tts_api_key: agent.tts_api_key.clone(),
        tts_voice_id: agent.tts_voice_id.clone(),
    };

    let mut config = SessionConfig {
        agent_id: agent.agent_id.clone(),
        agent_graph: agent.agent_graph.clone(),
        escalation_destinations: agent.escalation_destinations.clone(),
        // Seed voice_id from provider_configs (TTS row) as the default.
        // The agent graph may override this below.
        voice_id: agent.tts_voice_id.clone(),
        ..Default::default()
    };

    // Apply server-wide recordings output_uri as default (opt-out: enabled by default).
    // The agent graph's recording block (if present) overrides this entirely.
    config.recording.output_uri = state.recording_output_uri.clone();

    // Extract graph settings — mirrors voice-engine/src/python.rs graph extraction.
    // The agent_graph is the source of truth for v3_graph agents.
    if let Some(ref graph) = agent.agent_graph {
        // Top-level agent-wide settings
        if let Some(ref lang) = graph.language {
            if !lang.is_empty() {
                config.language = lang.clone();
            }
        }
        if let Some(ref vid) = graph.voice_id {
            if !vid.is_empty() {
                config.voice_id = vid.clone();
            }
        }

        // Recording configuration — graph wins if explicitly set.
        // Otherwise the server-wide default (enabled, configured output dir) applies.
        if let Some(ref recording) = graph.recording {
            config.recording = map_recording_config(recording);
        }
        // Always re-apply the server-wide recording output URI. The graph's
        // recording block may have overwritten it with its own default which
        // could be wrong inside Docker or on a custom bare-metal path.
        config.recording.output_uri = state.recording_output_uri.clone();

        // Entry node settings (system prompt, greeting, node-level voice_id)
        if let Some(entry_node) = graph.nodes.get(&graph.entry) {
            config.system_prompt = entry_node.system_prompt.clone();
            if let Some(ref g) = entry_node.greeting {
                config.greeting = Some(g.clone());
            }
            // Node-level voice_id overrides global
            if let Some(ref vid) = entry_node.voice_id {
                if !vid.is_empty() {
                    config.voice_id = vid.clone();
                }
            }
        }
        // Native Multimodal configuration
        if let Some(ref gm) = graph.gemini_live_model {
            if !gm.is_empty() {
                // "default" / "models/default" means the graph has a placeholder — use
                // the canonical model from the native_multimodal provider_config row.
                let resolved_model = if gm == "default" || gm == "models/default" {
                    agent.native_multimodal_model.clone()
                } else {
                    gm.clone()
                };

                config.native_multimodal = Some(voice_engine::session::NativeMultimodalConfig {
                    api_key: agent.native_multimodal_api_key.clone(),
                    model: Some(resolved_model),
                });
            }
        }
    }

    // Apply language instruction injection and any other derived mutations.
    // Mirrors the `cfg.finalize()` call in voice-engine/src/python.rs — must
    // be called after all graph fields are applied so `language` and
    // `system_prompt` are both set.
    let config = config.finalize();
    // ── Secrets ────────────────────────────────────────────────
    let agent_id_for_secrets = agent.agent_id.clone();
    let vault_token_for_refresh = vault_token.clone();

    // Verify and refresh expiring OAuth tokens on-demand.
    // We spawn this so it runs asynchronously in the background. It will opportunistically
    // refresh tokens, so by the time the agent acts, they should be fresh, without
    // blocking the critical path of the voice session handshake.
    #[cfg(feature = "integrations")]
    {
        if let Ok(parsed_id) = agent.agent_id.parse::<uuid::Uuid>() {
            let pool = state.pool.clone();
            let encryption = state.encryption.clone();
            let registry = state.registry.clone();
            tokio::spawn(async move {
                let _ = integrations::token_refresh::ensure_agent_tokens_fresh(
                    &pool,
                    &encryption,
                    &registry,
                    parsed_id,
                )
                .await;
            });
        }
    }

    let secrets =
        crate::secrets::resolve_vault_secrets(&agent_id_for_secrets, vault_token.as_deref()).await;
    let refresh_handle = crate::secrets::spawn_secret_refresh_task(
        agent_id_for_secrets,
        state.vault.clone(),
        vault_token_for_refresh,
        secrets.clone(),
    );
    let refresh_cell = refresh_handle.map(|h| std::sync::Arc::new(std::sync::Mutex::new(Some(h))));

    // ── Recording ──────────────────────────────────────────────
    // Voice-server creates the Tracer and subscribes BEFORE passing it to
    // voice-engine. voice-trace handles encoding; voice-server writes bytes
    // to disk via crate::recording::spawn (later: swap for S3 upload).
    let tracer = voice_trace::Tracer::new();
    let call_id = Uuid::new_v4();

    info!(
        session_id,
        call_id = %call_id,
        langfuse_enabled = state.observability.langfuse_enabled,
        langfuse_has_public_key = !state.observability.langfuse_public_key.is_empty(),
        langfuse_has_secret_key = !state.observability.langfuse_secret_key.is_empty(),
        langfuse_base_url = state.observability.langfuse_base_url.as_str(),
        "Registering session observability"
    );
    let obs_run = observability::spawn_adapter_manager(
        &tracer,
        state.pool.clone(),
        call_id,
        session_id.to_string(),
        state.observability.clone(),
    );
    let obs_variables = serde_json::json!({
        "observability": {
            "active_adapters": obs_run.active_adapters,
            "external_links": obs_run.external_links.iter().map(|l| {
                serde_json::json!({
                    "adapter": l.adapter,
                    "label": l.label,
                    "url": l.url,
                })
            }).collect::<Vec<_>>(),
        }
    });
    // Capture wall-clock start time for the call record (avoids clock-skew
    // from async delays between session end and DB write).
    let started_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // Insert an early call stub so call_events FK can reference this call_id
    // before final call completion record is written.
    db::insert_call_stub(
        &state.pool,
        call_id,
        &agent.agent_id,
        session_id,
        direction,
        started_at_epoch,
        Some(obs_variables.clone()),
    )
    .await;

    if config.recording.enabled {
        let recording_config = config.recording.clone();
        let session_id_rec = session_id.to_string();
        let pool_rec = state.pool.clone();
        let agent_id_rec = agent.agent_id.clone();
        let direction_rec = direction.to_string();
        let call_id_rec = call_id;
        let obs_variables_rec = Some(obs_variables.clone());

        // Subscribe BEFORE engine starts so no early events are missed.
        let rx = tracer.subscribe();

        // Capture the tokio runtime handle now — the on_complete closure runs
        // inside spawn_blocking where tokio::spawn requires an explicit handle.
        let rt_handle = tokio::runtime::Handle::current();

        // Wall-clock session start — used for ended_at regardless of whether
        // recording audio was capped early by max_duration_secs.
        let session_started = Instant::now();

        crate::recording::spawn(
            rx,
            session_id_rec.clone(),
            recording_config,
            move |recording_uri: Option<String>,
                  _audio_duration_secs: u32,
                  transcript_bytes: Option<Vec<u8>>| {
                // _audio_duration_secs is intentionally unused: we prefer
                // wall-clock elapsed time for the DB record because audio
                // duration can be shorter when max_duration_secs caps recording.
                let duration_secs = session_started.elapsed().as_secs() as u32;
                let ended_at_epoch = started_at_epoch + duration_secs as i64;
                let pool = pool_rec;
                let agent_id = agent_id_rec;
                let session_id = session_id_rec;
                let transcript_json: Option<serde_json::Value> =
                    transcript_bytes.and_then(|bytes| {
                        serde_json::from_slice(&bytes)
                            .map_err(|e| {
                                tracing::warn!("[recording] Failed to parse transcript JSON: {e}")
                            })
                            .ok()
                    });
                rt_handle.spawn(async move {
                    db::write_call_record(
                        &pool,
                        call_id_rec,
                        &agent_id,
                        &session_id,
                        duration_secs,
                        recording_uri,
                        transcript_json,
                        obs_variables_rec,
                        &direction_rec,
                        started_at_epoch,
                        ended_at_epoch,
                    )
                    .await;
                });
            },
        );
    } else {
        // Recording disabled — still write a call record (without a recording
        // URL) so every session appears in the calls list.
        let mut rx = tracer.subscribe();
        let pool_rec = state.pool.clone();
        let agent_id_rec = agent.agent_id.clone();
        let session_id_rec = session_id.to_string();
        let direction_rec = direction.to_string();
        let call_id_rec = call_id;
        let obs_variables_rec = Some(obs_variables);
        let mono_started = Instant::now();

        tokio::spawn(async move {
            use voice_trace::event::Event;
            loop {
                match rx.recv().await {
                    Ok(Event::SessionEnded) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    _ => continue,
                }
            }
            let duration_secs = mono_started.elapsed().as_secs() as u32;
            let ended_at_epoch = started_at_epoch + duration_secs as i64;
            db::write_call_record(
                &pool_rec,
                call_id_rec,
                &agent_id_rec,
                &session_id_rec,
                duration_secs,
                None,
                None,
                obs_variables_rec,
                &direction_rec,
                started_at_epoch,
                ended_at_epoch,
            )
            .await;
        });
    }

    // Wrap in Arc<Mutex<Option<...>>> so RegisteredSession stays Clone.
    let tracer_cell = std::sync::Arc::new(std::sync::Mutex::new(Some(tracer)));

    state.engine.sessions.insert(
        session_id.to_string(),
        RegisteredSession {
            config,
            providers,
            secrets: Some(secrets),
            secret_refresh_handle: refresh_cell,
            created_at: Instant::now(),
            tracer: Some(tracer_cell),
            telephony_creds,
            webhook_base_url: Some(state.public_url.clone()),
            from_number,
        },
    );
}

fn map_recording_config(
    proto_config: &proto::agent::RecordingConfig,
) -> agent_kit::RecordingConfig {
    let mut base = agent_kit::RecordingConfig {
        enabled: proto_config.enabled,
        ..Default::default()
    };

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
