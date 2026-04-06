"""rename model_messages_json to persisted_context, add parts column, drop content

Revision ID: 6f112f6d467c
Revises: e8b1e2a0664f
Create Date: 2026-03-08 16:15:20.726834
"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision: str = "6f112f6d467c"
down_revision: str | None = "e8b1e2a0664f"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # Rename persisted context column
    op.alter_column(
        "builder_conversations",
        "model_messages_json",
        new_column_name="persisted_context",
    )

    # Add parts column
    op.add_column(
        "builder_messages",
        sa.Column("parts", postgresql.JSON(astext_type=sa.Text()), nullable=True),
    )

    # Migrate existing content into parts
    op.execute(
        """
        UPDATE builder_messages
        SET parts = json_build_array(json_build_object('kind', 'text', 'content', content))
        WHERE content IS NOT NULL AND parts IS NULL
        """
    )

    # Drop content column
    op.drop_column("builder_messages", "content")


def downgrade() -> None:
    # Re-add content column
    op.add_column(
        "builder_messages",
        sa.Column("content", sa.Text(), nullable=False, server_default=""),
    )

    # Migrate parts back to content (extract text parts)
    op.execute(
        """
        UPDATE builder_messages
        SET content = COALESCE(
            (SELECT string_agg(elem->>'content', '')
             FROM json_array_elements(parts) AS elem
             WHERE elem->>'kind' = 'text'),
            ''
        )
        WHERE parts IS NOT NULL
        """
    )

    op.drop_column("builder_messages", "parts")
    op.alter_column(
        "builder_conversations",
        "persisted_context",
        new_column_name="model_messages_json",
    )
