"""nullable_credential_agent_id_default_connections

Allow credentials.agent_id to be NULL for platform-wide default connections.
A NULL agent_id credential is a "default connection" inherited by all agents.

Revision ID: c9d8e7f6a5b4
Revises: d4745d541def
Create Date: 2026-03-16 17:25:00.000000
"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "c9d8e7f6a5b4"
down_revision: str | None = "d4745d541def"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # Make agent_id nullable (drop NOT NULL constraint; FK stays)
    op.alter_column(
        "credentials",
        "agent_id",
        existing_type=sa.dialects.postgresql.UUID(as_uuid=True),
        nullable=True,
    )

    # Partial index to speed up default-connection lookups
    op.create_index(
        "ix_credentials_default_connections",
        "credentials",
        ["provider"],
        unique=False,
        postgresql_where=sa.text("agent_id IS NULL"),
    )


def downgrade() -> None:
    op.drop_index("ix_credentials_default_connections", table_name="credentials")

    # Remove any default (NULL agent_id) rows before restoring NOT NULL
    op.execute("DELETE FROM credentials WHERE agent_id IS NULL")

    op.alter_column(
        "credentials",
        "agent_id",
        existing_type=sa.dialects.postgresql.UUID(as_uuid=True),
        nullable=False,
    )
