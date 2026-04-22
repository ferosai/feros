"""Connection introspection tools — let the builder LLM make authenticated
API calls against connected integrations to discover user resources.

Two tools:
  - ``check_connection``  — lightweight DB lookup to see if a credential exists
  - ``api_call``          — authenticated HTTP request with auto-injected auth headers

Token resolution (including on-demand OAuth refresh) is delegated to
``integrations.resolve_token`` in the Rust extension — the Python side never
manually decrypts or refreshes tokens.

Security:
  - URL domain validated against ``api.base_url`` from ``integrations.yaml``
  - GET and POST only (no PUT/DELETE/PATCH)
  - Token never exposed to the LLM
  - Responses truncated to 8 KB
  - 15-second request timeout
"""

from __future__ import annotations

import asyncio
import json
import re
from typing import Any
from urllib.parse import urlparse

import httpx
from loguru import logger
from pydantic_ai import Agent, RunContext

import integrations
from app.agent_builder.deps import BuilderDeps
from app.lib.config import get_settings
from app.lib.config_utils import extract_secret_keys
from app.lib.credential_utils import find_credential, find_credentials_batch
from app.lib.database import async_session
from app.models.agent import Agent as DbAgent
from app.lib.integration_registry import (
    INTEGRATIONS_PATH,
    IntegrationConfig,
    integration_registry,
)

# Extra allowed API domains beyond the base_url in integrations.yaml.
# Google services in particular split across multiple subdomains.
_EXTRA_ALLOWED_DOMAINS: dict[str, list[str]] = {
    "google_sheets": ["www.googleapis.com", "sheets.googleapis.com"],
    "google_calendar": ["www.googleapis.com"],
    "google_docs": ["www.googleapis.com", "docs.googleapis.com"],
}

# Maximum response size returned to the LLM (characters).
_MAX_RESPONSE_SIZE = 8000

# Request timeout in seconds.
_REQUEST_TIMEOUT = 15.0


# ── Helpers ───────────────────────────────────────────────────────


async def _resolve_token(provider: str, agent_id: str) -> str | None:
    """Resolve a valid access token via the Rust integrations extension.

    Handles credential lookup (agent-specific → platform default),
    expiry checking, and on-demand OAuth refresh transparently.

    The Rust call blocks (creates its own tokio runtime), so we run
    it in a thread to avoid stalling the Python event loop.
    """
    settings = get_settings()
    # Strip the async driver suffix to get a plain psycopg URL for
    # the synchronous Rust integrations extension.  We split on the first
    # occurrence only and rejoin without the suffix to avoid mangling
    # URLs that might contain the substring elsewhere.
    raw_url = str(settings.database.url)
    db_url = raw_url.replace("+asyncpg", "", 1)

    try:
        return await asyncio.to_thread(
            integrations.resolve_token,
            db_url=db_url,
            secret_key=settings.auth.secret_key,
            integration_path=INTEGRATIONS_PATH,
            provider=provider,
            agent_id=agent_id or "",
        )
    except ValueError:
        return None


def _build_auth_headers(config: IntegrationConfig, token: str) -> dict[str, str]:
    """Build auth headers from integrations.yaml template, injecting the live token."""
    headers: dict[str, str] = {}
    if config.api and config.api.headers:
        for key, template in config.api.headers.items():
            # Template like "Bearer ${credentials.api_token}" → "Bearer sk-xxx…"
            resolved = re.sub(r"\$\{credentials\.\w+\}", token, template)
            # Normalize header name (e.g. "authorization" → "Authorization")
            header_name = "-".join(w.capitalize() for w in key.split("-"))
            headers[header_name] = resolved

    # Fallback: if no header template defined, use standard Bearer
    if not headers:
        headers["Authorization"] = f"Bearer {token}"

    return headers


