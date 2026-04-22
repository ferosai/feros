"""Workspace slug generation utilities.

Produces URL-safe, human-readable slugs from workspace names and guarantees
uniqueness within the ``workspaces`` table with a bounded retry loop.
"""

import uuid

from slugify import slugify
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession


def generate_base_slug(name: str) -> str:
    """Return a URL-safe lowercase slug from *name*, falling back to ``'ws'``."""
    base_slug = slugify(name, lowercase=True)
    return base_slug if base_slug else "ws"


async def generate_unique_slug(db: AsyncSession, name: str) -> str:
    """Return a slug derived from *name* that is unique in the workspaces table.

    Attempts up to 5 times, appending a random 6-character suffix on each
    collision.  Raises ``RuntimeError`` if all attempts are exhausted — this
    should only happen under extreme hash-collision probability and likely
    indicates a data-integrity issue.
    """
    from app.models.tenancy_internal import Workspace  # avoid circular import

    base_slug = generate_base_slug(name)
    current_slug = base_slug

    for _ in range(5):
        exists = await db.scalar(
            select(Workspace.id).where(Workspace.slug == current_slug)
        )
        if not exists:
            return current_slug
        short_id = str(uuid.uuid4())[:6]
        current_slug = f"{base_slug}-{short_id}"

    raise RuntimeError(
        f"Failed to generate unique slug for '{name}' after 5 attempts"
    )
