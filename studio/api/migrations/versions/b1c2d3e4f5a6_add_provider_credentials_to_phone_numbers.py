"""add provider_credentials_encrypted to phone_numbers

Revision ID: b1c2d3e4f5a6
Revises: a9f3c1d2e5b7
Create Date: 2026-03-20 00:00:00.000000

"""

import sqlalchemy as sa
from alembic import op

# revision identifiers, used by Alembic.
revision = "b1c2d3e4f5a6"
down_revision = "a9f3c1d2e5b7"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column(
        "phone_numbers",
        sa.Column("provider_credentials_encrypted", sa.Text(), nullable=True),
    )


def downgrade() -> None:
    op.drop_column("phone_numbers", "provider_credentials_encrypted")
