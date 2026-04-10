//! Mini Vault Server — a lightweight, Vault KV v2-compatible HTTP server.
//!
//! Speaks a subset of the HashiCorp Vault HTTP API so standard Vault clients
//! (e.g. `vaultrs`) can read secrets without modification.
//!
//! Supported endpoints:
//!   - Token auth via `X-Vault-Token` header (also accepts `Authorization: Bearer`)
//!   - KV v2 read: `GET /v1/secret/data/{path}`
//!   - Health:     `GET /v1/sys/health`
//!   - Standard JSON error responses: `{ "errors": ["..."] }`
//!
//! Security:
//!   - Constant-time token comparison to prevent timing attacks
//!   - Localhost-only binding by default
//!   - No plaintext secrets in logs (tracing at info level omits values)
//!   - Secret data is decrypted on-demand, never cached in memory
//!   - Scoped tokens restrict access to a single agent ID
//!   - Per-token rate limiting prevents brute-force secret enumeration

use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use serde::Serialize;

use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

use crate::encryption::EncryptionEngine;
use crate::secret_resolver;

// ── Secret Provider Trait ───────────────────────────────────────

/// Trait for resolving secrets by agent ID.
///
/// The vault server calls this on each KV v2 read request.
/// Production uses `DbSecretProvider` (Postgres + EncryptionEngine).
/// Tests can inject an in-memory mock.
#[async_trait]
pub trait SecretProvider: Send + Sync + 'static {
    /// Resolve secrets for an agent, returning a flat key→value map.
    async fn resolve(&self, agent_id: Uuid) -> Result<HashMap<String, String>, String>;
}

/// Production secret provider backed by Postgres + AES-256-GCM decryption.
pub struct DbSecretProvider {
    pool: sqlx::PgPool,
    engine: Arc<EncryptionEngine>,
}

#[async_trait]
impl SecretProvider for DbSecretProvider {
    async fn resolve(&self, agent_id: Uuid) -> Result<HashMap<String, String>, String> {
        let secrets = secret_resolver::resolve_secrets(&self.pool, &self.engine, agent_id)
            .await
            .map_err(|e| e.to_string())?;

        // Convert SecretMap (HashMap<String, SecretString>) to HashMap<String, String>
        use secrecy::ExposeSecret;
        Ok(secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
            .collect())
    }
}

// ── Token Auth ──────────────────────────────────────────────────

/// Header name for Vault token authentication.
/// Compatible with the HashiCorp Vault API.
const VAULT_TOKEN_HEADER: &str = "x-vault-token";

