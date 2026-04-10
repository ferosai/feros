//! Python bindings for Integrations.
//!
//! Exposes the Rust `EncryptionEngine` via PyO3 so the Python FastAPI
//! backend and Rust WebRTC engine share identical AES-256-GCM logic.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use secrecy::ExposeSecret;
use sqlx::PgPool;

use crate::encryption::EncryptionEngine;

/// Python wrapper for the AES-256-GCM encryption engine.
#[pyclass(name = "EncryptionEngine")]
pub struct PyEncryptionEngine {
    inner: EncryptionEngine,
}

#[pymethods]
impl PyEncryptionEngine {
    /// Create a new encryption engine from a base64-encoded 32-byte master key.
    #[new]
    fn new(key_b64: &str) -> PyResult<Self> {
        let inner = EncryptionEngine::from_base64(key_b64)
            .map_err(|e| PyValueError::new_err(format!("Invalid encryption key: {}", e)))?;
        Ok(Self { inner })
    }

    /// Encrypt a JSON-serializable Python dictionary.
    ///
    /// Returns a tuple: `(ciphertext_b64, nonce_b64)`.
    fn encrypt<'py>(
        &self,
        py: Python<'py>,
        data: &Bound<'py, PyDict>,
    ) -> PyResult<(String, String)> {
        // Convert PyDict -> JSON String via Python "json" module
        let json_str: String = py
            .import("json")?
            .getattr("dumps")?
            .call1((data,))?
            .extract()?;

        let json_val: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse dict as JSON: {}", e)))?;

        let (ct, nonce) = self
            .inner
            .encrypt_json(&json_val)
            .map_err(|e| PyValueError::new_err(format!("Encryption failed: {}", e)))?;

        Ok((ct, nonce))
    }

    /// Decrypt ciphertext/nonce strings back into a Python dictionary.
    fn decrypt<'py>(
        &self,
        py: Python<'py>,
        ciphertext_b64: &str,
        nonce_b64: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let json_val = self
            .inner
            .decrypt_json(ciphertext_b64, nonce_b64)
            .map_err(|e| PyValueError::new_err(format!("Decryption failed: {}", e)))?;

        let json_str = serde_json::to_string(&json_val)
            .map_err(|e| PyValueError::new_err(format!("JSON serialization failed: {}", e)))?;

        // Convert JSON String -> PyDict via Python "json" module
        py.import("json")?.getattr("loads")?.call1((json_str,))
    }
}

/// Start the token refresher background task in a detached OS thread.
///
/// This creates a new `tokio::Runtime` entirely separated from Python's
/// `asyncio` loop, allowing high-performance lock-contention (`FOR UPDATE SKIP LOCKED`)
/// without blocking the FastAPI server.
#[pyfunction]
fn start_token_refresher(
    db_url: String,
    secret_key: String,
    integration_path: String,
) -> PyResult<()> {
    // Channel to synchronously wait for the background thread's initialization result.
    let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = init_tx.send(Err(format!("failed to create tokio runtime: {e}")));
                return;
            }
        };

        rt.block_on(async move {
            let connect_opts: sqlx::postgres::PgConnectOptions = match db_url.parse() {
                Ok(o) => o,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("invalid DB URL: {e}")));
                    return;
                }
            };

            let pool = match sqlx::postgres::PgPoolOptions::new()
                .max_connections(3)
                .connect_with(connect_opts)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("failed to connect to DB: {e}")));
                    return;
                }
            };

            let engine = match crate::encryption::EncryptionEngine::from_base64(&secret_key) {
                Ok(e) => e,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("invalid encryption key: {e}")));
                    return;
                }
            };

            let registry = match crate::registry::ProviderRegistry::load(std::path::Path::new(
                &integration_path,
            )) {
                Ok(r) => r,
                Err(e) => {
                    let _ = init_tx.send(Err(format!(
                        "failed to load registry {integration_path}: {e}"
                    )));
                    return;
                }
            };

            let refresher = crate::token_refresh::TokenRefresher::new(pool, engine, registry);

            // Signal success — initialization complete.
            let _ = init_tx.send(Ok(()));

            // Loop infinitely in the background
            refresher.run_cron().await;
        });
    });

    // Block until the background thread finishes initialization.
    match init_rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(PyValueError::new_err(format!(
            "TokenRefresher init failed: {e}"
        ))),
        Err(_) => Err(PyValueError::new_err(
            "TokenRefresher thread exited before signaling init result",
        )),
    }
}

