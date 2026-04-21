from __future__ import annotations

import uuid
from datetime import datetime

from typing import TYPE_CHECKING, Any

from sqlalchemy import (
    DateTime,
    Float,
    ForeignKey,
    Integer,
    String,
    Text,
    Uuid,
    func,
)

from sqlalchemy.dialects.postgresql import JSONB
from sqlalchemy.orm import Mapped, mapped_column, relationship

from app.lib.database import Base

if TYPE_CHECKING:
    from app.models.agent import Agent
    from app.models.call_event import CallEvent


class Call(Base):
    """A phone call handled by a voice agent."""

    __tablename__ = "calls"

    id: Mapped[uuid.UUID] = mapped_column(Uuid, primary_key=True, default=uuid.uuid4)
    agent_id: Mapped[uuid.UUID] = mapped_column(
        Uuid,
        ForeignKey("agents.id", ondelete="CASCADE"),
        nullable=False,
    )
    direction: Mapped[str] = mapped_column(
        String(10), nullable=False
    )  # inbound | outbound | webrtc (webrtc = browser-initiated)
    caller_number: Mapped[str | None] = mapped_column(String(20), nullable=True)
    callee_number: Mapped[str | None] = mapped_column(String(20), nullable=True)
    status: Mapped[str] = mapped_column(
        String(20), nullable=False, default="initiated"
    )  # initiated | ringing | in_progress | completed | failed
    provider_call_id: Mapped[str | None] = mapped_column(
        String(255), nullable=True, index=True
    )  # Twilio CallSid or Telnyx call_control_id
    duration_seconds: Mapped[int | None] = mapped_column(Integer, nullable=True)
    transcript_json: Mapped[dict[str, Any] | None] = mapped_column(JSONB, nullable=True)
    recording_url: Mapped[str | None] = mapped_column(Text, nullable=True)
    variables_json: Mapped[dict[str, Any] | None] = mapped_column(JSONB, nullable=True)
    outcome: Mapped[str | None] = mapped_column(String(50), nullable=True)
    sentiment_score: Mapped[float | None] = mapped_column(Float, nullable=True)
    agent_version_used: Mapped[int | None] = mapped_column(Integer, nullable=True)
    started_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    ended_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )

    # Relationships
    agent: Mapped[Agent] = relationship("Agent", back_populates="calls")
    events: Mapped[list[CallEvent]] = relationship(
        "CallEvent",
        back_populates="call",
        cascade="all, delete-orphan",
        order_by="CallEvent.seq",
    )

    def __repr__(self) -> str:
        return f"<Call {self.direction} agent={self.agent_id} ({self.status})>"
