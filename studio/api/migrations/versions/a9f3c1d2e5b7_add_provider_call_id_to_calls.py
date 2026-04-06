"""add provider_call_id to calls

Revision ID: a9f3c1d2e5b7
Revises: c9d8e7f6a5b4
Create Date: 2026-03-17 01:27:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "a9f3c1d2e5b7"
down_revision: str | None = "c9d8e7f6a5b4"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.add_column(
        "calls",
        sa.Column(
            "provider_call_id",
            sa.String(length=255),
            nullable=True,
        ),
    )
    op.create_index(
        "ix_calls_provider_call_id",
        "calls",
        ["provider_call_id"],
        unique=False,
    )


def downgrade() -> None:
    op.drop_index("ix_calls_provider_call_id", table_name="calls")
    op.drop_column("calls", "provider_call_id")
