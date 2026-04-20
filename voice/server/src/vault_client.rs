//! Vault client — resolves agent secrets from a Vault KV v2-compatible server.
//!
//! Uses the `vaultrs` crate (standard HashiCorp Vault client) to read
//! decrypted credentials at session start time. The vault server is operated
//! by `integrations` and speaks the Vault KV v2 API.
//!
//! voice-server only knows the vault address and token — it has zero knowledge
//! of databases, encryption keys, or credential internals.

use std::collections::HashMap;

use agent_kit::agent_backends::SecretMap;
use secrecy::SecretString;
use tracing::{debug, info, warn};
use uuid::Uuid;
use vaultrs::client::{VaultClient, VaultClientSettingsBuilder};

/// Resolve secrets for an agent from the vault server.
///
/// Reads from the Vault KV v2 secret engine at `secret/{agent_id}`.
/// Returns a `SecretMap` — the caller is responsible for wrapping in
/// the appropriate shared container (`Arc<RwLock<…>>`).
///
/// # Arguments
/// * `vault_addr` — Vault server URL (e.g. `https://127.0.0.1:<port>`)
/// * `vault_token` — Root or scoped vault token
/// * `agent_id` — Agent UUID to resolve secrets for
pub async fn resolve_secrets(
    vault_addr: &str,
    vault_token: &str,
    agent_id: Uuid,
) -> Result<SecretMap, VaultResolveError> {
    let client = VaultClient::new(
        VaultClientSettingsBuilder::default()
            .address(vault_addr)
            .token(vault_token)
            .build()
            .map_err(|e| VaultResolveError::ClientConfig(e.to_string()))?,
    )
    .map_err(|e| VaultResolveError::ClientConfig(e.to_string()))?;

    let agent_id_str = agent_id.to_string();

    // Read from KV v2 at mount "secret", path = agent_id
    let data: HashMap<String, String> =
        match vaultrs::kv2::read(&client, "secret", &agent_id_str).await {
            Ok(d) => d,
            Err(vaultrs::error::ClientError::APIError { code: 404, .. }) => {
                // No secrets stored for this agent — return empty map
                debug!(
                    agent_id = %agent_id,
                    "No vault secrets found for agent (404)"
                );
                return Ok(SecretMap::new());
            }
            Err(vaultrs::error::ClientError::APIError { code: 403, .. }) => {
                // Scoped token has expired.
                warn!(
                    agent_id = %agent_id,
                    "Vault scoped token expired (403)"
                );
                return Err(VaultResolveError::TokenExpired);
            }
            Err(e) => {
                warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "Failed to read secrets from vault"
                );
                return Err(VaultResolveError::ReadFailed(e.to_string()));
            }
        };

    // Convert HashMap<String, String> → SecretMap (HashMap<String, SecretString>)
    let secret_map: SecretMap = data
        .into_iter()
        .map(|(k, v)| (k, SecretString::from(v)))
        .collect();

    info!(
        agent_id = %agent_id,
        num_secrets = secret_map.len(),
        "Resolved secrets from vault"
    );

    Ok(secret_map)
}

/// Errors that can occur during vault secret resolution.
#[derive(Debug, thiserror::Error)]
pub enum VaultResolveError {
    #[error("vault client configuration error: {0}")]
    ClientConfig(String),
    #[error("failed to read secrets from vault: {0}")]
    ReadFailed(String),
    /// The scoped vault token has expired (HTTP 403).
    #[error("vault scoped token expired")]
    TokenExpired,
}
