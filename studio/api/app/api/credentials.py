"""Credential management API — secure storage for integration secrets.

Credentials are encrypted at rest via the Rust ``integrations.EncryptionEngine``
(AES-256-GCM). Plaintext secret fields arrive in ``CredentialCreate.data``,
get encrypted, and are never returned in API responses.
"""

import uuid

from fastapi import APIRouter, Depends, HTTPException
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.lib import get_settings
from app.lib.config_utils import extract_secret_keys
from app.lib.database import get_db
from app.models.agent import Agent, AgentVersion
from app.models.credential import CURRENT_ENCRYPTION_VERSION, Credential
from app.schemas.credential import (
    CredentialCreate,
    CredentialListResponse,
    CredentialResponse,
    CredentialUpdate,
)

from app.lib.auth import TenantContext, require_tenant

router = APIRouter(
    prefix="/agents/{agent_id}/credentials",
    tags=["credentials"],
)


async def _providers_used_by_agent(db: AsyncSession, agent_id: uuid.UUID) -> set[str]:
    """Return the set of provider names referenced by ``secret()`` in this agent's tool scripts.

    Checks the active (published) version first.  If the agent has no
    active version yet (draft-only), falls back to the latest draft so
    that credentials are visible during initial agent setup.
    """
    # Determine which version to inspect
    agent_result = await db.execute(
        select(Agent.active_version).where(Agent.id == agent_id)
    )
    target_version = agent_result.scalar_one_or_none()

    if target_version is None:
        # No published version — fall back to latest draft
        draft_result = await db.execute(
            select(AgentVersion.version)
            .where(AgentVersion.agent_id == agent_id)
            .order_by(AgentVersion.version.desc())
            .limit(1)
        )
        target_version = draft_result.scalar_one_or_none()

    if target_version is None:
        return set()

    # Fetch the config
    ver_result = await db.execute(
        select(AgentVersion.config_json).where(
            AgentVersion.agent_id == agent_id,
            AgentVersion.version == target_version,
        )
    )
    config = ver_result.scalar_one_or_none()
    if not config or not isinstance(config, dict):
        return set()

    return extract_secret_keys(config)


@router.post("", response_model=CredentialResponse, status_code=201)
async def create_credential(
    agent_id: uuid.UUID,
    body: CredentialCreate,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CredentialResponse:
    """Store a new encrypted credential for an agent."""
    # Verify agent exists
    agent_query = select(Agent).where(Agent.id == agent_id)
    agent_result = await db.execute(agent_query)
    if not agent_result.scalar_one_or_none():
        raise HTTPException(status_code=404, detail="Agent not found")

    import integrations

    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
    encrypted, iv = engine.encrypt(body.data)

    credential = Credential(
        agent_id=agent_id,
        name=body.name,
        provider=body.provider,
        auth_type=body.auth_type,
        encrypted_data=encrypted,
        encryption_iv=iv,
        encryption_version=CURRENT_ENCRYPTION_VERSION,
    )
    db.add(credential)
    await db.flush()
    await db.refresh(credential)

    return CredentialResponse.model_validate(credential)


@router.get("", response_model=CredentialListResponse)
async def list_credentials(
    agent_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CredentialListResponse:
    """List credentials for an agent (secrets are never returned).

    Returns both per-agent credentials AND platform-wide default connections
    for providers the agent's tool scripts reference via ``secret("provider")``.
    Default rows are tagged ``is_default=True`` so the frontend can show an
    Override button.
    """
    # 1. Per-agent credentials
    agent_creds_query = select(Credential).where(Credential.agent_id == agent_id)
    agent_creds_query = agent_creds_query.order_by(Credential.created_at.desc())
    result = await db.execute(agent_creds_query)
    agent_creds = list(result.scalars().all())
    overridden_providers = {c.provider for c in agent_creds}

    # 2. Determine which providers this agent's config actually uses
    needed_providers = await _providers_used_by_agent(db, agent_id)

    # 3. Platform-wide defaults — only for providers the agent uses
    #    and not already covered by an agent-specific override
    default_creds: list[Credential] = []
    filter_providers = needed_providers - overridden_providers
    if filter_providers:
        defaults_query = select(Credential).where(
            Credential.agent_id.is_(None),
            Credential.provider.in_(filter_providers),
        )
        defaults_result = await db.execute(defaults_query)
        # Deduplicate providers so we only keep the highest priority one (workspace > global)
        seen_providers = set()
        default_creds = []
        for cred in defaults_result.scalars().all():
            if cred.provider not in seen_providers:
                seen_providers.add(cred.provider)
                default_creds.append(cred)

    # Build response — mark default rows
    rows: list[CredentialResponse] = [
        CredentialResponse.model_validate(c) for c in agent_creds
    ] + [
        CredentialResponse.model_validate(c).model_copy(update={"is_default": True})
        for c in default_creds
    ]

    return CredentialListResponse(credentials=rows, total=len(rows))


@router.put("/{credential_id}", response_model=CredentialResponse)
async def update_credential(
    agent_id: uuid.UUID,
    credential_id: uuid.UUID,
    body: CredentialUpdate,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CredentialResponse:
    """Update an existing credential (re-encrypts the data)."""
    query = select(Credential).where(
        Credential.id == credential_id,
        Credential.agent_id == agent_id,
    )
    result = await db.execute(query)
    credential = result.scalar_one_or_none()
    if not credential:
        raise HTTPException(status_code=404, detail="Credential not found")

    if body.name is not None:
        credential.name = body.name
    if body.data is not None:
        import integrations

        engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
        encrypted, iv = engine.encrypt(body.data)
        credential.encrypted_data = encrypted
        credential.encryption_iv = iv
        credential.encryption_version = CURRENT_ENCRYPTION_VERSION

    await db.flush()
    await db.refresh(credential)
    return CredentialResponse.model_validate(credential)


@router.delete("/{credential_id}", status_code=204)
async def delete_credential(
    agent_id: uuid.UUID,
    credential_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> None:
    """Delete a credential."""
    query = select(Credential).where(
        Credential.id == credential_id,
        Credential.agent_id == agent_id,
    )
    result = await db.execute(query)
    credential = result.scalar_one_or_none()
    if not credential:
        raise HTTPException(status_code=404, detail="Credential not found")

    await db.delete(credential)
