"""add evaluation config/run persistence tables

Revision ID: b74d66a2f9f1
Revises: 23bcafb9f857
Create Date: 2026-03-09 18:55:00.000000
"""

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op
from sqlalchemy.dialects import postgresql

# revision identifiers, used by Alembic.
revision: str = "b74d66a2f9f1"
down_revision: str | None = "23bcafb9f857"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        "evaluation_configs",
        sa.Column("id", sa.UUID(), nullable=False),
        sa.Column("agent_id", sa.UUID(), nullable=False),
        sa.Column("name", sa.String(length=255), nullable=False),
        sa.Column("status", sa.String(length=20), nullable=False),
        sa.Column("latest_version", sa.Integer(), nullable=False, server_default="1"),
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
        sa.ForeignKeyConstraint(["agent_id"], ["agents.id"], ondelete="CASCADE"),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index(
        "ix_evaluation_configs_agent_id",
        "evaluation_configs",
        ["agent_id"],
        unique=False,
    )
    op.create_index(
        "ix_evaluation_configs_status", "evaluation_configs", ["status"], unique=False
    )

    op.create_table(
        "evaluation_config_versions",
        sa.Column("id", sa.UUID(), nullable=False),
        sa.Column("config_id", sa.UUID(), nullable=False),
        sa.Column("version", sa.Integer(), nullable=False),
        sa.Column(
            "config_json", postgresql.JSONB(astext_type=sa.Text()), nullable=False
        ),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(
            ["config_id"], ["evaluation_configs.id"], ondelete="CASCADE"
        ),
        sa.PrimaryKeyConstraint("id"),
        sa.UniqueConstraint("config_id", "version"),
    )
    op.create_index(
        "ix_eval_config_versions_config_id",
        "evaluation_config_versions",
        ["config_id"],
        unique=False,
    )

    op.create_table(
        "evaluation_runs",
        sa.Column("id", sa.UUID(), nullable=False),
        sa.Column("agent_id", sa.UUID(), nullable=False),
        sa.Column("config_id", sa.UUID(), nullable=False),
        sa.Column("config_version_id", sa.UUID(), nullable=False),
        sa.Column("target_agent_version", sa.Integer(), nullable=True),
        sa.Column("seed", sa.Integer(), nullable=False, server_default="42"),
        sa.Column("status", sa.String(length=20), nullable=False),
        sa.Column("aggregate_score", sa.Float(), nullable=True),
        sa.Column("summary", sa.Text(), nullable=True),
        sa.Column("started_at", sa.DateTime(timezone=True), nullable=True),
        sa.Column("ended_at", sa.DateTime(timezone=True), nullable=True),
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
        sa.ForeignKeyConstraint(["agent_id"], ["agents.id"], ondelete="CASCADE"),
        sa.ForeignKeyConstraint(
            ["config_id"], ["evaluation_configs.id"], ondelete="CASCADE"
        ),
        sa.ForeignKeyConstraint(
            ["config_version_id"],
            ["evaluation_config_versions.id"],
            ondelete="RESTRICT",
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_evaluation_runs_agent_id", "evaluation_runs", ["agent_id"])
    op.create_index("ix_evaluation_runs_status", "evaluation_runs", ["status"])
    op.create_index("ix_evaluation_runs_started_at", "evaluation_runs", ["started_at"])
    op.create_index(
        "ix_evaluation_runs_config_version_id",
        "evaluation_runs",
        ["config_version_id"],
    )

    op.create_table(
        "evaluation_run_events",
        sa.Column("id", sa.UUID(), nullable=False),
        sa.Column("run_id", sa.UUID(), nullable=False),
        sa.Column("seq_no", sa.Integer(), nullable=False),
        sa.Column("event_type", sa.String(length=64), nullable=False),
        sa.Column("event_timestamp", sa.DateTime(timezone=True), nullable=False),
        sa.Column(
            "payload_json", postgresql.JSONB(astext_type=sa.Text()), nullable=False
        ),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(["run_id"], ["evaluation_runs.id"], ondelete="CASCADE"),
        sa.PrimaryKeyConstraint("id"),
        sa.UniqueConstraint("run_id", "seq_no"),
    )
    op.create_index("ix_eval_run_events_run_id", "evaluation_run_events", ["run_id"])
    op.create_index(
        "ix_eval_run_events_timestamp", "evaluation_run_events", ["event_timestamp"]
    )

    op.create_table(
        "evaluation_judgments",
        sa.Column("id", sa.UUID(), nullable=False),
        sa.Column("run_id", sa.UUID(), nullable=False),
        sa.Column(
            "hard_checks",
            postgresql.JSONB(astext_type=sa.Text()),
            nullable=False,
            server_default=sa.text("'{}'::jsonb"),
        ),
        sa.Column(
            "rubric_scores",
            postgresql.JSONB(astext_type=sa.Text()),
            nullable=False,
            server_default=sa.text("'{}'::jsonb"),
        ),
        sa.Column("summary", sa.Text(), nullable=True),
        sa.Column(
            "failure_highlights",
            postgresql.JSONB(astext_type=sa.Text()),
            nullable=False,
            server_default=sa.text("'[]'::jsonb"),
        ),
        sa.Column(
            "recommendations",
            postgresql.JSONB(astext_type=sa.Text()),
            nullable=False,
            server_default=sa.text("'[]'::jsonb"),
        ),
        sa.Column(
            "created_at",
            sa.DateTime(timezone=True),
            server_default=sa.text("now()"),
            nullable=False,
        ),
        sa.ForeignKeyConstraint(["run_id"], ["evaluation_runs.id"], ondelete="CASCADE"),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index(
        "ix_evaluation_judgments_run_id", "evaluation_judgments", ["run_id"]
    )


def downgrade() -> None:
    op.drop_index("ix_evaluation_judgments_run_id", table_name="evaluation_judgments")
    op.drop_table("evaluation_judgments")

    op.drop_index("ix_eval_run_events_timestamp", table_name="evaluation_run_events")
    op.drop_index("ix_eval_run_events_run_id", table_name="evaluation_run_events")
    op.drop_table("evaluation_run_events")

    op.drop_index(
        "ix_evaluation_runs_config_version_id",
        table_name="evaluation_runs",
    )
    op.drop_index("ix_evaluation_runs_started_at", table_name="evaluation_runs")
    op.drop_index("ix_evaluation_runs_status", table_name="evaluation_runs")
    op.drop_index("ix_evaluation_runs_agent_id", table_name="evaluation_runs")
    op.drop_table("evaluation_runs")

    op.drop_index(
        "ix_eval_config_versions_config_id",
        table_name="evaluation_config_versions",
    )
    op.drop_table("evaluation_config_versions")

    op.drop_index("ix_evaluation_configs_status", table_name="evaluation_configs")
    op.drop_index("ix_evaluation_configs_agent_id", table_name="evaluation_configs")
    op.drop_table("evaluation_configs")
