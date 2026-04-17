//! DB queries for voice-server.
//!
//! Reads directly from the Postgres database — same DB, no HTTP hop.
//!
//! Tables used:
//!   `agents`           — get active_version
//!   `agent_versions`   — get config_json (v3_graph)
//!   `provider_configs` — get STT/TTS/LLM settings and telephony credentials
//!
//! PgBouncer compatibility: The pool connects natively using extended query protocol.
//! To use this securely behind a transaction pooler without query overlap bugs in SQLx 0.8,
//! ensure the pooler is running in Session Mode.
//!
//! Encrypted fields (stored as `{ciphertext, iv}` JSON blobs) are decrypted
//! inline using `integrations::EncryptionEngine` with the same key as the Python
//! backend (`AUTH__SECRET_KEY`).

use crate::cred::EncryptionEngine;
use agent_kit::swarm::AgentGraphDef;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// ── Agent config ─────────────────────────────────────────────────

/// Everything voice-engine needs to start a session.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub agent_id: String,
    /// Parsed agent graph — None for legacy config format or if parsing fails.
    pub agent_graph: Option<AgentGraphDef>,
    /// Parsed escalation destinations for telephony handoff.
    pub escalation_destinations: Vec<agent_kit::swarm::EscalationDestination>,
    // LLM (from provider_configs)
    pub llm_provider: String,
    pub llm_model: String,
    pub llm_base_url: String,
    pub llm_api_key: String,
    // STT (from provider_configs)
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_base_url: String,
    pub stt_api_key: String,
    // TTS (from provider_configs)
    pub tts_provider: String,
    pub tts_model: String,
    pub tts_base_url: String,
    pub tts_api_key: String,
    pub tts_voice_id: String,
    // Native Multimodal (from provider_configs)
    pub native_multimodal_model: String,
    pub native_multimodal_api_key: String,
}

/// Encrypted blob shape stored in config_json fields like `api_key_encrypted`,
/// `twilio_auth_token_encrypted`, `telnyx_api_key_encrypted`.
/// Matches what the Python backend writes: `{"ciphertext": "...", "iv": "..."}`.
#[derive(Debug, Deserialize, Clone)]
struct EncryptedBlob {
    ciphertext: String,
    #[serde(alias = "nonce")]
    iv: String,
}

/// Raw `config_json` shape in `provider_configs`.
#[derive(Debug, Deserialize, Default, Clone)]
struct ProviderConfigJson {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_encrypted: Option<EncryptedBlob>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default)]
    pub voice_server_url: String,
}

#[derive(Debug, Clone)]
pub struct ObservabilitySettings {
    pub db_events_enabled: bool,
    pub langfuse_enabled: bool,
    pub db_categories: Vec<String>,
    pub db_event_types: Vec<String>,
    pub queue_size: usize,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
    pub shutdown_flush_timeout_ms: u64,
    pub drop_policy: String,
    pub langfuse_base_url: String,
    pub langfuse_public_key: String,
    pub langfuse_secret_key: String,
    pub langfuse_trace_public: bool,
}

impl Default for ObservabilitySettings {
    fn default() -> Self {
        Self {
            db_events_enabled: true,
            langfuse_enabled: false,
            db_categories: vec![
                "session".to_string(),
                "metrics".to_string(),
                "observability".to_string(),
                "tool".to_string(),
                "error".to_string(),
            ],
            db_event_types: Vec::new(),
            queue_size: 2048,
            batch_size: 128,
            flush_interval_ms: 1000,
            shutdown_flush_timeout_ms: 1500,
            drop_policy: "drop_oldest".to_string(),
            langfuse_base_url: "https://cloud.langfuse.com".to_string(),
            langfuse_public_key: String::new(),
            langfuse_secret_key: String::new(),
            langfuse_trace_public: false,
        }
    }
}

impl ProviderConfigJson {
    /// Resolve the plaintext API key — prefers the encrypted form if present.
    fn resolved_api_key(&self, engine: &EncryptionEngine) -> String {
        if let Some(blob) = &self.api_key_encrypted {
            match engine.decrypt(&blob.ciphertext, &blob.iv) {
                Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(v) => {
                        return v
                            .get("value")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string()
                    }
                    Err(_) => return String::from_utf8_lossy(&bytes).to_string(),
                },
                Err(e) => warn!("Failed to decrypt api_key_encrypted: {e}"),
            }
        }
        self.api_key.clone()
    }
}

