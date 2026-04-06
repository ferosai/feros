"""add phone_numbers table

Revision ID: d4e5f6a7b8c9
Revises: 2496c33e06d1
Create Date: 2026-03-14 17:30:00.000000
"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision: str = "d4e5f6a7b8c9"
down_revision: str | None = "2496c33e06d1"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        "phone_numbers",
        sa.Column("id", postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column("provider", sa.String(length=20), nullable=False),
        sa.Column("phone_number", sa.String(length=30), nullable=False),
        sa.Column("provider_sid", sa.String(length=128), nullable=True),
        sa.Column("friendly_name", sa.String(length=255), nullable=True),
        sa.Column("agent_id", postgresql.UUID(as_uuid=True), nullable=True),
        sa.Column("py_backend_url", sa.Text(), nullable=True),
        sa.Column("rust_voice_url", sa.Text(), nullable=True),
        sa.Column("telnyx_connection_id", sa.String(length=128), nullable=True),
        sa.Column("is_active", sa.Boolean(), nullable=False, server_default="true"),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.Column(
            "updated_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(["agent_id"], ["agents.id"], ondelete="SET NULL"),
        sa.PrimaryKeyConstraint("id"),
        sa.UniqueConstraint("phone_number", name="uq_phone_numbers_e164"),
    )
    op.create_index("ix_phone_numbers_agent_id", "phone_numbers", ["agent_id"])
    op.create_index("ix_phone_numbers_provider", "phone_numbers", ["provider"])
    op.create_index(
        "ix_phone_numbers_phone_number", "phone_numbers", ["phone_number"], unique=True
    )


def downgrade() -> None:
    op.drop_index("ix_phone_numbers_phone_number", table_name="phone_numbers")
    op.drop_index("ix_phone_numbers_provider", table_name="phone_numbers")
    op.drop_index("ix_phone_numbers_agent_id", table_name="phone_numbers")
    op.drop_table("phone_numbers")
