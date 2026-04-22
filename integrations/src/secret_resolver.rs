//! Secret resolver — load and decrypt all credentials for a voice session.
//!
//! This replaces Python's session credential loader.
//! Called once at session start — returns `Arc<SecretMap>` for zero-copy
//! sharing across the session lifetime.

use std::collections::HashMap;
use std::sync::Arc;

use secrecy::SecretString;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::encryption::EncryptionEngine;

/// Secrets map: provider key → secret value.
/// This is the same type used by `agent-kit` for `AgentBackendConfig.secrets`.
pub type SecretMap = HashMap<String, SecretString>;

fn primary_secret_value(data: &HashMap<String, String>) -> Option<&String> {
    const PREFERRED_KEYS: &[&str] = &[
        "access_token",
        "api_key",
        "token",
        "bot_token",
        "bearer_token",
        "client_secret",
        "password",
        "key",
        "refresh_token",
    ];

    for key in PREFERRED_KEYS {
        if let Some(value) = data.get(*key) {
            return Some(value);
        }
    }

    let first_key = data.keys().min()?;
    data.get(first_key)
}

/// Resolve decrypted secrets for a voice session.
///
/// Loads credentials for the given agent **and** platform-wide defaults
/// (`agent_id IS NULL`). Agent-specific credentials take precedence over
/// defaults when both exist for the same provider.
///
/// Returns a flat map following the convention:
///   - `"<provider>"` → primary field value (first field)
///   - `"<provider>.<field_key>"` → individual field value
///
/// Wraps the result in `Arc` for cheap sharing across the session.
///
/// # Errors
///
/// Returns `sqlx::Error` if the database query fails. Individual credential
/// decryption failures are logged and skipped (the session can still use
/// credentials that were successfully decrypted).
pub async fn resolve_secrets(
    pool: &PgPool,
    engine: &EncryptionEngine,
    agent_id: Uuid,
) -> Result<Arc<SecretMap>, sqlx::Error> {
    // Use .persistent(false) to avoid sending PREPARE/EXECUTE. This is
    // required for PgBouncer in transaction-pooling mode (e.g. Supabase)
    // where prepared statements leak across connections.
    //
    // Fetch both agent-specific, workspace-wide, and platform-wide default credentials.
    // ORDER BY specificity ensures we see the most specific credential first:
    // 1. Agent-specific (agent_id IS NOT NULL)
    // 2. Workspace-specific (workspace_id IS NOT NULL)
    // 3. Global (workspace_id IS NULL)
    let rows = sqlx::query_as::<_, CredentialRow>(
        "SELECT c.provider, c.encrypted_data, c.encryption_iv, c.encryption_version,
                CASE WHEN c.agent_id IS NOT NULL THEN true ELSE false END AS is_agent_specific
         FROM credentials c
         LEFT JOIN agents a ON a.id = $1
         WHERE c.agent_id = $1 
            OR (c.agent_id IS NULL AND (c.workspace_id = a.workspace_id OR c.workspace_id IS NULL))
         ORDER BY 
            c.agent_id NULLS LAST,
            c.workspace_id NULLS LAST,
            c.created_at DESC",
    )
    .bind(agent_id)
    .persistent(false)
    .fetch_all(pool)
    .await?;

    // Track which providers we have already processed so we keep only the
    // most specific credential for each.
    let mut seen_providers: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut secrets = SecretMap::new();
    for row in &rows {
        // Skip less specific credentials if we already have one for this provider.
        if seen_providers.contains(&row.provider) {
            info!(
                provider = %row.provider,
                "Skipping less specific credential"
            );
            continue;
        }

        match decrypt_credential(engine, row) {
            Ok(data) => {
                // Primary key: secret("provider") → preferred auth field
                if let Some(primary_value) = primary_secret_value(&data) {
                    secrets.insert(row.provider.clone(), primary_value.clone().into());
                }
                // Compound keys: secret("provider.field") → individual field value
                for (k, v) in &data {
                    secrets.insert(format!("{}.{}", row.provider, k), v.clone().into());
                }
                
                seen_providers.insert(row.provider.clone());
                
                info!(
                    provider = %row.provider,
                    fields = data.len(),
                    is_default = !row.is_agent_specific,
                    "Resolved credential"
                );
            }
            Err(e) => {
                warn!(
                    provider = %row.provider,
                    error = %e,
                    "Failed to decrypt credential — skipping"
                );
            }
        }
    }

    Ok(Arc::new(secrets))
}

/// Lightweight row for credential resolution (only the fields we need).
#[derive(sqlx::FromRow)]
struct CredentialRow {
    provider: String,
    encrypted_data: String,
    encryption_iv: Option<String>,
    encryption_version: i32,
    /// True when the credential is agent-specific; false for platform defaults.
    is_agent_specific: bool,
}

/// Decrypt a credential row based on its encryption version.
fn decrypt_credential(
    engine: &EncryptionEngine,
    row: &CredentialRow,
) -> Result<HashMap<String, String>, crate::encryption::EncryptionError> {
    match row.encryption_version {
        2 => {
            // AES-256-GCM (integrations)
            let iv = row
                .encryption_iv
                .as_deref()
                .ok_or(crate::encryption::EncryptionError::DecryptFailed)?;
            let json = engine.decrypt_json(&row.encrypted_data, iv)?;
            let map: HashMap<String, String> = serde_json::from_value(json)
                .map_err(|_| crate::encryption::EncryptionError::InvalidJson)?;
            Ok(map)
        }
        _ => {
            // Legacy Fernet (version 1) or unknown — cannot decrypt here
            Err(crate::encryption::EncryptionError::DecryptFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::primary_secret_value;
    use std::collections::HashMap;

    #[test]
    fn primary_secret_prefers_access_token_over_refresh_token() {
        let mut data = HashMap::new();
        data.insert("refresh_token".to_string(), "refresh".to_string());
        data.insert("access_token".to_string(), "access".to_string());

        assert_eq!(
            primary_secret_value(&data).map(String::as_str),
            Some("access")
        );
    }

    #[test]
    fn primary_secret_prefers_api_key_over_config_fields() {
        let mut data = HashMap::new();
        data.insert("spreadsheet_id".to_string(), "sheet-123".to_string());
        data.insert("api_key".to_string(), "secret-key".to_string());

        assert_eq!(
            primary_secret_value(&data).map(String::as_str),
            Some("secret-key")
        );
    }

    #[test]
    fn primary_secret_uses_stable_sorted_fallback() {
        let mut data = HashMap::new();
        data.insert("z_field".to_string(), "z".to_string());
        data.insert("a_field".to_string(), "a".to_string());

        assert_eq!(primary_secret_value(&data).map(String::as_str), Some("a"));
    }
}