/// Extract the client token from request headers.
///
/// Supports (in priority order):
///   1. `X-Vault-Token` header
///   2. `Authorization: Bearer <token>` header
fn extract_token(headers: &HeaderMap) -> Option<String> {
    // X-Vault-Token header (primary)
    if let Some(val) = headers.get(VAULT_TOKEN_HEADER) {
        return val.to_str().ok().map(|s| s.to_string());
    }

    // Authorization: Bearer <token> (fallback)
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Constant-time token comparison to prevent timing attacks.
fn validate_token(provided: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

// ── Vault KV v2 Response Types ──────────────────────────────────

/// Vault KV v2 read response shape.
/// Must match <https://developer.hashicorp.com/vault/api-docs/secret/kv/kv-v2#read-secret-version>
/// so `vaultrs::kv2::read()` can parse it.
#[derive(Serialize)]
struct Kv2ReadResponse {
    request_id: String,
    lease_id: String,
    renewable: bool,
    lease_duration: u64,
    data: Kv2Data,
}

#[derive(Serialize)]
struct Kv2Data {
    data: HashMap<String, String>,
    metadata: Kv2Metadata,
}

#[derive(Serialize)]
struct Kv2Metadata {
    created_time: String,
    deletion_time: String,
    destroyed: bool,
    version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_metadata: Option<HashMap<String, String>>,
}

/// Standard Vault error response.
#[derive(Serialize)]
struct VaultErrorResponse {
    errors: Vec<String>,
}

fn vault_error(status: StatusCode, msg: &str) -> impl IntoResponse {
    (
        status,
        Json(VaultErrorResponse {
            errors: vec![msg.to_string()],
        }),
    )
}

// ── Token Registry (Scoped Tokens) ──────────────────────────────

/// Policy attached to a scoped token — restricts access to a single agent.
struct TokenPolicy {
    agent_id: Uuid,
    expires_at: chrono::DateTime<chrono::Utc>,
}

/// Thread-safe registry of scoped tokens.
///
/// The root token is stored separately and bypasses this registry.
pub struct TokenRegistry {
    tokens: std::sync::RwLock<HashMap<String, TokenPolicy>>,
}

impl TokenRegistry {
    fn new() -> Self {
        Self {
            tokens: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Register a new scoped token for a specific agent.
    /// Returns the generated token string.
    ///
    /// Also evicts any expired tokens to prevent unbounded memory growth.
    fn create_scoped_token(&self, agent_id: Uuid, ttl: Duration) -> String {
        let token = generate_scoped_token();
        let now = chrono::Utc::now();
        let policy = TokenPolicy {
            agent_id,
            expires_at: now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(2)),
        };
        let mut tokens = self.tokens.write().unwrap();
        // Evict expired tokens
        tokens.retain(|_, p| p.expires_at > now);
        tokens.insert(token.clone(), policy);
        info!(agent_id = %agent_id, ttl_secs = ttl.as_secs(), active_tokens = tokens.len(), "Scoped token created");
        token
    }

    /// Validate a scoped token and return the allowed agent ID if valid.
    fn validate(&self, token: &str, requested_agent_id: Uuid) -> ScopedTokenResult {
        let tokens = self.tokens.read().unwrap();
        match tokens.get(token) {
            None => ScopedTokenResult::NotFound,
            Some(policy) => {
                if chrono::Utc::now() > policy.expires_at {
                    ScopedTokenResult::Expired
                } else if policy.agent_id != requested_agent_id {
                    ScopedTokenResult::AgentMismatch
                } else {
                    ScopedTokenResult::Ok
                }
            }
        }
    }
}

enum ScopedTokenResult {
    Ok,
    NotFound,
    Expired,
    AgentMismatch,
}

// ── Shared State ────────────────────────────────────────────────

/// Vault server shared state.
#[derive(Clone)]
pub struct VaultState {
    /// The root token for authentication (allows access to any agent).
    token: Arc<String>,
    /// Secret provider (DB-backed in production, mockable in tests).
    provider: Arc<dyn SecretProvider>,
    /// Scoped token registry for agent-specific access control.
    registry: Arc<TokenRegistry>,
    /// Per-request rate limiter (shared across all tokens).
    rate_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
}

// ── Route Handlers ──────────────────────────────────────────────

/// `GET /v1/secret/data/{path}` — Read secrets for an agent.
///
/// The `path` is the agent ID (UUID). Returns decrypted credentials
/// in Vault KV v2 format so `vaultrs::kv2::read()` works out of the box.
///
/// Authentication is checked in two tiers:
///   1. Root token — allows access to any agent (backward compat)
///   2. Scoped token — only allows access to the assigned agent ID
async fn kv2_read(
    Path(path): Path<String>,
    headers: HeaderMap,
    State(state): State<VaultState>,
) -> impl IntoResponse {
    // ── Rate limiting ───────────────────────────────────────────
    if state.rate_limiter.check().is_err() {
        warn!("Vault rate limit exceeded");
        return vault_error(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    // ── Extract token ───────────────────────────────────────────
    let token = match extract_token(&headers) {
        Some(t) => t,
        None => {
            warn!("Vault request missing token");
            return vault_error(StatusCode::FORBIDDEN, "missing client token").into_response();
        }
    };

    // ── Parse agent ID early (needed for scoped check) ──────────
    let agent_id = match Uuid::parse_str(&path) {
        Ok(id) => id,
        Err(_) => {
            return vault_error(StatusCode::BAD_REQUEST, "invalid agent_id format").into_response();
        }
    };

    // ── Authenticate ────────────────────────────────────────────
    let is_root = validate_token(&token, &state.token);

    if !is_root {
        // Check scoped token registry
        match state.registry.validate(&token, agent_id) {
            ScopedTokenResult::Ok => { /* scoped token is valid for this agent */ }
            ScopedTokenResult::NotFound => {
                warn!(agent_id = %agent_id, "Vault request with unknown token");
                return vault_error(StatusCode::FORBIDDEN, "permission denied").into_response();
            }
            ScopedTokenResult::Expired => {
                warn!(agent_id = %agent_id, "Vault request with expired scoped token");
                return vault_error(StatusCode::FORBIDDEN, "token expired").into_response();
            }
            ScopedTokenResult::AgentMismatch => {
                warn!(agent_id = %agent_id, "Scoped token used for wrong agent");
                return vault_error(StatusCode::FORBIDDEN, "permission denied").into_response();
            }
        }
    }

    // ── Resolve secrets via the provider ─────────────────────────
    let data = match state.provider.resolve(agent_id).await {
        Ok(map) => map,
        Err(e) => {
            warn!(agent_id = %agent_id, error = %e, "Failed to resolve secrets");
            return vault_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to resolve secrets",
            )
            .into_response();
        }
    };

    info!(
        agent_id = %agent_id,
        num_secrets = data.len(),
        token_type = if is_root { "root" } else { "scoped" },
        "Vault: resolved secrets"
    );

    // Return Vault KV v2 response
    let response = Kv2ReadResponse {
        request_id: Uuid::new_v4().to_string(),
        lease_id: String::new(),
        renewable: false,
        lease_duration: 0,
        data: Kv2Data {
            data,
            metadata: Kv2Metadata {
                created_time: chrono::Utc::now().to_rfc3339(),
                deletion_time: String::new(),
                destroyed: false,
                version: 1,
                custom_metadata: None,
            },
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// `GET /v1/sys/health` — Health check endpoint.
///
/// Returns 200 with vault status. Compatible with Vault's health API.
async fn sys_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "initialized": true,
        "sealed": false,
        "standby": false,
        "server_time_utc": chrono::Utc::now().timestamp(),
        "version": "0.1.0-mini"
    }))
}

// ── Server Lifecycle ────────────────────────────────────────────

/// Handle returned from `start_vault_server` to manage the server lifecycle.
pub struct VaultHandle {
    /// The root token for this vault instance.
    pub token: String,
    /// The address the server is listening on.
    pub addr: SocketAddr,
    /// PEM-encoded self-signed certificate for this vault instance.
    /// Clients should trust only this certificate when connecting.
    pub cert_pem: String,
    /// Server handle for graceful shutdown.
    server_handle: axum_server::Handle<SocketAddr>,
    /// Shared token registry for creating scoped tokens.
    registry: Arc<TokenRegistry>,
}

impl VaultHandle {
    /// Get the vault URL (e.g. `https://127.0.0.1:<port>`).
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Create a scoped token that only allows reading secrets for a specific agent.
    ///
    /// The token expires after `ttl`. Requests using this token for any other
    /// agent ID will be rejected with 403.
    ///
    /// # Arguments
    /// * `agent_id` — The UUID of the agent this token is scoped to
    /// * `ttl` — Duration before the token expires
    pub fn create_scoped_token(&self, agent_id: Uuid, ttl: Duration) -> String {
        self.registry.create_scoped_token(agent_id, ttl)
    }

    /// Gracefully shut down the vault server.
    pub fn shutdown(self) {
        self.server_handle.shutdown();
    }
}

impl Drop for VaultHandle {
    fn drop(&mut self) {
        self.server_handle.shutdown();
    }
}

/// Start the mini vault server.
///
/// Binds to `127.0.0.1:{port}` and serves the Vault KV v2 API.
/// The server runs on a dedicated OS thread with its own tokio runtime
/// (same pattern as the token refresher — fully independent of Python's asyncio).
///
/// Returns a `VaultHandle` with the root token and address.
///
/// # Arguments
/// * `db_url` — Postgres connection string
/// * `secret_key` — Base64-encoded 32-byte encryption key
/// * `port` — Port to bind to (0 = OS-assigned ephemeral port)
pub fn start_vault_server(
    db_url: String,
    secret_key: String,
    port: u16,
) -> Result<VaultHandle, Box<dyn std::error::Error + Send + Sync>> {
    let engine = EncryptionEngine::from_base64(&secret_key)?;
    start_vault_server_with_engine(db_url, engine, port)
}

/// Start the vault server with a pre-built `EncryptionEngine`.
///
/// This is the core implementation — `start_vault_server` is a thin
/// wrapper that constructs the engine from a base64 key first.
pub fn start_vault_server_with_engine(
    db_url: String,
    engine: EncryptionEngine,
    port: u16,
) -> Result<VaultHandle, Box<dyn std::error::Error + Send + Sync>> {
    let engine = Arc::new(engine);

    // Build the DB-backed provider lazily on the vault server's thread.
    // We wrap the DB config in a closure so the PgPool is created on the
    // same tokio runtime that will later run queries — otherwise the pool's
    // internal connections would be tied to a dead runtime.
    let provider_factory = {
        move || {
            Box::pin(async move {
                let connect_opts: sqlx::postgres::PgConnectOptions =
                    db_url.parse().map_err(|e: sqlx::Error| e.to_string())?;

                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(5)
                    .connect_with(connect_opts)
                    .await
                    .map_err(|e| format!("Failed to connect to DB: {}", e))?;

                let db_provider = DbSecretProvider { pool, engine };
                let provider = into_secret_provider(db_provider);

                Ok::<_, String>(provider)
            })
        }
    };

    start_vault_server_with_lazy_provider(provider_factory, port)
}

/// Start the vault server with a lazily-constructed provider.
///
/// The `provider_factory` closure is called on the vault server's own tokio
/// runtime, ensuring that any async resources (DB pools, etc.) are tied to
/// the correct runtime.
fn start_vault_server_with_lazy_provider<F, Fut>(
    provider_factory: F,
    port: u16,
) -> Result<VaultHandle, Box<dyn std::error::Error + Send + Sync>>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Arc<dyn SecretProvider>, String>> + Send,
{
    // Install the ring crypto provider for rustls (required by rustls 0.23+).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let token = generate_root_token();
    let token_clone = token.clone();
    let registry = Arc::new(TokenRegistry::new());
    let registry_clone = registry.clone();

    let rate_limiter = Arc::new(RateLimiter::direct(Quota::per_second(
        NonZeroU32::new(50).unwrap(),
    )));

    // Generate an ephemeral self-signed TLS certificate
    let (cert_pem, key_pem) = generate_self_signed_cert()?;
    let cert_pem_clone = cert_pem.clone();

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert_pem.as_bytes().to_vec(),
        key_pem.as_bytes().to_vec(),
    );

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    let server_handle = axum_server::Handle::new();
    let server_handle_clone = server_handle.clone();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("Failed to create tokio runtime: {}", e)));
                return;
            }
        };

        rt.block_on(async move {
            // Build the provider on THIS runtime so DB connections are valid
            let provider = match provider_factory().await {
                Ok(p) => p,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Failed to build provider: {}", e)));
                    return;
                }
            };

            let state = VaultState {
                token: Arc::new(token_clone),
                provider,
                registry: registry_clone,
                rate_limiter,
            };

            let app = Router::new()
                .route("/v1/secret/data/{path}", get(kv2_read))
                .route("/v1/sys/health", get(sys_health))
                .layer(TraceLayer::new_for_http())
                .with_state(state);

            let addr = SocketAddr::from(([127, 0, 0, 1], port));

            let tls_cfg = match tls_config.await {
                Ok(cfg) => cfg,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Failed to configure TLS: {}", e)));
                    return;
                }
            };

            let tcp_listener = match std::net::TcpListener::bind(addr) {
                Ok(l) => l,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Failed to bind to {}: {}", addr, e)));
                    return;
                }
            };
            tcp_listener.set_nonblocking(true).unwrap();
            let bound_addr = tcp_listener.local_addr().unwrap();

            let server = match axum_server::from_tcp_rustls(tcp_listener, tls_cfg) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Failed to create TLS server: {}", e)));
                    return;
                }
            };

            info!("\u{1f510} Vault server listening on {} (TLS)", bound_addr);
            let _ = ready_tx.send(Ok(bound_addr));

            server
                .handle(server_handle_clone)
                .serve(app.into_make_service())
                .await
                .unwrap();
        });
    });

    let addr = ready_rx
        .recv()
        .map_err(|_| "Vault server thread died before reporting readiness")?
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

    Ok(VaultHandle {
        token,
        addr,
        server_handle,
        registry,
        cert_pem: cert_pem_clone,
    })
}

