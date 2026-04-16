"""Pydantic schemas for the vibe-code builder conversations."""

from __future__ import annotations

import uuid
from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field

from app.schemas.agent import ActionCardSchema, ImportedConnection


class FileAttachment(BaseModel):
    """Metadata for a user-uploaded file attached to a message."""

    file_id: str
    filename: str
    total_lines: int


class BuilderMessageCreate(BaseModel):
    """User sends a message to the builder."""

    content: str = Field(
        ..., min_length=1, description="The user's natural language instruction"
    )
    attachments: list[FileAttachment] = Field(
        default_factory=list,
        description="Uploaded files attached to this message",
    )


class BuilderMessageResponse(BaseModel):
    """A single message in the builder conversation."""

    id: uuid.UUID
    role: str  # user | assistant
    parts: list[dict[str, Any]] = Field(default_factory=list)  # rendered parts for UI
    agent_version_id: uuid.UUID | None = None
    action_cards: list[ActionCardSchema] = Field(default_factory=list)
    mermaid_diagram: str | None = None
    imported_connections: list[ImportedConnection] = Field(default_factory=list)
    created_at: datetime

    model_config = {"from_attributes": True}


class BuilderConversationResponse(BaseModel):
    """Full builder conversation with all messages."""

    id: uuid.UUID
    agent_id: uuid.UUID
    messages: list[BuilderMessageResponse]
    created_at: datetime

    model_config = {"from_attributes": True}