def _is_url_allowed(full_url: str, provider: str, config: IntegrationConfig) -> bool:
    """Validate that the URL is under the provider's known API domain(s)."""
    parsed = urlparse(full_url)

    # Must be HTTPS
    if parsed.scheme != "https":
        return False

    hostname = parsed.hostname or ""

    # Check against base_url domain from integrations.yaml
    if config.api and config.api.base_url:
        base_host = urlparse(config.api.base_url).hostname
        if hostname == base_host:
            return True

    # Check against extra allowed domains
    for domain in _EXTRA_ALLOWED_DOMAINS.get(provider, []):
        if hostname == domain:
            return True

    return False


async def get_connection_status(agent_id: str, config: dict[str, Any] | None) -> str:
    """Build a connection status summary for injection into the system prompt.

    Only checks providers referenced via ``secret()`` in the current config.
    If none are referenced, the section is skipped entirely to avoid
    unnecessary noise and token usage.

    Returns a multi-line string with ✅ / ❌ indicators.
    """
    providers_to_check: set[str] = set()

    # Providers referenced via secret() in the current config
    if config:
        providers_to_check = extract_secret_keys(config)

    if not providers_to_check:
        return "No integrations checked."

    lines: list[str] = []
    async with async_session() as db:
        agent = await db.get(DbAgent, agent_id)
        workspace_id = getattr(agent, "workspace_id", None) if agent else None
        creds = await find_credentials_batch(
            db, providers_to_check, agent_id, workspace_id
        )

        for provider in sorted(providers_to_check):
            agent_cred, default_cred = creds.get(provider, (None, None))

            if agent_cred:
                lines.append(
                    f"  ✅ {provider}: connected (agent-specific, {agent_cred.auth_type})"
                )
            elif default_cred:
                lines.append(
                    f"  ✅ {provider}: connected (platform default, {default_cred.auth_type})"
                )
            else:
                lines.append(f"  ❌ {provider}: not connected")

    return "\n".join(lines) if lines else "No integrations checked."


# ── Tool registration ─────────────────────────────────────────────


def register_connection_tools(agent: Agent[BuilderDeps, str]) -> None:
    """Register ``check_connection`` and ``api_call`` on the builder agent."""

    @agent.tool
    async def check_connection(
        ctx: RunContext[BuilderDeps],
        provider: str,
    ) -> str:
        """Check if a specific integration is connected for this agent.

        Returns connection status and guidance on what to do next.
        Call this before emitting action cards or using api_call.

        Args:
            provider: Integration name (e.g. "airtable", "slack", "google_sheets")
        """
        integration = integration_registry.load_integration_config(provider)
        if not integration:
            available = [i.name for i in integration_registry.list_integrations()]
            return (
                f"Unknown integration '{provider}'. "
                f"Available integrations: {', '.join(available[:15])}"
            )

        agent_id = ctx.deps.agent_id

        async with async_session() as db:
            agent = await db.get(DbAgent, agent_id)
            workspace_id = getattr(agent, "workspace_id", None) if agent else None
            agent_cred, default_cred = await find_credential(
                db, provider, agent_id, workspace_id
            )

        display = integration.display_name

        if agent_cred:
            return (
                f"✅ {provider}: Connected (agent-specific, {agent_cred.auth_type}). "
                f"You can use api_call() to explore the user's {display} account. "
                f"Do not emit an action card — the connection already exists."
            )
        if default_cred:
            return (
                f"✅ {provider}: Connected via platform default ({default_cred.auth_type}). "
                f"You can use api_call() to explore the user's {display} account. "
                f"Do not emit an action card — a default connection is inherited."
            )

        auth_type = integration.auth.type
        return (
            f"❌ {provider}: Not connected. Auth type: {auth_type}. "
            f'Use secret("{provider}") in tool scripts — the system will '
            f"emit the correct action card so the user can connect {display}. "
            f"After they connect, try api_call() to discover their resources."
        )

    @agent.tool
    async def api_call(
        ctx: RunContext[BuilderDeps],
        provider: str,
        method: str,
        path: str,
        body: str = "",
    ) -> str:
        """Make an authenticated API call to a connected integration.

        Use this to try discovering resources in the user's account (bases,
        tables, channels, calendars, etc.). Auth headers are injected
        automatically based on the provider's configuration.

        Not all providers support resource listing. If this call fails or
        returns an error, ask the user for the required details instead.

        Args:
            provider: Integration name (e.g. "airtable", "slack")
            method: HTTP method — GET or POST only
            path: API path appended to the provider's base URL.
                  Example: "/meta/bases" for Airtable.
                  Or a full HTTPS URL for providers with multiple API domains
                  (e.g. Google Drive at https://www.googleapis.com/...).
            body: Optional JSON body for POST requests (e.g. search queries)
        """
        return await _execute_api_call(
            agent_id=ctx.deps.agent_id,
            provider=provider,
            method=method,
            path=path,
            body=body,
        )