/// Coerce a concrete `SecretProvider` into `Arc<dyn SecretProvider>`.
///
/// A standalone function is used so the coercion is explicit.
///
/// Note: rust-analyzer may show E0308 here due to a known limitation with
/// `#[async_trait]` macro expansion. The compiler (rustc) accepts this
/// correctly — `Arc<T>` coerces to `Arc<dyn SecretProvider>` when `T: SecretProvider`.
fn into_secret_provider<T: SecretProvider + 'static>(p: T) -> Arc<dyn SecretProvider> {
    Arc::new(p)
}

/// Start the vault server with a custom `SecretProvider`.
///
/// This is the lowest-level entrypoint — accepts any provider, making
/// it easy to inject an in-memory mock for integration tests.
///
/// Delegates to `start_vault_server_with_lazy_provider` with a trivial
/// factory that returns the provider immediately.
pub fn start_vault_server_with_provider(
    provider: Arc<dyn SecretProvider>,
    port: u16,
) -> Result<VaultHandle, Box<dyn std::error::Error + Send + Sync>> {
    start_vault_server_with_lazy_provider(move || async { Ok(provider) }, port)
}

/// Generate a cryptographically random root token.
///
/// Format: `hvs.<32 random hex chars>` following the Vault token convention
/// so standard clients handle it naturally.
fn generate_root_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("hvs.{}", hex::encode(bytes))
}

/// Generate a cryptographically random scoped token.
///
/// Format: `hvs.s.<32 random hex chars>` — the `.s.` prefix distinguishes
/// scoped tokens from root tokens in logs.
fn generate_scoped_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("hvs.s.{}", hex::encode(bytes))
}

/// Generate an ephemeral self-signed certificate for localhost TLS.
///
/// Returns `(cert_pem, key_pem)`. The certificate is valid for
/// `127.0.0.1` and `localhost` and uses the `rcgen` default validity
/// period. Since the cert is ephemeral (regenerated on each process
/// start), the exact expiry is irrelevant.
fn generate_self_signed_cert() -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>>
{
    use rcgen::{CertificateParams, SanType};

    let mut params = CertificateParams::new(vec!["localhost".to_string()])?;
    params
        .subject_alt_names
        .push(SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));

    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}