fn llm_role_prefix(role_name: &str) -> Option<&'static str> {
    match role_name {
        "__voice__" => Some("voice"),
        "__builder__" => Some("builder"),
        "__native_multimodal__" => Some("native_multimodal"),
        _ => None,
    }
}

async fn fetch_role_scoped_llm_provider(pool: &PgPool, role_name: &str) -> ProviderConfigJson {
    let pointer = fetch_provider(pool, "llm", role_name).await;
    let active = if !pointer.active.is_empty() {
        pointer.active.as_str()
    } else {
        pointer.provider.as_str()
    };

    if active.is_empty() {
        debug!("fetch_role_scoped_llm_provider({role_name}): no active provider");
        return ProviderConfigJson::default();
    }

    let Some(role_prefix) = llm_role_prefix(role_name) else {
        warn!("fetch_role_scoped_llm_provider({role_name}): unknown role prefix");
        return ProviderConfigJson::default();
    };

    let scoped_name = format!("{role_prefix}::{active}");
    let scoped = fetch_provider(pool, "llm", &scoped_name).await;
    if scoped.provider.is_empty() {
        debug!("fetch_role_scoped_llm_provider({role_name}): scoped row {scoped_name} not found");
    }
    scoped
}

// ── Public APIs ──────────────────────────────────────────────────

/// Fetch agent config by UUID — used by both the browser session-creation
/// endpoint and (indirectly) the telephony webhook handlers.
///
/// Returns `None` if the agent doesn't exist or has no published version.
pub async fn agent_by_id(
    pool: &PgPool,
    engine: &EncryptionEngine,
    agent_id: Uuid,
) -> Option<AgentConfig> {
    // Step 1: agents → active_version
    let row = sqlx::query("SELECT active_version FROM agents WHERE id = $1")
        .bind(agent_id)
        .persistent(false)
        .fetch_optional(pool)
        .await
        .map_err(|e| error!("DB lookup for agent {agent_id}: {e}"))
        .ok()??;

    let active_version: i32 = match row.try_get("active_version") {
        Ok(Some(v)) => v,
        Ok(None) => return None,
        Err(e) => {
            error!("active_version column missing for agent {agent_id}: {e}");
            return None;
        }
    };

    agent_config_for_id(pool, engine, agent_id, active_version).await
}

/// Fetch the public voice-server URL from the DB, or from the VOICE_SERVER__PUBLIC_URL env var if set.
/// Defaults to `http://localhost:8300` if not yet configured.
pub async fn voice_server_url(pool: &PgPool) -> String {
    if let Ok(env_url) = std::env::var("VOICE_SERVER__PUBLIC_URL") {
        if !env_url.is_empty() {
            return env_url;
        }
    }

    let cfg = fetch_provider(pool, "telephony", "__voice__").await;
    if cfg.voice_server_url.is_empty() {
        "http://localhost:8300".to_string()
    } else {
        cfg.voice_server_url
    }
}