async def _execute_api_call(
    agent_id: str,
    provider: str,
    method: str,
    path: str,
    body: str = "",
) -> str:
    """Core implementation of the api_call tool."""
    # 1. Load integration config
    config = integration_registry.load_integration_config(provider)
    if not config:
        return f"Error: Unknown integration '{provider}'."

    if not config.api or not config.api.base_url:
        return (
            f"Error: Integration '{provider}' has no API base URL configured. "
            f"You'll need to ask the user for the endpoint URL."
        )

    # 2. Build full URL
    if path.startswith("https://"):
        full_url = path
        if not _is_url_allowed(full_url, provider, config):
            return (
                f"Error: URL domain is not allowed for '{provider}'. "
                f"Only URLs under {config.api.base_url} (and known aliases) are permitted."
            )
    else:
        base = config.api.base_url.rstrip("/")
        full_url = f"{base}/{path.lstrip('/')}"

    # 3. Method guard
    method_upper = method.upper()
    if method_upper not in ("GET", "POST"):
        return "Error: Only GET and POST methods are allowed for discovery calls."

    # 4. Resolve credential
    token = await _resolve_token(provider, agent_id)
    if not token:
        return (
            f"Error: No credential found for '{provider}'. "
            f"The user needs to connect {config.display_name} first. "
            f'Use secret("{provider}") in tool scripts to prompt the action card.'
        )

    # 5. Build auth headers from integrations.yaml templates
    headers = _build_auth_headers(config, token)
    headers["Accept"] = "application/json"

    # 6. Make the request
    async with httpx.AsyncClient(timeout=_REQUEST_TIMEOUT) as client:
        try:
            if method_upper == "GET":
                resp = await client.get(full_url, headers=headers)
            else:
                try:
                    json_body = json.loads(body) if body else {}
                except json.JSONDecodeError as e:
                    return f"Error: Invalid JSON in body — {e}"
                headers["Content-Type"] = "application/json"
                resp = await client.post(full_url, headers=headers, json=json_body)
        except httpx.TimeoutException:
            return (
                f"Error: Request to {config.display_name} timed out "
                f"({_REQUEST_TIMEOUT}s). Try again or ask the user."
            )
        except Exception as exc:
            logger.warning(
                "api_call {} {} {} failed: {}",
                provider,
                method_upper,
                path,
                exc,
            )
            return f"Error: Request failed — {exc}"

    # 7. Handle error responses
    if resp.status_code == 401:
        return (
            f"Error 401: Unauthorized. The {config.display_name} token is invalid "
            f"or was revoked. The user may need to reconnect {config.display_name}."
        )
    if resp.status_code == 403:
        return (
            f"Error 403: Forbidden. The {config.display_name} token may lack "
            f"the required scopes for this endpoint. Ask the user."
        )
    if resp.status_code >= 400:
        return f"Error {resp.status_code}: {resp.text[:500]}"

    # 8. Truncate large responses
    text = resp.text
    if len(text) > _MAX_RESPONSE_SIZE:
        text = text[:_MAX_RESPONSE_SIZE] + "\n... (response truncated)"

    logger.info(
        "api_call: {} {} {} → {}",
        provider,
        method_upper,
        path,
        resp.status_code,
    )
    return text
