//! Token Refresh Service — background task that refreshes expiring OAuth tokens.
//!
//! Runs as a `tokio::spawn`ed loop inside the owning service process.
//! On each tick (default 60 s) it queries for credentials whose
//! `token_expires_at` is within the refresh margin, then exchanges
//! refresh tokens for new access tokens.
//!
//! Locking: the entire batch runs inside one Postgres transaction.
//! `FOR UPDATE SKIP LOCKED` ensures only one worker processes each
//! credential, and the lock is held until commit/rollback.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::encryption::EncryptionEngine;
use crate::registry::ProviderRegistry;
use crate::secret_store::CredentialRow;

// ── Configuration ─────────────────────────────────────────────

const DEFAULT_CHECK_INTERVAL_SECS: u64 = 60;
const DEFAULT_MARGIN_MINUTES: i64 = 5;
const COOLDOWN_SECONDS: i64 = 60;
const MAX_ATTEMPTS: i32 = 5;
const BATCH_SIZE: i32 = 10;

/// OAuth client credentials for a single integration, sourced from the
/// `oauth_apps` DB table.
#[derive(Clone)]
pub struct OAuthClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for OAuthClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClientConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .finish()
    }
}

/// Background service that refreshes expiring OAuth credentials.
pub struct TokenRefresher {
    pool: PgPool,
    engine: EncryptionEngine,
    registry: ProviderRegistry,
    http: reqwest::Client,
    check_interval: Duration,
}

#[derive(Debug, sqlx::FromRow)]
struct OAuthAppRow {
    integration_name: String,
    client_id: String,
    client_secret_encrypted: String,
    client_secret_iv: String,
}

impl TokenRefresher {
    /// Create a new token refresher.
    pub fn new(pool: PgPool, engine: EncryptionEngine, registry: ProviderRegistry) -> Self {
        Self {
            pool,
            engine,
            registry,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("Failed to build HTTP client"),
            check_interval: Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS),
        }
    }

    /// Override the default check interval.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.check_interval = interval;
        self
    }

    /// Background cron loop — meant to be wrapped in `tokio::spawn`.
    ///
    /// ```ignore
    /// let refresher = TokenRefresher::new(pool, engine, registry);
    /// tokio::spawn(async move { refresher.run_cron().await });
    /// ```
    pub async fn run_cron(self) {
        info!(
            interval_secs = self.check_interval.as_secs(),
            "Token refresh service started"
        );
        let mut ticker = tokio::time::interval(self.check_interval);
        loop {
            ticker.tick().await;
            match self.refresh_stale_batch().await {
                Ok(0) => {} // nothing to refresh
                Ok(count) => info!(refreshed = count, "Token refresh batch complete"),
                Err(e) => error!(error = %e, "Token refresh batch failed"),
            }
        }
    }

    /// Fetch stale credentials and refresh them inside a single transaction.
    ///
    /// The `FOR UPDATE SKIP LOCKED` rows are locked until the transaction
    /// commits, preventing other workers from processing the same rows.
    pub async fn refresh_stale_batch(
        &self,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        // Use the maximum per-provider margin so we don't miss any provider
        // whose configured margin exceeds the global default.
        let margin_minutes = self.max_refresh_margin_minutes();

        let mut tx = self.pool.begin().await?;

        // Fetch candidates — locked until commit.
        // NULL token_expires_at is treated as "already expired" so we attempt
        // to refresh credentials that were stored without a known expiry.
        let stale = sqlx::query_as::<_, CredentialRow>(
            "SELECT id, agent_id, name, provider, auth_type, encrypted_data,
                    encryption_iv, encryption_version, token_expires_at,
                    last_refresh_success, last_refresh_failure, last_refresh_error,
                    refresh_attempts, refresh_exhausted, last_fetched_at
             FROM credentials
             WHERE auth_type = 'oauth2'
               AND refresh_exhausted = FALSE
               AND encryption_version = 2
               AND (token_expires_at IS NULL
                    OR token_expires_at < NOW() + $1 * INTERVAL '1 minute')
               AND (last_refresh_failure IS NULL
                    OR last_refresh_failure < NOW() - $2 * INTERVAL '1 second')
             FOR UPDATE SKIP LOCKED
             LIMIT $3",
        )
        .bind(margin_minutes)
        .bind(COOLDOWN_SECONDS)
        .bind(BATCH_SIZE)
        .persistent(false)
        .fetch_all(&mut *tx)
        .await?;

        if stale.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }

        let mut providers: Vec<String> = stale.iter().map(|cred| cred.provider.clone()).collect();
        providers.sort_unstable();
        providers.dedup();

        // Load OAuth client credentials fresh from the `oauth_apps` table on
        // every tick so newly-configured apps are picked up without a restart.
        let client_configs =
            load_client_configs_from_db(&self.pool, &self.engine, &providers).await?;

        let mut refreshed = 0u32;
        for cred in &stale {
            if !client_configs.contains_key(&cred.provider) {
                warn!(
                    credential_id = %cred.id,
                    provider = %cred.provider,
                    "Skipping refresh: no enabled OAuth app config for provider"
                );
                continue;
            }

            match self.do_refresh(&mut tx, cred, &client_configs).await {
                Ok(()) => refreshed += 1,
                Err(e) => {
                    warn!(
                        credential_id = %cred.id,
                        provider = %cred.provider,
                        error = %e,
                        "Token refresh failed"
                    );
                    // Record failure inside the same transaction
                    let new_attempts = cred.refresh_attempts + 1;
                    let exhausted = new_attempts >= MAX_ATTEMPTS;
                    let error_msg = e.to_string();
                    sqlx::query(
                        "UPDATE credentials SET
                            last_refresh_failure = NOW(),
                            last_refresh_error = $1,
                            refresh_attempts = $2,
                            refresh_exhausted = $3
                         WHERE id = $4",
                    )
                    .bind(&error_msg[..error_msg.len().min(500)])
                    .bind(new_attempts)
                    .bind(exhausted)
                    .bind(cred.id)
                    .persistent(false)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        error!(
                            credential_id = %cred.id,
                            error = %e,
                            "Failed to record refresh failure in DB"
                        );
                    })
                    .ok();

                    if exhausted {
                        warn!(
                            credential_id = %cred.id,
                            "Refresh exhausted after {} attempts", MAX_ATTEMPTS
                        );
                    }
                }
            }
        }

        tx.commit().await?;
        Ok(refreshed)
    }

    /// Refresh a single credential within an active transaction.
    ///
    /// Delegates to the shared `do_refresh_row` function.
    async fn do_refresh(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        cred: &CredentialRow,
        client_configs: &HashMap<String, OAuthClientConfig>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        do_refresh_row(
            &mut **tx,
            &self.engine,
            &self.registry,
            client_configs,
            &self.http,
            cred,
        )
        .await?;
        Ok(())
    }

    /// Compute the refresh margin as the maximum of the global default and
    /// all per-provider `refresh_margin_minutes` values in the registry.
    /// This ensures no provider's tokens expire before we attempt a refresh.
    fn max_refresh_margin_minutes(&self) -> i64 {
        self.registry
            .integrations
            .values()
            .filter(|p| p.auth.auth_type == "oauth2")
            .map(|p| i64::from(p.auth.refresh_margin_minutes))
            .max()
            .unwrap_or(DEFAULT_MARGIN_MINUTES)
            .max(DEFAULT_MARGIN_MINUTES)
    }
}

