"""OAuth 2.0 flow — authorize + callback for integration connections.

Uses authlib for PKCE and token exchange.
OAuth state is stored in an ephemeral KV store (in-memory or Redis).

Flow:
  1. Frontend calls GET /oauth/{integration_name}/authorize?agent_id=xxx
  2. Backend generates state + PKCE, stores in KV store with TTL
  3. Returns authorize_url with PKCE challenge
  4. User authorizes in popup → integration redirects to GET /oauth/callback
  5. Backend exchanges code for tokens (via authlib), encrypts, stores in credentials
  6. Returns HTML that postMessage's back to the opener and closes
"""

from __future__ import annotations

import secrets
import uuid
from base64 import b64encode
from datetime import UTC, datetime, timedelta
from typing import Any

import httpx
from authlib.common.security import generate_token
from authlib.oauth2.rfc7636 import create_s256_code_challenge
from fastapi import APIRouter, Depends, HTTPException, Query
from fastapi.responses import HTMLResponse
from loguru import logger
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.lib import get_settings
from app.lib.database import get_db
from app.lib.integration_registry import integration_registry
from app.lib.kv_store import kv_store
from app.lib.oauth_apps import get_oauth_client_credentials
from app.models.agent import Agent
from app.models.credential import CURRENT_ENCRYPTION_VERSION, Credential
from app.lib.auth import TenantContext, require_tenant

router = APIRouter(prefix="/oauth", tags=["oauth"])

_STATE_TTL_SECONDS = 600  # 10 minutes


async def _get_oauth_client_credentials(
    integration: str, db: AsyncSession
) -> tuple[str, str]:
    """Look up platform-level client_id and client_secret for an integration."""
    return await get_oauth_client_credentials(integration, db)


def _oauth_callback_url() -> str:
    """Return the normalized OAuth callback URL used by the backend."""
    settings = get_settings()
    return f"{settings.oauth_callback_base_url.rstrip('/')}/api/oauth/callback"


# ── Routes ───────────────────────────────────────────────────────


@router.get("/callback-url")
async def oauth_callback_url() -> dict[str, str]:
    """Expose the effective OAuth callback URL for frontend display."""
    return {"callback_url": _oauth_callback_url()}


