from __future__ import annotations

import uuid
from datetime import datetime
from enum import StrEnum
from typing import TYPE_CHECKING, Any

from sqlalchemy import (
    DateTime,
    ForeignKey,
    Integer,
    String,
    Text,
    UniqueConstraint,
    Uuid,
    func,
)
from sqlalchemy.dialects.postgresql import JSONB
from sqlalchemy.orm import Mapped, mapped_column, relationship

from app.lib.database import Base

if TYPE_CHECKING:
    from app.models.call import Call
    from app.models.conversation import BuilderConversation, BuilderMessage
    from app.models.credential import Credential
    from app.models.evaluation import EvaluationConfig, EvaluationRun
    from app.models.phone_number import PhoneNumber


class AgentStatus(StrEnum):
    """Lifecycle states for an agent.

    Stored as a plain VARCHAR for backwards compatibility; StrEnum
    ensures all code paths use well-known values instead of bare strings.
    """

    DRAFT = "draft"
    ACTIVE = "active"
    PAUSED = "paused"


class Agent(Base):
    """A voice agent that can handle calls."""

    __tablename__ = "agents"

    id: Mapped[uuid.UUID] = mapped_column(Uuid, primary_key=True, default=uuid.uuid4)
    name: Mapped[str] = mapped_column(String(255), nullable=False)
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    status: Mapped[AgentStatus] = mapped_column(
        String(20), nullable=False, default=AgentStatus.DRAFT
    )
    active_version: Mapped[int | None] = mapped_column(Integer, nullable=True)
    phone_number: Mapped[str | None] = mapped_column(String(20), nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), onupdate=func.now()
    )
    # Relationships
    versions: Mapped[list[AgentVersion]] = relationship(
        back_populates="agent",
        cascade="all, delete-orphan",
        order_by="AgentVersion.version",
    )
    builder_conversations: Mapped[list[BuilderConversation]] = relationship(
        "BuilderConversation",
        back_populates="agent",
        cascade="all, delete-orphan",
    )
    calls: Mapped[list[Call]] = relationship(
        "Call",
        back_populates="agent",
        cascade="all, delete-orphan",
    )
    credentials: Mapped[list[Credential]] = relationship(
        "Credential",
        primaryjoin="and_(Agent.id == foreign(Credential.agent_id))",
        cascade="all, delete-orphan",
    )
    artifacts: Mapped[list[AgentArtifact]] = relationship(
        back_populates="agent",
        cascade="all, delete-orphan",
    )
    evaluation_configs: Mapped[list[EvaluationConfig]] = relationship(
        "EvaluationConfig",
        back_populates="agent",
        cascade="all, delete-orphan",
    )
    evaluation_runs: Mapped[list[EvaluationRun]] = relationship(
        "EvaluationRun",
        back_populates="agent",
        cascade="all, delete-orphan",
    )
    phone_numbers: Mapped[list[PhoneNumber]] = relationship(
        "PhoneNumber",
        back_populates="agent",
        passive_deletes=True,  # FK is SET NULL on delete, no Python cascade needed
    )

    def __repr__(self) -> str:
        return f"<Agent {self.name} ({self.status})>"


class AgentVersion(Base):
    """Immutable snapshot of an agent's configuration at a point in time."""

    __tablename__ = "agent_versions"
    __table_args__ = (UniqueConstraint("agent_id", "version"),)

    id: Mapped[uuid.UUID] = mapped_column(Uuid, primary_key=True, default=uuid.uuid4)
    agent_id: Mapped[uuid.UUID] = mapped_column(
        Uuid,
        ForeignKey("agents.id", ondelete="CASCADE"),
        nullable=False,
    )
    version: Mapped[int] = mapped_column(Integer, nullable=False)
    config_json: Mapped[dict[str, Any]] = mapped_column(JSONB, nullable=False)
    change_summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )

    # Relationships
    agent: Mapped[Agent] = relationship(back_populates="versions")
    builder_messages: Mapped[list[BuilderMessage]] = relationship(
        "BuilderMessage",
        back_populates="agent_version",
    )

    def __repr__(self) -> str:
        return f"<AgentVersion agent={self.agent_id} v{self.version}>"


class AgentArtifact(Base):
    """Persistent builder memory — named text documents scoped per agent."""

    __tablename__ = "agent_artifacts"
    __table_args__ = (UniqueConstraint("agent_id", "name"),)

    id: Mapped[uuid.UUID] = mapped_column(Uuid, primary_key=True, default=uuid.uuid4)
    agent_id: Mapped[uuid.UUID] = mapped_column(
        Uuid,
        ForeignKey("agents.id", ondelete="CASCADE"),
        nullable=False,
    )
    name: Mapped[str] = mapped_column(String(255), nullable=False)
    content: Mapped[str] = mapped_column(Text, nullable=False, default="")
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), onupdate=func.now()
    )

    # Relationships
    agent: Mapped[Agent] = relationship(back_populates="artifacts")

    def __repr__(self) -> str:
        return f"<AgentArtifact {self.name} agent={self.agent_id}>"