async fn load_client_configs_from_db(
    pool: &PgPool,
    engine: &EncryptionEngine,
    providers: &[String],
) -> Result<HashMap<String, OAuthClientConfig>, Box<dyn std::error::Error + Send + Sync>> {
    if providers.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, OAuthAppRow>(
        "SELECT integration_name, client_id, client_secret_encrypted, client_secret_iv
         FROM oauth_apps
         WHERE enabled = TRUE
           AND integration_name = ANY($1)",
    )
    .bind(providers)
    .persistent(false)
    .fetch_all(pool)
    .await?;

    let mut configs = HashMap::with_capacity(rows.len());
    for row in rows {
        let integration_name = row.integration_name.clone();
        let decrypted =
            match engine.decrypt_json(&row.client_secret_encrypted, &row.client_secret_iv) {
                Ok(data) => data,
                Err(e) => {
                    warn!(
                        provider = %integration_name,
                        error = %e,
                        "Skipping invalid oauth_app: failed to decrypt client secret"
                    );
                    continue;
                }
            };

        let Some(client_secret) = decrypted.get("client_secret").and_then(|v| v.as_str()) else {
            warn!(
                provider = %integration_name,
                "Skipping invalid oauth_app: missing client_secret"
            );
            continue;
        };

        configs.insert(
            integration_name,
            OAuthClientConfig {
                client_id: row.client_id,
                client_secret: client_secret.to_string(),
            },
        );
    }
    Ok(configs)
}

// ── Standalone refresh function ──────────────────────────────────

