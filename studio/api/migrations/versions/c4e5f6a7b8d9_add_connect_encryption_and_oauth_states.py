"""add integrations encryption columns

Revision ID: c4e5f6a7b8d9
Revises: b7d9f3a1c2e4
Create Date: 2026-03-10 15:00:00.000000
"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision: str = "c4e5f6a7b8d9"
down_revision: str | None = "b7d9f3a1c2e4"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # ── Encryption metadata ──────────────────────────────────────
    op.add_column(
        "credentials",
        sa.Column("encryption_iv", sa.Text(), nullable=True),
    )
    op.add_column(
        "credentials",
        sa.Column(
            "encryption_version",
            sa.Integer(),
            nullable=False,
            server_default="1",  # existing rows are Fernet (v1)
        ),
    )

    # ── Refresh tracking ─────────────────────────────────────────
    # Both success and failure timestamps tracked separately.
    # refresh_attempts incremented once per day (not per attempt)
    # to prevent provider outages from instantly exhausting retries.
    op.add_column(
        "credentials",
        sa.Column("last_refresh_success", sa.DateTime(timezone=True), nullable=True),
    )
    op.add_column(
        "credentials",
        sa.Column("last_refresh_failure", sa.DateTime(timezone=True), nullable=True),
    )
    op.add_column(
        "credentials",
        sa.Column("last_refresh_error", sa.Text(), nullable=True),
    )
    op.add_column(
        "credentials",
        sa.Column(
            "refresh_attempts",
            sa.Integer(),
            nullable=False,
            server_default="0",
        ),
    )
    op.add_column(
        "credentials",
        sa.Column(
            "refresh_exhausted",
            sa.Boolean(),
            nullable=False,
            server_default="false",
        ),
    )

    # ── Stale connection tracking ─────────────────────────────────
    # Tracks when credentials were last resolved for a session.
    # Refresh cron skips connections not fetched recently.
    op.add_column(
        "credentials",
        sa.Column("last_fetched_at", sa.DateTime(timezone=True), nullable=True),
    )


def downgrade() -> None:
    op.drop_column("credentials", "last_fetched_at")
    op.drop_column("credentials", "refresh_exhausted")
    op.drop_column("credentials", "refresh_attempts")
    op.drop_column("credentials", "last_refresh_error")
    op.drop_column("credentials", "last_refresh_failure")
    op.drop_column("credentials", "last_refresh_success")
    op.drop_column("credentials", "encryption_version")
    op.drop_column("credentials", "encryption_iv")
