from typing import Any

class EncryptionEngine:
    def __init__(self, key_b64: str) -> None:
        """Create a new encryption engine from a base64-encoded 32-byte master key."""
        ...

    def encrypt(self, data: dict[str, Any]) -> tuple[str, str]:
        """Encrypt a JSON-serializable Python dictionary.

        Returns:
            tuple[str, str]: (ciphertext_b64, nonce_b64)
        """
        ...

    def decrypt(self, ciphertext_b64: str, nonce_b64: str) -> dict[str, Any]:
        """Decrypt ciphertext/nonce strings back into a Python dictionary."""
        ...

def resolve_token(
    db_url: str,
    secret_key: str,
    integration_path: str,
    provider: str,
    agent_id: str,
) -> str:
    """Resolve a valid access token for a provider, refreshing if expired.

    Checks the stored credential, and if the token has expired,
    refreshes it using the OAuth refresh_token flow before returning.
    For non-OAuth credentials (API keys), returns the stored value directly.

    Args:
        db_url: PostgreSQL connection string.
        secret_key: Base64-encoded encryption key.
        integration_path: Path to integrations.yaml.
        provider: Integration slug (e.g. 'airtable', 'slack').
        agent_id: UUID string, or empty string for platform default.

    Returns:
        The valid access token string.

    Raises:
        ValueError: If no credential is found or refresh fails.
    """
    ...

def resolve_agent_secrets(
    db_url: str,
    secret_key: str,
    agent_id: str,
) -> dict[str, str]:
    """Resolve the full secret map for an agent, including platform defaults.

    Returns the same flattened keys used by the voice runtime, including both
    ``provider`` and ``provider.field`` entries.
    """
    ...

def start_vault_server(
    db_url: str, secret_key: str, port: int = 0
) -> tuple[str, str, str]:
    """Start the vault server on a background thread.

    Returns:
        tuple[str, str, str]: (vault_token, vault_url, cert_pem)
    """
    ...

def stop_vault_server() -> None:
    """Stop the vault server (if running)."""
    ...

def create_scoped_token(agent_id: str, ttl_seconds: int = 7200) -> str:
    """Create a scoped token that only allows reading secrets for a specific agent.

    Args:
        agent_id: UUID string of the agent.
        ttl_seconds: Time-to-live in seconds (default: 2 hours).

    Returns:
        str: The scoped token string.
    """
    ...

def embedded_integrations_yaml() -> str:
    """Return the integrations.yaml content baked into the binary at compile time.

    Use this instead of reading a file from disk:

        import integrations, yaml
        registry = yaml.safe_load(integrations.embedded_integrations_yaml())
    """
    ...