@router.get("/{integration_name}/authorize")
async def oauth_authorize(
    integration_name: str,
    agent_id: uuid.UUID | None = Query(
        default=None,
        description="Agent to attach the credential to. Omit for a platform-wide default.",
    ),
    is_default: bool = Query(
        default=False,
        description="If true, store as a platform-wide default (agent_id must be omitted).",
    ),
    origin: str = Query(
        default="", description="Opener window origin for postMessage targeting"
    ),
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> dict[str, Any]:
    """Start OAuth flow: generate authorize URL with PKCE.

    Returns {"authorize_url": "https://accounts.google.com/o/oauth2/..."}

    When ``agent_id`` is omitted **or** ``is_default=true``, the resulting
    credential is stored as a platform-wide default (``agent_id IS NULL``).
    """
    # Validate agent exists when agent_id is provided
    if agent_id is not None and not is_default:
        result = await db.execute(select(Agent).where(Agent.id == agent_id))
        if not result.scalar_one_or_none():
            raise HTTPException(status_code=404, detail="Agent not found")

    # Determine storage mode
    store_agent_id = (
        "__platform__" if (is_default or agent_id is None) else str(agent_id)
    )

    # Load integration config from integrations.yaml
    config = integration_registry.load_integration_config(integration_name)
    if not config:
        raise HTTPException(
            status_code=404, detail=f"Integration '{integration_name}' not found"
        )

    auth = config.auth
    if auth.type != "oauth2":
        raise HTTPException(
            status_code=400,
            detail=f"Integration '{integration_name}' does not support OAuth",
        )

    client_id, _ = await _get_oauth_client_credentials(integration_name, db)
    if not client_id:
        raise HTTPException(
            status_code=500,
            detail=f"OAuth not configured for integration '{integration_name}'. "
            f"Register it via POST /api/oauth-apps.",
        )

    # PKCE via authlib
    code_verifier = generate_token(48)
    code_challenge = create_s256_code_challenge(code_verifier)  # type: ignore[no-untyped-call]

    # State token — random, opaque
    state_token = secrets.token_urlsafe(32)

    # Store state in KV store with TTL
    await kv_store.set(
        f"oauth:{state_token}",
        {
            "agent_id": store_agent_id,
            "integration": integration_name,
            "code_verifier": code_verifier,
            "origin": origin or "",
            "workspace_id": str(ctx.workspace.id) if ctx else None,
        },
        ttl=_STATE_TTL_SECONDS,
    )

    # Build authorize URL from integrations.yaml config
    callback_url = _oauth_callback_url()

    # Start with integration-defined authorization_params
    params: dict[str, str] = dict(auth.authorization_params)

    # Merge required OAuth params
    scopes = auth.scope_separator.join(auth.default_scopes)

    params.update(
        {
            "client_id": client_id,
            "redirect_uri": callback_url,
            "scope": scopes,
            "state": state_token,
        }
    )

    # Add PKCE if enabled for this integration
    if auth.pkce:
        params["code_challenge"] = code_challenge
        params["code_challenge_method"] = "S256"

    authorize_url = auth.authorization_url
    if not authorize_url:
        raise HTTPException(
            status_code=500,
            detail=f"Integration '{integration_name}' is missing authorization_url",
        )
    full_url = str(httpx.URL(authorize_url, params=params))

    return {"authorize_url": full_url}


@router.get("/callback", response_class=HTMLResponse)
async def oauth_callback(
    code: str | None = Query(default=None),
    state: str | None = Query(default=None),
    error: str | None = Query(default=None),
    error_description: str | None = Query(default=None),
    db: AsyncSession = Depends(get_db),
) -> HTMLResponse:
    """OAuth callback — exchange code for tokens, store credential, close popup."""
    if error:
        detail = error_description or error
        return HTMLResponse(
            "<html><body><h2>Authorization cancelled</h2>"
            f"<p>{detail}</p>"
            "<script>setTimeout(()=>window.close(),4000)</script></body></html>",
            status_code=400,
        )

    if not code or not state:
        return HTMLResponse(
            "<html><body><h2>Authorization failed</h2>"
            "<p>Missing OAuth callback parameters (code/state).</p>"
            "<script>setTimeout(()=>window.close(),4000)</script></body></html>",
            status_code=400,
        )

    settings = get_settings()

    # Pop state from KV store (atomic get-and-delete)
    state_data = await kv_store.pop(f"oauth:{state}")

    if not state_data:
        return HTMLResponse(
            "<html><body><h2>Authorization failed</h2>"
            "<p>State token expired or invalid. Please try again.</p>"
            "<script>setTimeout(()=>window.close(),3000)</script></body></html>",
            status_code=400,
        )

    raw_agent_id = state_data["agent_id"]
    is_platform_default = raw_agent_id == "__platform__"
    agent_id = None if is_platform_default else uuid.UUID(raw_agent_id)
    # Support both old ("provider") and new ("integration") KV keys
    integration_name = state_data.get("integration") or state_data.get("provider", "")
    code_verifier = state_data["code_verifier"]
    opener_origin = state_data.get("origin", "")
    # Load integration auth config
    config = integration_registry.load_integration_config(integration_name)
    if not config:
        return HTMLResponse(
            "<html><body><h2>Integration not found</h2></body></html>", status_code=400
        )

    auth = config.auth
    client_id, client_secret = await _get_oauth_client_credentials(integration_name, db)
    callback_url = _oauth_callback_url()

    # Build token exchange request from integrations.yaml token_params
    token_data: dict[str, str] = dict(auth.token_params)
    token_data.update(
        {
            "code": code,
            "redirect_uri": callback_url,
        }
    )
    headers = {}

    if auth.client_auth_method == "header":
        # Send client credentials via HTTP Basic Authorization header
        # (RFC 6749 §2.3.1). Used by providers like Airtable.
        auth_str = f"{client_id}:{client_secret}"
        b64_auth = b64encode(auth_str.encode()).decode()
        headers["Authorization"] = f"Basic {b64_auth}"
    else:
        # Default "body" method: include client_id/secret in the form body
        token_data["client_id"] = client_id
        token_data["client_secret"] = client_secret

    if auth.pkce:
        token_data["code_verifier"] = code_verifier

    # Exchange code for tokens
    try:
        async with httpx.AsyncClient(timeout=15.0) as client:
            resp = await client.post(
                auth.token_url or "", data=token_data, headers=headers
            )

        if not resp.is_success:
            logger.error(
                "OAuth token exchange failed: {} {}",
                resp.status_code,
                resp.text[:300],
            )
            return HTMLResponse(
                "<html><body><h2>Authorization failed</h2>"
                "<p>Could not exchange authorization code. Please try again.</p>"
                "<script>setTimeout(()=>window.close(),5000)</script></body></html>",
                status_code=502,
            )

        tokens = resp.json()
    except Exception as e:
        logger.exception("OAuth token exchange error")
        return HTMLResponse(
            f"<html><body><h2>Error</h2><p>{e}</p></body></html>",
            status_code=502,
        )

    access_token = tokens.get("access_token", "")
    refresh_token = tokens.get("refresh_token", "")
    expires_in = tokens.get("expires_in", 3600)

    if not access_token:
        return HTMLResponse(
            "<html><body><h2>No access token received</h2></body></html>",
            status_code=502,
        )

    # Encrypt and store credential (v2 AES-256-GCM)
    cred_data = {"access_token": access_token}
    if refresh_token:
        cred_data["refresh_token"] = refresh_token
    import integrations

    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
    encrypted, iv = engine.encrypt(cred_data)

    token_expires_at = datetime.now(UTC) + timedelta(seconds=int(expires_in))
    display_name = config.display_name

    # Upsert: update existing credential for this agent+integration, or create new
    existing_query = select(Credential).where(
        (
            Credential.agent_id == agent_id
            if not is_platform_default
            else Credential.agent_id.is_(None)
        ),
        Credential.provider == integration_name,
    )
    existing_result = await db.execute(existing_query)
    existing = existing_result.scalar_one_or_none()

    if existing:
        existing.encrypted_data = encrypted
        existing.encryption_iv = iv
        existing.encryption_version = CURRENT_ENCRYPTION_VERSION
        existing.auth_type = "oauth2"
        existing.token_expires_at = token_expires_at
        existing.last_refresh_success = datetime.now(UTC)
        existing.last_refresh_failure = None
        existing.refresh_attempts = 0
        existing.refresh_exhausted = False
    else:
        credential = Credential(
            agent_id=agent_id,  # None for platform defaults
            name=f"{display_name} {'Default ' if is_platform_default else ''}Connection",
            provider=integration_name,
            auth_type="oauth2",
            encrypted_data=encrypted,
            encryption_iv=iv,
            encryption_version=CURRENT_ENCRYPTION_VERSION,
            token_expires_at=token_expires_at,
            last_refresh_success=datetime.now(UTC),
        )
        db.add(credential)

    await db.commit()

    logger.info(
        "OAuth credential stored for agent={} integration={} expires={}",
        agent_id,
        integration_name,
        token_expires_at,
    )

    # Return HTML that notifies the opener and closes the popup
    target_origin = opener_origin or settings.oauth_callback_base_url
    return HTMLResponse(
        """<!DOCTYPE html>
<html>
<head><title>Authorization Complete</title></head>
<body>
<h2>Authorization successful!</h2>
<p>This window will close automatically.</p>
<script>
  if (window.opener) {
    window.opener.postMessage({type: 'oauth_complete', integration: '%s'}, '%s');
  }
  setTimeout(() => window.close(), 1000);
</script>
</body>
</html>""".replace("%s", integration_name, 1).replace("%s", target_origin, 1)
    )
