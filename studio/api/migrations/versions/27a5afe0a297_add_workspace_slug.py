"""add_workspace_multi_tenancy

Revision ID: 27a5afe0a297
Revises: e2f4a6b8c0d1
Create Date: 2026-04-20 15:43:00.000000
"""

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision: str = "27a5afe0a297"
down_revision: str | None = "e2f4a6b8c0d1"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    pass
def downgrade() -> None:
    pass
