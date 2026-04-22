"""Integrations API — serves integration metadata, credential schemas, and default connections.

Default connections are platform-wide credentials (agent_id IS NULL in the
credentials table). Agents inherit them automatically; per-agent credentials
override the default for that specific agent.
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

import integrations
from app.lib import get_settings
from app.lib.database import get_db
from app.lib.integration_registry import (
    CredentialSchema,
    IntegrationSummary,
    integration_registry,
)
from app.lib.oauth_apps import get_oauth_app
from app.models.credential import CURRENT_ENCRYPTION_VERSION, Credential
from app.schemas.credential import (
    DefaultConnectionResponse,
    DefaultConnectionUpsert,
)

from app.lib.auth import TenantContext, require_tenant

router = APIRouter(prefix="/integrations", tags=["integrations"])


@router.get("")
async def list_integrations() -> list[IntegrationSummary]:
    """List all available integrations (Level 1 metadata)."""
    return integration_registry.list_integrations()


@router.get("/{integration_name}/credential-schema")
async def get_credential_schema(
    integration_name: str,
    db: AsyncSession = Depends(get_db),
) -> CredentialSchema:
    """Get the credential form schema for an integration (Level 3).

    Used by the frontend to render the secure credential collection modal.
    """
    schema = integration_registry.load_credential_schema(integration_name)
    if not schema:
        raise HTTPException(
            status_code=404,
            detail=f"Credential schema not found for integration '{integration_name}'",
        )

    # Check if platform has OAuth configured for this integration
    if schema.auth_type == "oauth2":
        app = await get_oauth_app(integration_name, db)
        schema.oauth_available = app is not None

    return schema


# ── Default Connections ───────────────────────────────────────────


@router.get("/default-connections")
async def list_default_connections(
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> list[DefaultConnectionResponse]:
    """List all platform-wide default connections."""
    query = select(Credential).where(Credential.agent_id.is_(None))
    query = query.order_by(Credential.provider)
    result = await db.execute(query)
    rows = list(result.scalars().all())
    return [DefaultConnectionResponse.model_validate(r) for r in rows]


@router.get("/{integration_name}/default-connection")
async def get_default_connection(
    integration_name: str,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> DefaultConnectionResponse:
    """Get the platform-wide default connection for an integration, if configured."""
    query = select(Credential).where(
        Credential.provider == integration_name,
        Credential.agent_id.is_(None),
    )
    result = await db.execute(query.limit(1))
    row = result.scalars().first()
    if not row:
        raise HTTPException(status_code=404, detail="No default connection configured")
    return DefaultConnectionResponse.model_validate(row)


@router.put(
    "/{integration_name}/default-connection", response_model=DefaultConnectionResponse
)
async def upsert_default_connection(
    integration_name: str,
    body: DefaultConnectionUpsert,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> DefaultConnectionResponse:
    """Create or replace the platform-wide default connection for an integration.

    The credential data is AES-256-GCM encrypted before storage. Secrets
    are never returned via the API.
    """
    # Verify integration exists
    config = integration_registry.load_integration_config(integration_name)
    if not config:
        raise HTTPException(
            status_code=404, detail=f"Integration '{integration_name}' not found"
        )

    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
    encrypted, iv = engine.encrypt(body.data)

    query = select(Credential).where(
        Credential.provider == integration_name,
        Credential.agent_id.is_(None),
    )
    result = await db.execute(query)
    existing = result.scalar_one_or_none()

    if existing:
        existing.auth_type = body.auth_type
        existing.encrypted_data = encrypted
        existing.encryption_iv = iv
        existing.encryption_version = CURRENT_ENCRYPTION_VERSION
        await db.flush()
        await db.refresh(existing)
        return DefaultConnectionResponse.model_validate(existing)

    credential = Credential(
        agent_id=None,
        name=f"{integration_name} (default)",
        provider=integration_name,
        auth_type=body.auth_type,
        encrypted_data=encrypted,
        encryption_iv=iv,
        encryption_version=CURRENT_ENCRYPTION_VERSION,
    )
    db.add(credential)
    await db.flush()
    await db.refresh(credential)
    return DefaultConnectionResponse.model_validate(credential)


@router.delete("/{integration_name}/default-connection", status_code=204)
async def delete_default_connection(
    integration_name: str,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> None:
    """Remove the platform-wide default connection for an integration."""
    query = select(Credential).where(
        Credential.provider == integration_name,
        Credential.agent_id.is_(None),
    )
    result = await db.execute(query)
    row = result.scalar_one_or_none()
    if not row:
        raise HTTPException(status_code=404, detail="No default connection configured")
    await db.delete(row)