/// Refresh a single credential row, writing updated tokens back to DB.
///
/// This is the single source of truth for the refresh logic — used by both
/// the background `TokenRefresher` cron and the on-demand `resolve_token` path.
async fn do_refresh_row<'e, E>(
    executor: E,
    engine: &EncryptionEngine,
    registry: &ProviderRegistry,
    client_configs: &HashMap<String, OAuthClientConfig>,
    http: &reqwest::Client,
    cred: &CredentialRow,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    // 1. Look up integration config
    let provider_config = registry
        .get(&cred.provider)
        .ok_or_else(|| format!("Unknown integration: {}", cred.provider))?;
    let auth = &provider_config.auth;
    let token_url = auth
        .token_url
        .as_ref()
        .ok_or_else(|| format!("No token_url for {}", cred.provider))?;

    // 2. Decrypt current credential to get refresh_token
    let iv = cred
        .encryption_iv
        .as_deref()
        .ok_or("Missing encryption IV")?;
    let current_data = engine.decrypt_json(&cred.encrypted_data, iv)?;
    let refresh_token = current_data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or("No refresh_token in credential")?;

    // 3. Get client credentials
    let client_config = client_configs
        .get(&cred.provider)
        .ok_or_else(|| format!("No OAuth client config for {}", cred.provider))?;

    // 4. Build refresh request from integrations.yaml refresh_params
    let mut form: HashMap<String, String> = auth.refresh_params.clone();
    form.insert("refresh_token".to_string(), refresh_token.to_string());

    let mut req = http.post(token_url);
    if auth.client_auth_method == "header" {
        req = req.basic_auth(&client_config.client_id, Some(&client_config.client_secret));
    } else {
        form.insert("client_id".to_string(), client_config.client_id.clone());
        form.insert(
            "client_secret".to_string(),
            client_config.client_secret.clone(),
        );
    }

    // 5. Exchange refresh token
    let resp = req.form(&form).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Token refresh returned {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        )
        .into());
    }

    let tokens: serde_json::Value = resp.json().await?;
    let new_access = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("Refresh response missing access_token")?;

    // 6. Preserve refresh_token if not rotated
    let new_refresh = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or(refresh_token);

    let new_data = serde_json::json!({
        "access_token": new_access,
        "refresh_token": new_refresh,
    });

    // 7. Calculate new expiry
    let expires_in = parse_expires_in(&tokens);
    let expires_at = Utc::now() + chrono::Duration::seconds(expires_in);

    // 8. Re-encrypt and store
    let (ciphertext, new_iv) = engine.encrypt_json(&new_data)?;
    sqlx::query(
        "UPDATE credentials SET
            encrypted_data = $1,
            encryption_iv = $2,
            encryption_version = 2,
            token_expires_at = $3,
            last_refresh_success = NOW(),
            last_refresh_failure = NULL,
            last_refresh_error = NULL,
            refresh_attempts = 0
         WHERE id = $4",
    )
    .bind(&ciphertext)
    .bind(&new_iv)
    .bind(expires_at)
    .bind(cred.id)
    .persistent(false)
    .execute(executor)
    .await?;

    info!(
        credential_id = %cred.id,
        provider = %cred.provider,
        expires_in_secs = expires_in,
        "Token refreshed"
    );

    Ok(new_access.to_string())
}

// ── On-demand token resolution ───────────────────────────────────

/// Find the credential for a provider, checking agent-specific first,
/// then falling back to platform default.
async fn find_credential(
    pool: &PgPool,
    provider: &str,
    agent_id: Option<uuid::Uuid>,
) -> Result<CredentialRow, Box<dyn std::error::Error + Send + Sync>> {
    sqlx::query_as::<_, CredentialRow>(
        "SELECT c.id, c.agent_id, c.name, c.provider, c.auth_type, c.encrypted_data,
                c.encryption_iv, c.encryption_version, c.token_expires_at,
                c.last_refresh_success, c.last_refresh_failure, c.last_refresh_error,
                c.refresh_attempts, c.refresh_exhausted, c.last_fetched_at
         FROM credentials c
         LEFT JOIN agents a ON a.id = $2
         WHERE c.provider = $1
           AND (
               c.agent_id = $2
               OR (c.agent_id IS NULL AND (c.workspace_id = a.workspace_id OR c.workspace_id IS NULL))
           )
         ORDER BY 
            c.agent_id NULLS LAST,
            c.workspace_id NULLS LAST,
            c.created_at DESC
         LIMIT 1"
    )
    .bind(provider)
    .bind(agent_id)
    .persistent(false)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| format!("No credential found for provider '{}'", provider).into())
}

