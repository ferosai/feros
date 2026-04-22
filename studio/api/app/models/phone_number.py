from __future__ import annotations

import uuid
from datetime import datetime
from typing import TYPE_CHECKING

from sqlalchemy import (
    Boolean,
    DateTime,
    ForeignKey,
    String,
    Text,
    UniqueConstraint,
    Uuid,
    func,
)
from sqlalchemy.orm import Mapped, mapped_column, relationship

from app.lib.database import Base

if TYPE_CHECKING:
    from app.models.agent import Agent


class PhoneNumber(Base):
    """A phone number owned by the user on a telephony provider (Twilio or Telnyx).

    Lifecycle:
      - Numbers are imported from the provider account and stored in this table.
      - A number may be assigned to at most one agent at a time (agent_id is nullable).
      - On assignment, the backend automatically updates the provider's webhook
        configuration so incoming calls are routed correctly.
    """

    __tablename__ = "phone_numbers"
    __table_args__ = (
        UniqueConstraint("phone_number", name="uq_phone_numbers_e164"),
    )
    id: Mapped[uuid.UUID] = mapped_column(Uuid, primary_key=True, default=uuid.uuid4)

    # Provider info
    provider: Mapped[str] = mapped_column(
        String(20), nullable=False
    )  # "twilio" | "telnyx"
    phone_number: Mapped[str] = mapped_column(
        String(30), nullable=False
    )  # E.164 format: +15551234567
    provider_sid: Mapped[str | None] = mapped_column(
        String(128), nullable=True
    )  # Twilio IncomingPhoneNumber SID or Telnyx phone_number_id
    friendly_name: Mapped[str | None] = mapped_column(String(255), nullable=True)

    # Assignment
    agent_id: Mapped[uuid.UUID | None] = mapped_column(
        Uuid,
        ForeignKey("agents.id", ondelete="SET NULL"),
        nullable=True,
        index=True,
    )

    # Per-number snapshot of the voice-server URL used on the last successful assignment.
    voice_server_url: Mapped[str | None] = mapped_column(
        Text, nullable=True
    )  # e.g. https://voice.myapp.com

    # Per-number encrypted provider credentials (JSON blob with ciphertext + iv).
    # Stored at import time so each number can use its own provider account.
    provider_credentials_encrypted: Mapped[str | None] = mapped_column(
        Text, nullable=True
    )

    # Telnyx-specific: the phone number requires a "connection" (Application) for
    # webhook delivery. We store the connection_id so we can update it on assign.
    telnyx_connection_id: Mapped[str | None] = mapped_column(String(128), nullable=True)

    is_active: Mapped[bool] = mapped_column(
        Boolean, nullable=False, server_default="true"
    )

    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), onupdate=func.now()
    )

    # Relationships
    agent: Mapped[Agent | None] = relationship("Agent", back_populates="phone_numbers")

    def __repr__(self) -> str:
        assigned = f" → agent={self.agent_id}" if self.agent_id else " (unassigned)"
        return f"<PhoneNumber {self.phone_number} [{self.provider}]{assigned}>"

