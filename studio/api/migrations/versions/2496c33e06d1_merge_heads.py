"""merge_heads

Revision ID: 2496c33e06d1
Revises: b74d66a2f9f1, f61f165b24ec
Create Date: 2026-03-12 15:05:27.205449
"""

from collections.abc import Sequence

# revision identifiers, used by Alembic.
revision: str = "2496c33e06d1"
down_revision: str | None = ("b74d66a2f9f1", "f61f165b24ec")
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    pass


def downgrade() -> None:
    pass
