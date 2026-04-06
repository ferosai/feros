"""add call_events table

Revision ID: e2f4a6b8c0d1
Revises: b1c2d3e4f5a6
Create Date: 2026-03-24 22:30:00.000000

"""

import sqlalchemy as sa
from alembic import op
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision = "e2f4a6b8c0d1"
down_revision = "b1c2d3e4f5a6"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "call_events",
        sa.Column("id", postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column("call_id", postgresql.UUID(as_uuid=True), nullable=False),
        sa.Column("session_id", sa.String(length=255), nullable=False),
        sa.Column("seq", sa.BigInteger(), nullable=False),
        sa.Column("event_type", sa.String(length=64), nullable=False),
        sa.Column("event_category", sa.String(length=32), nullable=False),
        sa.Column("occurred_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column(
            "payload_json", postgresql.JSONB(astext_type=sa.Text()), nullable=False
        ),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(["call_id"], ["calls.id"], ondelete="CASCADE"),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_call_events_call_id", "call_events", ["call_id"], unique=False)
    op.create_index(
        "ix_call_events_session_id", "call_events", ["session_id"], unique=False
    )
    op.create_index(
        "ix_call_events_occurred_at", "call_events", ["occurred_at"], unique=False
    )
    op.create_index(
        "ix_call_events_session_id_seq",
        "call_events",
        ["session_id", "seq"],
        unique=False,
    )


def downgrade() -> None:
    op.drop_index("ix_call_events_session_id_seq", table_name="call_events")
    op.drop_index("ix_call_events_occurred_at", table_name="call_events")
    op.drop_index("ix_call_events_session_id", table_name="call_events")
    op.drop_index("ix_call_events_call_id", table_name="call_events")
    op.drop_table("call_events")
