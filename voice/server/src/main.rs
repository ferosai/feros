//! voice-server — telephony gateway.
//!
//! Sits in front of voice-engine: handles HTTP webhooks from Twilio/Telnyx,
//! reads agent config from Postgres, registers sessions, then delegates all
//! real-time WebSocket handling to voice-engine's built-in router.
//!
//! Users expose **one public URL** — this server.
//! Python backend only needs internal (private) network access.

mod config;
mod cred;
mod db;
mod observability;
mod recording;
mod secrets;
mod sessions;
mod telephony;
mod utils;
mod vault_client;

use std::sync::Arc;

use axum::http::{HeaderValue, Method};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use voice_engine::server::{build_router, ProviderConfig, ServerState, TelephonyCredentials};

use config::Settings;
use cred::{EncryptionEngine, VaultHandle};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "voice_server=debug,voice_engine=debug,agent_kit=info,tower_http=debug".into()
            }),
        )
        .init();

    let settings = Settings::from_env().expect("Failed to parse settings");
    info!(?settings, "voice-server starting");

    // Connect to the same Postgres DB as the Python backend.
    // statement_cache_capacity(0) on PgConnectOptions forces the simple query
    // protocol for every query, which is required by Supabase/PgBouncer in
    // transaction mode. Without this, parallel sqlx queries (tokio::join!) can
    // hit a connection with a stale prepared-statement handle, causing
    // "unnamed prepared statement does not exist" errors.
    let connect_opts = settings
        .database_url
        .parse::<PgConnectOptions>()
        .expect("Invalid DATABASE__URL")
        .statement_cache_capacity(0);
    let pool = PgPoolOptions::new()
        .connect_with(connect_opts)
        .await
        .expect("Failed to connect to Postgres");
    info!("Connected to Postgres");

    // ── Credential encryption engine ─────────────────────────────────
    // Uses AUTH__SECRET_KEY as the AES-256-GCM key.
    // When the `integrations` feature is disabled this is a no-op stub that lets
    // the plaintext field values through instead.
    let encryption_engine = if settings.auth_secret_key.is_empty() {
        warn!("AUTH__SECRET_KEY not set — encrypted credentials will not be decrypted");
        Arc::new(EncryptionEngine::new(&[0u8; 32]))
    } else {
        Arc::new(
            EncryptionEngine::from_base64(&settings.auth_secret_key).unwrap_or_else(|e| {
                warn!("Invalid AUTH__SECRET_KEY for decryption ({e}) — using dummy key");
                EncryptionEngine::new(&[0u8; 32])
            }),
        )
    };

    // ── Vault server ─────────────────────────────────────────────────
    // Started when the `integrations` feature is enabled. Enables per-agent OAuth
    // credential resolution at tool-call time (e.g., Google Calendar, Slack).
    // Managing VAULT_ADDR / VAULT_CACERT env vars is handled here internally
    // — callers don't need to set them.
    let vault_handle: Option<Arc<VaultHandle>> = start_vault(&settings);

    // Read public URL from DB. Credentials are stored per phone number
    // (phone_numbers.provider_credentials_encrypted) and loaded at inbound webhook time.
    let public_url = db::voice_server_url(&pool).await;
    if public_url.starts_with("http://localhost") {
        warn!(
            "voice_server_url not configured in DB — using localhost default. \
             Set it in Settings → Telephony for Twilio/Telnyx webhooks to work."
        );
    }
    info!(%public_url, "Voice server public URL");

    let observability_settings = db::observability_settings(&pool, &encryption_engine).await;
    info!(
        db_events_enabled = observability_settings.db_events_enabled,
        langfuse_enabled = observability_settings.langfuse_enabled,
        langfuse_has_public_key = !observability_settings.langfuse_public_key.is_empty(),
        langfuse_has_secret_key = !observability_settings.langfuse_secret_key.is_empty(),
        langfuse_base_url = observability_settings.langfuse_base_url.as_str(),
        "Loaded observability settings"
    );
    observability::init_global_adapters(&observability_settings);

    #[cfg(feature = "integrations")]
    let registry = match integrations::ProviderRegistry::from_embedded() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            warn!(error = %e, "Failed to load embedded integrations registry");
            panic!(
                "Cannot start voice-server without integrations registry (integrations feature enabled)"
            );
        }
    };

    let providers = ProviderConfig::default();
    let engine_state = ServerState::new(
        providers,
        TelephonyCredentials::default(),
        settings.auth_secret_key.clone(),
    );

    let app_state = telephony::AppState {
        engine: engine_state.clone(),
        pool: pool.clone(),
        public_url: public_url.clone(),
        encryption: encryption_engine,
        vault: vault_handle,
        recording_output_uri: settings.recording_output_uri.clone(),
        #[cfg(feature = "integrations")]
        registry: registry.clone(),
        observability: observability_settings,
    };

    // ── OAuth token refresher ────────────────────────────────────────
    // Refreshes expiring OAuth credentials every 60 s, writing fresh tokens
    // back to the `credentials` table.  The per-session `spawn_secret_refresh_task`
    // in secrets.rs re-reads them every 90 s, so a newly-refreshed token is
    // picked up within ~90 s without restarting the session.
    #[cfg(feature = "integrations")]
    start_token_refresher(&settings, pool.clone(), registry.clone());
    #[cfg(not(feature = "integrations"))]
    start_token_refresher(&settings, pool.clone());

    // Session TTL cleanup
    {
        let s = engine_state.clone();
        let ttl = s.session_ttl_secs;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                s.sessions.retain(|id, sess| {
                    let alive = sess.created_at.elapsed().as_secs() < ttl;
                    if !alive {
                        tracing::info!("Session {} expired", id);
                        // Abort the background token-refresh task if it was never
                        // consumed by a handler (client never connected).
                        // Dropping a JoinHandle only *detaches* the task in Tokio;
                        // we must abort() explicitly to stop it.
                        if let Some(h) = sess.take_refresh_handle() {
                            h.abort();
                        }
                    }
                    alive
                });
            }
        });
    }

    // CORS — outermost so all routes inherit it
    let cors = {
        let origins_str = std::env::var("ALLOWED_ORIGINS")
            .unwrap_or_else(|_| "http://localhost:3000,http://127.0.0.1:3000".to_string());

        if origins_str.trim() == "*" {
            info!("CORS: permissive (ALLOWED_ORIGINS=*)");
            CorsLayer::permissive()
        } else {
            let mut origins: Vec<HeaderValue> = origins_str
                .split(',')
                .filter_map(|o| {
                    let s = o.trim();
                    if s.is_empty() {
                        None
                    } else {
                        s.parse().ok()
                    }
                })
                .collect();

            // Always include the DB-configured public URL origin so the frontend
            // served from that URL can reach the voice server without extra config.
            if let Ok(public_url_origin) = public_url.trim_end_matches('/').parse::<HeaderValue>() {
                origins.push(public_url_origin);
            }

            if origins.is_empty() {
                info!("CORS: permissive (no origins configured)");
                CorsLayer::permissive()
            } else {
                info!("CORS allowed origins: {origins_str} + {public_url}");
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                    .allow_headers([
                        axum::http::header::CONTENT_TYPE,
                        axum::http::header::AUTHORIZATION,
                    ])
            }
        }
    };

    let app = build_router(engine_state)
        .merge(telephony::http_router(app_state.clone()))
        .merge(sessions::router(app_state))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let addr: std::net::SocketAddr = format!("{}:{}", settings.listen_host, settings.listen_port)
        .parse()
        .expect("Invalid LISTEN_HOST/LISTEN_PORT");

    info!("🚀 voice-server listening on {addr}");
    info!("   - POST /telephony/twilio/incoming/{{agent_id}} (Webhook)");
    info!("   - GET  /telephony/twilio (WebSocket Stream)");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Start the embedded vault server when the `integrations` feature is enabled.