/// Global observability settings (DB row preferred, env fallback).
///
/// Reads provider row:
/// - `provider_type = "observability"`
/// - `provider_name = "__voice__"`
pub async fn observability_settings(
    pool: &PgPool,
    engine: &EncryptionEngine,
) -> ObservabilitySettings {
    let mut settings = ObservabilitySettings::default();

    let row = sqlx::query(
        "SELECT config_json FROM provider_configs WHERE provider_type = $1 AND provider_name = $2 LIMIT 1"
    )
    .bind("observability")
    .bind("__voice__")
    .persistent(false)
    .fetch_optional(pool)
    .await
    .map_err(|e| error!("DB observability settings fetch failed: {e}"))
    .ok()
    .flatten();

    let Some(row) = row else {
        return settings;
    };
    let Ok(cfg) = row.try_get::<serde_json::Value, _>("config_json") else {
        return settings;
    };
    let Some(obj) = cfg.as_object() else {
        return settings;
    };

    if let Some(v) = obj.get("db_events_enabled").and_then(|v| v.as_bool()) {
        settings.db_events_enabled = v;
    }
    if let Some(v) = obj.get("langfuse_enabled").and_then(|v| v.as_bool()) {
        settings.langfuse_enabled = v;
    }
    if let Some(v) = obj.get("queue_size").and_then(|v| v.as_u64()) {
        settings.queue_size = (v as usize).max(1);
    }
    if let Some(v) = obj.get("batch_size").and_then(|v| v.as_u64()) {
        settings.batch_size = (v as usize).max(1);
    }
    if let Some(v) = obj.get("flush_interval_ms").and_then(|v| v.as_u64()) {
        settings.flush_interval_ms = v.max(50);
    }
    if let Some(v) = obj
        .get("shutdown_flush_timeout_ms")
        .and_then(|v| v.as_u64())
    {
        settings.shutdown_flush_timeout_ms = v.max(50);
    }
    if let Some(v) = obj.get("drop_policy").and_then(|v| v.as_str()) {
        settings.drop_policy = v.to_string();
    }
    if let Some(v) = obj.get("db_categories").and_then(|v| v.as_array()) {
        settings.db_categories = v
            .iter()
            .filter_map(|x| x.as_str().map(ToString::to_string))
            .collect();
    }
    if let Some(v) = obj.get("db_event_types").and_then(|v| v.as_array()) {
        settings.db_event_types = v
            .iter()
            .filter_map(|x| x.as_str().map(ToString::to_string))
            .collect();
    }
    if let Some(v) = obj.get("langfuse_base_url").and_then(|v| v.as_str()) {
        settings.langfuse_base_url = v.to_string();
    }
    if let Some(v) = obj.get("langfuse_trace_public").and_then(|v| v.as_bool()) {
        settings.langfuse_trace_public = v;
    }

    if let Some(v) = obj.get("langfuse_public_key").and_then(|v| v.as_str()) {
        settings.langfuse_public_key = v.to_string();
    }
    if let Some(v) = obj.get("langfuse_secret_key").and_then(|v| v.as_str()) {
        settings.langfuse_secret_key = v.to_string();
    }

    // encrypted key overrides plaintext key if present
    if let Some(blob_v) = obj.get("langfuse_public_key_encrypted") {
        if let Ok(blob) = serde_json::from_value::<EncryptedBlob>(blob_v.clone()) {
            if let Ok(bytes) = engine.decrypt(&blob.ciphertext, &blob.iv) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    settings.langfuse_public_key = v
                        .get("value")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
    }
    if let Some(blob_v) = obj.get("langfuse_secret_key_encrypted") {
        if let Ok(blob) = serde_json::from_value::<EncryptedBlob>(blob_v.clone()) {
            if let Ok(bytes) = engine.decrypt(&blob.ciphertext, &blob.iv) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    settings.langfuse_secret_key = v
                        .get("value")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
    }

    settings
}

/// Decrypt and return the telephony credentials stored on a phone number record.
///
/// Queries `phone_numbers.provider_credentials_encrypted` by E.164 phone number.
/// Returns `None` if the number isn't found or has no credentials stored.
///
/// The stored JSON blob (written by the Python backend) has the shape:
/// `{"ciphertext": "...", "iv": "...", "version": 1}`
/// Decrypted content: `{"provider": "twilio"|"telnyx", "twilio_account_sid"?, "twilio_auth_token"?, "telnyx_api_key"?}`
pub async fn phone_number_credentials(
    pool: &PgPool,
    engine: &EncryptionEngine,
    phone_number: &str,
) -> Option<voice_engine::server::TelephonyCredentials> {
    let row = sqlx::query(
        "SELECT provider_credentials_encrypted FROM phone_numbers WHERE phone_number = $1 LIMIT 1",
    )
    .bind(phone_number)
    .persistent(false)
    .fetch_optional(pool)
    .await
    .map_err(|e| error!("DB lookup for phone_number credentials {phone_number}: {e}"))
    .ok()??;

    let encrypted: Option<String> = row
        .try_get("provider_credentials_encrypted")
        .map_err(|e| warn!("provider_credentials_encrypted column error: {e}"))
        .ok()?;

    let raw = encrypted?;

    let blob: EncryptedBlob = serde_json::from_str(&raw)
        .map_err(|e| warn!("Failed to parse credentials blob for {phone_number}: {e}"))
        .ok()?;

    let bytes = engine
        .decrypt(&blob.ciphertext, &blob.iv)
        .map_err(|e| warn!("Failed to decrypt credentials for {phone_number}: {e}"))
        .ok()?;

    let creds: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| warn!("Failed to parse decrypted credentials JSON for {phone_number}: {e}"))
        .ok()?;

    let provider = creds.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    match provider {
        "twilio" => Some(voice_engine::server::TelephonyCredentials {
            twilio_account_sid: creds
                .get("twilio_account_sid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            twilio_auth_token: creds
                .get("twilio_auth_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            telnyx_api_key: String::new(),
            telnyx_connection_id: String::new(),
        }),
        "telnyx" => Some(voice_engine::server::TelephonyCredentials {
            twilio_account_sid: String::new(),
            twilio_auth_token: String::new(),
            telnyx_api_key: creds
                .get("telnyx_api_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            telnyx_connection_id: creds
                .get("telnyx_connection_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        other => {
            warn!("Unknown provider '{other}' in credentials for {phone_number}");
            None
        }
    }
}

// ── Internals ────────────────────────────────────────────────────

async fn agent_config_for_id(
    pool: &PgPool,
    engine: &EncryptionEngine,
    agent_id: Uuid,
    version: i32,
) -> Option<AgentConfig> {
    // Fetch the version's config_json
    let row =
        sqlx::query("SELECT config_json FROM agent_versions WHERE agent_id = $1 AND version = $2")
            .bind(agent_id)
            .bind(version)
            .persistent(false)
            .fetch_optional(pool)
            .await
            .map_err(|e| error!("DB version fetch for agent {agent_id} v{version}: {e}"))
            .ok()??;

    let config_json: serde_json::Value = row
        .try_get("config_json")
        .map_err(|e| error!("config_json column missing: {e}"))
        .ok()?;

    // Parse graph — voice-engine handles v3_graph via AgentGraphDef.
    let schema = config_json
        .get("config_schema_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_graph: Option<AgentGraphDef> = if schema == "v3_graph" {
        match serde_json::from_value(config_json.clone()) {
            Ok(g) => Some(g),
            Err(e) => {
                warn!("Failed to parse AgentGraphDef for agent {agent_id}: {e}");
                None
            }
        }
    } else {
        None
    };

    // escalation_destinations is now a first-class field on AgentGraphDef (proto tag 11).
    // Read it from the parsed graph; fall back to direct JSON extraction for agents
    // serialized before the proto field was added.
    let escalation_destinations: Vec<agent_kit::swarm::EscalationDestination> = agent_graph
        .as_ref()
        .map(|g| {
            g.escalation_destinations
                .iter()
                .map(|d| agent_kit::swarm::EscalationDestination {
                    name: d.name.clone(),
                    phone_number: d.phone_number.clone(),
                })
                .collect()
        })
        .filter(|v: &Vec<_>| !v.is_empty())
        .unwrap_or_else(|| {
            config_json
                .get("escalation_destinations")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        });

    // Fetch voice provider settings in a single round-trip.
    // This is faster than tokio::join! (which requires 3 connections) and
    // sequential fetches (which requires 3 RTTs). A single query is also
    // the most stable pattern for PgBouncer.
    let provider_map = fetch_providers_bulk(pool, &["stt", "tts"], "__voice__").await;
    let llm = fetch_role_scoped_llm_provider(pool, "__voice__").await;
    let native_multimodal = fetch_role_scoped_llm_provider(pool, "__native_multimodal__").await;
    let stt = provider_map.get("stt").cloned().unwrap_or_default();
    let mut tts = provider_map.get("tts").cloned().unwrap_or_default();

    // If agent specifies a custom tts_provider, override the global TTS settings.
    if let Some(ref graph) = agent_graph {
        if let Some(ref custom_provider) = graph.tts_provider {
            // If the custom provider differs from the global one, fetch its credentials.
            if custom_provider != &tts.provider {
                let custom_tts = fetch_provider(pool, "tts", custom_provider).await;
                if !custom_tts.provider.is_empty() {
                    tts = custom_tts;
                } else {
                    warn!(
                        "agent {agent_id}: custom tts_provider '{custom_provider}' not found in \
                         provider_configs — falling back to global TTS provider '{}'",
                        tts.provider
                    );
                }
            }
            // Override the model if specified
            if let Some(ref m) = graph.tts_model {
                tts.model = m.clone();
            }
        } else if let Some(ref m) = graph.tts_model {
            // Even if no custom provider is specified, we might have a custom model
            tts.model = m.clone();
        }
    }

    Some(AgentConfig {
        agent_id: agent_id.to_string(),
        agent_graph,
        escalation_destinations,
        llm_provider: llm.provider.clone(),
        llm_model: llm.model.clone(),
        llm_base_url: llm.base_url.clone(),
        llm_api_key: llm.resolved_api_key(engine),
        stt_provider: stt.provider.clone(),
        stt_model: stt.model.clone(),
        stt_base_url: stt.base_url.clone(),
        stt_api_key: stt.resolved_api_key(engine),
        tts_provider: tts.provider.clone(),
        tts_model: tts.model.clone(),
        tts_base_url: tts.base_url.clone(),
        tts_api_key: tts.resolved_api_key(engine),
        tts_voice_id: tts.voice_id.clone(),
        native_multimodal_model: native_multimodal.model.clone(),
        native_multimodal_api_key: native_multimodal.resolved_api_key(engine),
    })
}

async fn fetch_providers_bulk(
    pool: &PgPool,
    types: &[&str],
    name: &str,
) -> std::collections::HashMap<String, ProviderConfigJson> {
    let mut map = std::collections::HashMap::new();
    let rows = sqlx::query(
        r#"SELECT
            p1.provider_type,
            COALESCE(
                CASE
                    WHEN p1.config_json ? 'active' THEN (
                        SELECT p2.config_json
                        FROM provider_configs p2
                        WHERE p2.provider_type = p1.provider_type
                          AND p2.provider_name = (p1.config_json->>'active')
                        LIMIT 1
                    )
                    ELSE p1.config_json
                END,
                p1.config_json
            ) as config_json
        FROM provider_configs p1
        WHERE p1.provider_name = $1 AND p1.provider_type = ANY($2)"#,
    )
    .bind(name)
    .bind(types)
    .persistent(false)
    .fetch_all(pool)
    .await
    .map_err(|e| error!("DB bulk provider fetch for {name}: {e}"))
    .ok();

    if let Some(rows) = rows {
        for row in rows {
            let p_type: String = row.try_get("provider_type").unwrap_or_default();
            let config_val: serde_json::Value = row.try_get("config_json").unwrap_or_default();
            if let Ok(cfg) = serde_json::from_value::<ProviderConfigJson>(config_val) {
                map.insert(p_type, cfg);
            }
        }
    }
    map
}

async fn fetch_provider(
    pool: &PgPool,
    provider_type: &str,
    provider_name: &str,
) -> ProviderConfigJson {
    let row = sqlx::query(
        r#"SELECT
            COALESCE(
                CASE
                    WHEN p1.config_json ? 'active' THEN (
                        SELECT p2.config_json
                        FROM provider_configs p2
                        WHERE p2.provider_type = p1.provider_type
                          AND p2.provider_name = (p1.config_json->>'active')
                        LIMIT 1
                    )
                    ELSE p1.config_json
                END,
                p1.config_json
            ) as config_json
        FROM provider_configs p1
        WHERE p1.provider_type = $1 AND p1.provider_name = $2
        LIMIT 1"#,
    )
    .bind(provider_type)
    .bind(provider_name)
    .persistent(false)
    .fetch_optional(pool)
    .await
    .map_err(|e| error!("DB provider fetch {provider_type}/{provider_name}: {e}"))
    .ok()
    .flatten();

    match row {
        Some(r) => {
            let v: serde_json::Value = match r.try_get("config_json") {
                Ok(v) => v,
                Err(e) => {
                    warn!("fetch_provider({provider_type}/{provider_name}): config_json column error: {e}");
                    return ProviderConfigJson::default();
                }
            };
            match serde_json::from_value::<ProviderConfigJson>(v.clone()) {
                Ok(cfg) => {
                    debug!("fetch_provider({provider_type}/{provider_name}): base_url={:?}, provider={:?}", cfg.base_url, cfg.provider);
                    cfg
                }
                Err(e) => {
                    warn!("fetch_provider({provider_type}/{provider_name}): deserialization failed: {e}, raw={v}");
                    ProviderConfigJson::default()
                }
            }
        }
        None => {
            debug!("fetch_provider({provider_type}/{provider_name}): no row found in DB");
            ProviderConfigJson::default()
        }
    }
}

/// Insert a completed call record into the `calls` table.
///
/// Called by voice-server after a session ends. The `recording_url` points
/// to `{public_url}/recordings/{filename}` served as a static file.
/// `direction` should be `"inbound"` for telephony or `"webrtc"` for browser calls.
///
/// `started_at_epoch` and `ended_at_epoch` are Unix timestamps (seconds since
/// epoch) captured by the caller, avoiding clock-skew from async delays
/// between session end and DB write.
///
/// `transcript_json` is the raw transcript bytes (JSON-serialised
/// `SessionTranscript`). Stored in the `transcript_json` JSONB column
/// for API retrieval. Pass `None` when recording is disabled.
#[allow(clippy::too_many_arguments)]
pub async fn write_call_record(
    pool: &PgPool,
    call_id: Uuid,
    agent_id: &str,
    session_id: &str,
    duration_secs: u32,
    recording_url: Option<String>,
    transcript_json: Option<serde_json::Value>,
    variables_json: Option<serde_json::Value>,
    direction: &str,
    started_at_epoch: i64,
    ended_at_epoch: i64,
) {
    let Ok(agent_uuid) = uuid::Uuid::parse_str(agent_id) else {
        warn!("write_call_record: invalid agent_id {agent_id}");
        return;
    };
    let result = sqlx::query(
        r#"INSERT INTO calls
           (id, agent_id, direction, status, provider_call_id, duration_seconds, recording_url, transcript_json, variables_json, started_at, ended_at)
           VALUES ($1, $2, $3, 'completed', $4, $5, $6, $7, $8, to_timestamp($9), to_timestamp($10))
           ON CONFLICT (id) DO UPDATE SET
             direction = EXCLUDED.direction,
             status = 'completed',
             provider_call_id = EXCLUDED.provider_call_id,
             duration_seconds = EXCLUDED.duration_seconds,
             recording_url = EXCLUDED.recording_url,
             transcript_json = EXCLUDED.transcript_json,
             variables_json = EXCLUDED.variables_json,
             started_at = EXCLUDED.started_at,
             ended_at = EXCLUDED.ended_at"#,
    )
    .bind(call_id)
    .bind(agent_uuid)
    .bind(direction)
    .bind(session_id)
    .bind(duration_secs as i32)
    .bind(&recording_url)
    .bind(&transcript_json)
    .bind(&variables_json)
    .bind(started_at_epoch)
    .bind(ended_at_epoch)
    .persistent(false)
    .execute(pool)
    .await;

    match result {
        Ok(_) => info!("Call record written (session={session_id}, direction={direction}, duration={duration_secs}s, recording={recording_url:?})"),
        Err(e) => error!("Failed to write call record for session {session_id}: {e}"),
    }
}

/// Create an initial call row early so child rows (call_events) can safely
/// reference `call_id` via FK before the call completes.
#[allow(clippy::too_many_arguments)]
pub async fn insert_call_stub(
    pool: &PgPool,
    call_id: Uuid,
    agent_id: &str,
    session_id: &str,
    direction: &str,
    started_at_epoch: i64,
    variables_json: Option<serde_json::Value>,
) {
    let Ok(agent_uuid) = uuid::Uuid::parse_str(agent_id) else {
        warn!("insert_call_stub: invalid agent_id {agent_id}");
        return;
    };

    let result = sqlx::query(
        r#"INSERT INTO calls
           (id, agent_id, direction, status, provider_call_id, variables_json, started_at)
           VALUES ($1, $2, $3, 'in_progress', $4, $5, to_timestamp($6))
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(call_id)
    .bind(agent_uuid)
    .bind(direction)
    .bind(session_id)
    .bind(&variables_json)
    .bind(started_at_epoch)
    .persistent(false)
    .execute(pool)
    .await;

    if let Err(e) = result {
        error!("Failed to insert call stub for session {session_id}: {e}");
    }
}
