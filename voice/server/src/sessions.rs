//! Browser/WebSocket session creation endpoint.
//!
//! `POST /voice/session/{agent_id}` — mirrors what Python's voice_session.py does today.
//!
//! Flow:
//!   1. Client sends agent_id in the URL path.
//!   2. voice-server reads agent config from DB.
//!   3. Registers a pre-signed session in voice-engine.
//!   4. Returns {session_id, ws_url, token}.
//!   5. Client connects to ws_url for real-time audio.
//!
//! Auth: expects a bearer token verified by the `integrations` crate (or skipped in dev).

use crate::{db, telephony::AppState, utils::to_ws_url};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

// ── Response type ────────────────────────────────────────────────

#[derive(Serialize)]
pub struct VoiceSessionResponse {
    pub session_id: String,
    pub ws_url: String,
    pub token: String,
}

// ── Router ───────────────────────────────────────────────────────

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/voice/session/{agent_id}", post(create_session))
        .with_state(state)
}

// ── Handler ──────────────────────────────────────────────────────

/// `POST /voice/session/{agent_id}`
///
/// Creates a voice session for the given agent and returns the WebSocket URL
/// the client should connect to.
async fn create_session(
    Path(agent_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Parse the agent UUID
    let Ok(agent_uuid) = agent_id.parse::<Uuid>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid agent_id"})),
        )
            .into_response();
    };

    // Load agent config from DB
    let Some(agent) = db::agent_by_id(&state.pool, &state.encryption, agent_uuid).await else {
        warn!("Agent {agent_id} not found or has no active version");
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "agent not found"})),
        )
            .into_response();
    };

    // Register session in voice-engine
    let session_id: String = Uuid::new_v4().to_string();
    let token = state.engine.sign_token(&session_id);
    let vault_token = state.vault_token_for(agent_uuid);
    let public_url = db::voice_server_url(&state.pool).await;
    // Browser/WebRTC sessions have no telephony credentials and no from_number
    register_session(&state, &session_id, &agent, vault_token, None, "webrtc", None).await;

    // Build the WS URL — converts https:// → wss:// automatically
    let ws_url = format!(
        "{}/ws/voice/{}?token={}",
        to_ws_url(&public_url),
        session_id,
        token
    );

    info!(
        session_id = session_id.as_str(),
        agent_id = agent_id.as_str(),
        "Created browser voice session"
    );

    Json(VoiceSessionResponse {
        session_id,
        ws_url,
        token,
    })
    .into_response()
}

// register_session lives in telephony.rs (shared between telephony webhooks
// and browser session creation).
use crate::telephony::register_session;
