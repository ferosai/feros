"""API key authentication dependency.

Provides a simple API key check via the ``X-API-Key`` header.
Exempt paths (health check, telephony webhooks) are handled via
FastAPI dependency overrides or by mounting them without the dep.

Security note: API key comparison uses ``hmac.compare_digest`` to
prevent timing-oracle attacks. A naive ``==`` comparison short-circuits
on the first mismatched byte, allowing an attacker to statistically
infer the key one character at a time by measuring response latencies.
``compare_digest`` always takes the same time regardless of where the
strings differ, eliminating that signal.
"""

from __future__ import annotations

import hmac
from typing import Any

from fastapi import Depends, HTTPException, Request, Security
from fastapi.security import APIKeyHeader

from app.lib import get_settings

_api_key_header = APIKeyHeader(name="X-API-Key", auto_error=False)



async def require_api_key(
    request: Request,
    key: str | None = Security(_api_key_header),
) -> str:
    """Validate the API key from the request header.

    Uses a constant-time comparison (``hmac.compare_digest``) to
    prevent timing-oracle attacks.

    Raises:
        HTTPException 401 if the key is missing or invalid.
    """
    settings = get_settings()

    # Skip auth in development when no key is configured
    # Check for Clerk JWT first, even in development
    if settings.is_development and settings.auth.api_key == "":
        return "dev-no-key"

    if not key:
        raise HTTPException(status_code=401, detail="Missing API key")

    # Constant-time comparison — prevents timing-oracle attacks
    if not hmac.compare_digest(key, settings.auth.api_key):
        raise HTTPException(status_code=401, detail="Invalid API key")

    return key



if "TenantContext" not in globals():
    class TenantContext:  # type: ignore[no-redef, unused-ignore]
        user: Any
        organization: Any
        workspace: Any
        role: str

if "require_tenant" not in globals():
    async def require_tenant(*_args: Any, **_kwargs: Any) -> Any:  # type: ignore[misc, unused-ignore]
        return None


# Re-usable dependency
RequireAuth = Depends(require_api_key)