/// Resolve a valid access token for a provider, refreshing if expired.
///
/// This is the on-demand entry point for Python. It:
/// 1. Finds the credential (agent-specific first, then platform default)
/// 2. Checks if the token has expired
/// 3. If expired, refreshes using the OAuth refresh_token flow
/// 4. Returns the valid access_token
///
/// For non-OAuth credentials (API keys), the stored value is returned directly.
///
/// Blocks the calling thread until the operation completes.
#[pyfunction]
fn resolve_token(
    db_url: String,
    secret_key: String,
    integration_path: String,
    provider: String,
    agent_id: String,
) -> PyResult<String> {
    // Obtain (or lazily create) a cached runtime + pool.
    // Both live in the same OnceLock so the pool's background tasks always
    // have a live reactor.
    let cached = get_or_create_runtime_and_pool(&db_url)
        .map_err(|e| PyValueError::new_err(format!("DB pool init failed: {}", e)))?;

    cached.rt.block_on(async {
        let pool = cached.pool.clone();

        let engine = crate::encryption::EncryptionEngine::from_base64(&secret_key)
            .map_err(|e| PyValueError::new_err(format!("Encryption engine error: {}", e)))?;

        let registry =
            crate::registry::ProviderRegistry::load(std::path::Path::new(&integration_path))
                .map_err(|e| PyValueError::new_err(format!("Registry load error: {}", e)))?;

        // Parse agent_id — empty string means platform default (None)
        let aid = if agent_id.is_empty() {
            None
        } else {
            Some(
                uuid::Uuid::parse_str(&agent_id)
                    .map_err(|e| PyValueError::new_err(format!("Invalid agent_id: {}", e)))?,
            )
        };

        crate::token_refresh::resolve_token(&pool, &engine, &registry, &provider, aid)
            .await
            .map_err(|e| PyValueError::new_err(format!("Token resolution failed: {}", e)))
    })
}

/// Resolve all secrets for an agent, including platform defaults.
///
/// This reuses the canonical Rust secret resolver used by the vault server,
/// so Python callers get the same precedence and secret-shaping semantics as
/// production voice sessions.
#[pyfunction]
fn resolve_agent_secrets(
    db_url: String,
    secret_key: String,
    agent_id: String,
) -> PyResult<std::collections::HashMap<String, String>> {
    let cached = get_or_create_runtime_and_pool(&db_url)
        .map_err(|e| PyValueError::new_err(format!("DB pool init failed: {}", e)))?;

    cached.rt.block_on(async {
        let pool = cached.pool.clone();

        let engine = crate::encryption::EncryptionEngine::from_base64(&secret_key)
            .map_err(|e| PyValueError::new_err(format!("Encryption engine error: {}", e)))?;

        let aid = uuid::Uuid::parse_str(&agent_id)
            .map_err(|e| PyValueError::new_err(format!("Invalid agent_id: {}", e)))?;

        let secrets = crate::secret_resolver::resolve_secrets(&pool, &engine, aid)
            .await
            .map_err(|e| PyValueError::new_err(format!("Secret resolution failed: {}", e)))?;

        Ok(secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
            .collect())
    })
}

/// Cached tokio runtime + PgPool for `resolve_token`.
///
/// Both must share the same runtime so the pool's background connection
/// health-check tasks always have a live IO driver.  The pool is
/// configured properly so it works behind PgBouncer.
struct CachedRuntimePool {
    rt: tokio::runtime::Runtime,
    pool: PgPool,
}

/// Global cache for the runtime + pool.
///
/// `resolve_token` is called from `asyncio.to_thread` on the Python side,
/// so multiple OS threads may call concurrently.  `std::sync::Mutex` is
/// used because we need to hold the lock across the (synchronous)
/// `rt.block_on()` pool creation — `tokio::sync::Mutex` would require an
/// async context we don't have yet.
static RT_POOL_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(String, std::sync::Arc<CachedRuntimePool>)>>,
> = std::sync::OnceLock::new();