///
/// Sets `VAULT_ADDR` and `VAULT_CACERT` env vars internally — the `secrets`
/// module reads them automatically. No external configuration
/// required from callers.
#[cfg(feature = "integrations")]
fn start_vault(settings: &Settings) -> Option<Arc<VaultHandle>> {
    if settings.auth_secret_key.is_empty() {
        warn!("AUTH__SECRET_KEY not set — vault server will not start");
        return None;
    }
    match cred::start_vault_server(
        settings.database_url.clone(),
        settings.auth_secret_key.clone(),
        0, // port 0 = OS-assigned ephemeral port
    ) {
        Ok(handle) => {
            let vault_url = handle.url();
            let cert_path = write_vault_cert(&handle.cert_pem);
            // Set internally — secrets module reads these to contact the vault.
            std::env::set_var("VAULT_ADDR", &vault_url);
            std::env::set_var("VAULT_CACERT", &cert_path);
            info!(%vault_url, "🔐 Vault server started (TLS, `integrations` feature)");
            Some(Arc::new(handle))
        }
        Err(e) => {
            warn!("Vault startup failed: {e} — sessions will run without agent credentials");
            None
        }
    }
}

/// No-op vault startup when `integrations` feature is disabled.
#[cfg(not(feature = "integrations"))]
fn start_vault(_settings: &Settings) -> Option<Arc<VaultHandle>> {
    None
}

/// Write the vault's ephemeral TLS cert to a temp file and return the path.
#[cfg(feature = "integrations")]
fn write_vault_cert(cert_pem: &str) -> String {
    let mut path = std::env::temp_dir();
    path.push(format!("voice_server_vault_ca_{}.pem", std::process::id()));
    std::fs::write(&path, cert_pem).expect("Failed to write vault cert");
    path.to_string_lossy().to_string()
}

#[cfg(feature = "integrations")]
fn start_token_refresher(
    settings: &Settings,
    pool: sqlx::PgPool,
    registry: Arc<integrations::ProviderRegistry>,
) {
    let Ok(engine) = integrations::EncryptionEngine::from_base64(&settings.auth_secret_key) else {
        warn!("AUTH__SECRET_KEY missing or invalid — OAuth TokenRefresher will not start");
        return;
    };

    // Client credentials (client_id / client_secret) are stored in the
    // `oauth_apps` DB table and loaded fresh on every refresh tick.
    let refresher = integrations::TokenRefresher::new(pool, engine, (*registry).clone());
    info!("OAuth TokenRefresher started");
    tokio::spawn(async move { refresher.run_cron().await });
}

#[cfg(not(feature = "integrations"))]
fn start_token_refresher(_settings: &Settings, _pool: sqlx::PgPool) {}
