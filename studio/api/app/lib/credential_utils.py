"""Shared credential lookup utilities.

Centralises the agent-specific → platform-default credential resolution
pattern used by ``connections.py``, ``credentials.py``, and others.
"""

import uuid

from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.models.credential import Credential


async def find_credential(
    db: AsyncSession,
    provider: str,
    agent_id: str | uuid.UUID | None,
    workspace_id: str | uuid.UUID | None = None,
) -> tuple[Credential | None, Credential | None]:
    """Look up credentials for *provider*, returning (agent_cred, default_cred).

    Resolution order:
      1. **Agent-specific** credential (``agent_id`` matches)
      2. **Workspace-specific default** credential (``agent_id IS NULL`` and ``workspace_id`` matches)
      3. **Platform default** credential (``agent_id IS NULL`` and ``workspace_id IS NULL``)

    Either or both may be ``None``.
    """
    agent_cred: Credential | None = None
    default_cred: Credential | None = None

    # 1. Agent-specific lookup
    if agent_id:
        try:
            agent_uuid = (
                agent_id
                if isinstance(agent_id, uuid.UUID)
                else uuid.UUID(str(agent_id))
            )
        except ValueError:
            agent_uuid = None

        if agent_uuid is not None:
            r = await db.execute(
                select(Credential)
                .where(
                    Credential.provider == provider,
                    Credential.agent_id == agent_uuid,
                )
                .limit(1)
            )
            agent_cred = r.scalar_one_or_none()

    # 2. Platform default lookup
    query = select(Credential).where(
        Credential.provider == provider,
        Credential.agent_id.is_(None),
    )

    r = await db.execute(query.limit(1))
    default_cred = r.scalar_one_or_none()

    return agent_cred, default_cred


async def find_credentials_batch(
    db: AsyncSession,
    providers: set[str],
    agent_id: str | uuid.UUID | None,
    workspace_id: str | uuid.UUID | None = None,
) -> dict[str, tuple[Credential | None, Credential | None]]:
    """Batch-lookup credentials for multiple *providers* in two queries.

    Returns a dict mapping each provider to ``(agent_cred, default_cred)``.
    This avoids the N+1 query problem of calling :func:`find_credential`
    in a loop.
    """
    if not providers:
        return {}

    agent_uuid: uuid.UUID | None = None
    if agent_id:
        try:
            agent_uuid = (
                agent_id
                if isinstance(agent_id, uuid.UUID)
                else uuid.UUID(str(agent_id))
            )
        except ValueError:
            pass

    # Pre-fill result map
    result: dict[str, tuple[Credential | None, Credential | None]] = {
        p: (None, None) for p in providers
    }

    # 1. Fetch all agent-specific credentials in one query
    agent_creds: dict[str, Credential] = {}
    if agent_uuid is not None:
        r = await db.execute(
            select(Credential).where(
                Credential.provider.in_(providers),
                Credential.agent_id == agent_uuid,
            )
        )
        for cred in r.scalars().all():
            agent_creds[cred.provider] = cred

    # 2. Fetch all platform default credentials in one query
    default_creds: dict[str, Credential] = {}
    query = select(Credential).where(
        Credential.provider.in_(providers),
        Credential.agent_id.is_(None),
    )

    r = await db.execute(query)
    for cred in r.scalars().all():
        if cred.provider not in default_creds:
            default_creds[cred.provider] = cred

    # 3. Merge into result
    for p in providers:
        result[p] = (agent_creds.get(p), default_creds.get(p))

    return result