/// Resolve a valid access token for a provider, refreshing if expired.
///
/// This is the main entry point for Python's `api_call` tool. It:
/// 1. Queries the credential (agent-specific first, then platform default)
/// 2. Decrypts the stored data to get the access_token
/// 3. Checks `token_expires_at` — if expired or about to expire, refreshes
/// 4. Returns the (possibly fresh) access_token
///
/// For non-OAuth credentials (API keys), expiry is not checked — the
/// stored value is returned directly.
pub async fn resolve_token(
    pool: &PgPool,
    engine: &EncryptionEngine,
    registry: &ProviderRegistry,
    provider: &str,
    agent_id: Option<uuid::Uuid>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // 1. Find the credential — agent-specific first, then default
    let cred = find_credential(pool, provider, agent_id).await?;

    // 2. Update last_fetched_at so the cron knows this credential is active
    sqlx::query("UPDATE credentials SET last_fetched_at = NOW() WHERE id = $1")
        .bind(cred.id)
        .persistent(false)
        .execute(pool)
        .await
        .ok();

    // 3. Decrypt and extract token
    let iv = cred
        .encryption_iv
        .as_deref()
        .ok_or("Missing encryption IV")?;
    let data = engine.decrypt_json(&cred.encrypted_data, iv)?;

    // For non-OAuth (API keys), return directly — no expiry check
    if cred.auth_type != "oauth2" {
        let token = extract_token(&data).ok_or("No usable token found in credential data")?;
        return Ok(token);
    }

    // 4. Check expiry — refresh if expired or within 60s margin
    let needs_refresh = match cred.token_expires_at {
        Some(expires) => Utc::now() + chrono::Duration::seconds(60) > expires,
        None => true, // No expiry recorded — try refresh to be safe
    };

    if needs_refresh {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let providers = vec![provider.to_string()];
        let client_configs = load_client_configs_from_db(pool, engine, &providers).await?;

        match do_refresh_row(pool, engine, registry, &client_configs, &http, &cred).await {
            Ok(fresh_token) => return Ok(fresh_token),
            Err(e) => {
                warn!(
                    provider = %provider,
                    error = %e,
                    "On-demand token refresh failed, returning stored token"
                );
                // Fall through — return the (possibly expired) stored token
                // so the caller can at least try and get a 401
            }
        }
    }

    // 5. Return the stored access_token
    let token = extract_token(&data).ok_or("No usable token found in credential data")?;
    Ok(token)
}

/// Ensure all OAuth tokens for a specific agent are fresh.
///
/// Queries all OAuth providers used by the agent (or platform defaults)
/// and triggers `resolve_token` which refreshes them if they are about to expire.
/// Used at session startup to guarantee Vault reads fresh tokens.
pub async fn ensure_agent_tokens_fresh(
    pool: &PgPool,
    engine: &EncryptionEngine,
    registry: &ProviderRegistry,
    agent_id: uuid::Uuid,
) -> Result<(), sqlx::Error> {
    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM credentials 
         WHERE auth_type = 'oauth2' 
           AND (agent_id = $1 OR agent_id IS NULL)",
    )
    .bind(agent_id)
    .persistent(false)
    .fetch_all(pool)
    .await?;

    for provider in providers {
        // Ignore errors, if it fails to refresh we just move on
        let _ = resolve_token(pool, engine, registry, &provider, Some(agent_id)).await;
    }

    Ok(())
}

/// Extract a usable token string from decrypted credential data.
fn extract_token(data: &serde_json::Value) -> Option<String> {
    for key in &["access_token", "api_token", "bot_token", "api_key", "token"] {
        if let Some(v) = data.get(key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // Fallback: first non-empty string value, excluding fields that are
    // definitely not tokens (e.g. `token_type: "Bearer"`, `scope: "read"`).
    const NON_TOKEN_KEYS: &[&str] = &[
        "token_type",
        "scope",
        "expires_in",
        "refresh_token",
        "grant_type",
        "token_uri",
        "client_id",
        "client_secret",
    ];
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            if NON_TOKEN_KEYS.contains(&k.as_str()) {
                continue;
            }
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Parse `expires_in` from a token response, handling both numeric and
/// string representations. Falls back to 3600 (1 hour) if absent or
/// unparseable.
fn parse_expires_in(tokens: &serde_json::Value) -> i64 {
    tokens
        .get("expires_in")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(3600)
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_client_config_redacts_secret() {
        let config = OAuthClientConfig {
            client_id: "test-id".to_string(),
            client_secret: "super-secret".to_string(),
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("test-id"));
        assert!(!debug.contains("super-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn parse_expires_in_numeric() {
        let tokens = serde_json::json!({ "expires_in": 1800 });
        assert_eq!(parse_expires_in(&tokens), 1800);
    }

    #[test]
    fn parse_expires_in_string() {
        let tokens = serde_json::json!({ "expires_in": "7200" });
        assert_eq!(parse_expires_in(&tokens), 7200);
    }

    #[test]
    fn parse_expires_in_missing_defaults_to_3600() {
        let tokens = serde_json::json!({ "access_token": "abc" });
        assert_eq!(parse_expires_in(&tokens), 3600);
    }

    #[test]
    fn parse_expires_in_invalid_string_defaults_to_3600() {
        let tokens = serde_json::json!({ "expires_in": "not_a_number" });
        assert_eq!(parse_expires_in(&tokens), 3600);
    }
}