fn get_or_create_runtime_and_pool(
    db_url: &str,
) -> Result<std::sync::Arc<CachedRuntimePool>, Box<dyn std::error::Error + Send + Sync>> {
    let mutex = RT_POOL_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = mutex
        .lock()
        .map_err(|e| format!("Pool cache lock poisoned: {}", e))?;

    // Fast path: reuse existing runtime + pool if URL matches
    if let Some((ref cached_url, ref cached)) = *guard {
        if cached_url == db_url {
            return Ok(cached.clone());
        }
    }

    // Create a new multi-thread runtime that stays alive for the process
    // lifetime.  Unlike `current_thread`, this has its own worker threads
    // so the pool's background tasks (keepalive, health checks) run even
    // when no `block_on` call is active.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;

    // Create the pool on the new runtime
    let pool = rt.block_on(async {
        let connect_opts: sqlx::postgres::PgConnectOptions = db_url.parse().map_err(
            |e: sqlx::Error| -> Box<dyn std::error::Error + Send + Sync> {
                format!("Invalid DB URL: {}", e).into()
            },
        )?;

        sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect_with(connect_opts)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("DB connection failed: {}", e).into()
            })
    })?;

    let cached = std::sync::Arc::new(CachedRuntimePool { rt, pool });
    *guard = Some((db_url.to_string(), cached.clone()));
    Ok(cached)
}

/// Start the vault server on a background thread.
///
/// Returns `(token, url, cert_pem)` — the root token, the vault HTTPS URL,
/// and the PEM-encoded self-signed certificate for client trust pinning.
/// The server binds to `127.0.0.1:{port}` and speaks the Vault KV v2 API over TLS.
#[pyfunction]
#[pyo3(signature = (db_url, secret_key, port=0))]
fn start_vault_server(
    db_url: String,
    secret_key: String,
    port: u16,
) -> PyResult<(String, String, String)> {
    let handle = crate::vault::start_vault_server(db_url, secret_key, port)
        .map_err(|e| PyValueError::new_err(format!("Failed to start vault server: {}", e)))?;

    let token = handle.token.clone();
    let url = handle.url();
    let cert_pem = handle.cert_pem.clone();

    VAULT_HANDLE.lock().unwrap().replace(handle);

    Ok((token, url, cert_pem))
}

/// Stop the vault server (if running).
#[pyfunction]
fn stop_vault_server() -> PyResult<()> {
    if let Some(handle) = VAULT_HANDLE.lock().unwrap().take() {
        handle.shutdown();
    }
    Ok(())
}

/// Create a scoped token that only allows reading secrets for a specific agent.
///
/// The token expires after `ttl_seconds` (default: 7200 = 2 hours).
/// Requests with this token for a different agent ID will be rejected with 403.
///
/// The vault server must be running (call `start_vault_server` first).
#[pyfunction]
#[pyo3(signature = (agent_id, ttl_seconds=7200))]
fn create_scoped_token(agent_id: String, ttl_seconds: u64) -> PyResult<String> {
    let guard = VAULT_HANDLE.lock().unwrap();
    let handle = guard
        .as_ref()
        .ok_or_else(|| PyValueError::new_err("Vault server is not running"))?;

    let uuid = uuid::Uuid::parse_str(&agent_id)
        .map_err(|e| PyValueError::new_err(format!("Invalid agent_id: {}", e)))?;

    let token = handle.create_scoped_token(uuid, std::time::Duration::from_secs(ttl_seconds));
    Ok(token)
}

/// Global vault handle for lifecycle management.
static VAULT_HANDLE: std::sync::Mutex<Option<crate::vault::VaultHandle>> =
    std::sync::Mutex::new(None);

/// Return the `integrations.yaml` content baked into the binary at compile time.
///
/// Python can use this instead of reading a file from disk:
///
/// ```python
/// import integrations, yaml
/// registry = yaml.safe_load(integrations.embedded_integrations_yaml())
/// ```
#[pyfunction]
fn embedded_integrations_yaml() -> &'static str {
    include_str!("../integrations.yaml")
}

/// The initialization function for the Python module.
/// The `#[pymodule]` name must match the library name in `Cargo.toml`.
#[pymodule]
fn integrations(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEncryptionEngine>()?;
    m.add_function(wrap_pyfunction!(start_token_refresher, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_token, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_agent_secrets, m)?)?;
    m.add_function(wrap_pyfunction!(start_vault_server, m)?)?;
    m.add_function(wrap_pyfunction!(stop_vault_server, m)?)?;
    m.add_function(wrap_pyfunction!(create_scoped_token, m)?)?;
    m.add_function(wrap_pyfunction!(embedded_integrations_yaml, m)?)?;
    Ok(())
}
