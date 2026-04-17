"""Pydantic schemas for agents and agent configs.

These schemas define the API contract and are used by FastAPI to
auto-generate the OpenAPI spec → which feeds `openapi-typescript`
for TS type generation.
"""

from __future__ import annotations

import ipaddress
import socket
import uuid
from datetime import datetime
from typing import Any, Literal
from urllib.parse import urlparse

from pydantic import BaseModel, Field, field_validator

from app.models.agent import AgentStatus

# ── Tool Config (used inside AgentConfig) ──────────────────────────


class ToolParameterSchema(BaseModel):
    """A parameter the agent extracts from the conversation to call a tool."""

    name: str = Field(..., description="Parameter name, e.g. 'order_id'")
    description: str = Field(..., description="What to ask the caller for")
    type: str = Field(
        default="string", description="Parameter type: string | number | boolean"
    )
    required: bool = Field(default=True)


# Private / link-local CIDR ranges that must never be tool targets (SSRF)
_PRIVATE_NETWORKS = [
    ipaddress.ip_network(cidr)
    for cidr in (
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "127.0.0.0/8",
        "169.254.0.0/16",  # link-local / cloud IMDS
        "::1/128",
        "fc00::/7",
    )
]


class ToolConfigSchema(BaseModel):
    """A tool the voice agent can invoke during a call."""

    name: str = Field(..., description="Tool identifier, e.g. 'check_order_status'")
    description: str = Field(..., description="What this tool does — shown to the LLM")
    endpoint: str = Field(..., description="HTTPS endpoint to call")
    method: Literal["GET", "POST", "PUT", "PATCH", "DELETE"] = Field(
        default="GET", description="HTTP method"
    )
    headers: dict[str, str] = Field(
        default_factory=dict, description="Static request headers (e.g. Content-Type)"
    )
    credential_id: str | None = Field(
        default=None,
        description="ID of the encrypted credential to inject at call time",
    )
    parameters: list[ToolParameterSchema] = Field(default_factory=list)
    body_template: dict[str, Any] | None = Field(
        default=None,
        description="Nested JSON body with {{slot}} placeholders. "
        "When present, overrides params for request body construction.",
    )
    query_params: dict[str, str] | None = Field(
        default=None,
        description="Explicit URL query parameters with {{slot}} placeholders.",
    )
    response_template: str = Field(
        default="",
        description="Template for how to speak the result to the caller",
    )

    @field_validator("endpoint")
    @classmethod
    def endpoint_must_be_safe(cls, v: str) -> str:
        """Reject non-HTTPS schemes and private/loopback addresses (SSRF guard).

        Tool endpoints must be reachable public HTTPS URLs. Allowing
        arbitrary URLs would let a malicious config target cloud metadata
        services (e.g. http://169.254.169.254) or internal infrastructure.
        """
        parsed = urlparse(v)
        if parsed.scheme != "https":
            raise ValueError(
                f"Tool endpoint must use HTTPS (got '{parsed.scheme}://'). "
                "Unencrypted or non-HTTP schemes are not allowed."
            )
        hostname = parsed.hostname or ""
        try:
            # Resolve hostname to IPs to catch DNS rebinding/localhost bypasses
            addr_info = socket.getaddrinfo(hostname, None)
            for result in addr_info:
                ip_str = result[4][0]
                addr = ipaddress.ip_address(ip_str)
                for net in _PRIVATE_NETWORKS:
                    if addr in net:
                        raise ValueError(
                            f"Tool endpoint '{hostname}' resolves to a private/reserved "
                            "address. Only public endpoints are allowed."
                        )
        except socket.gaierror:
            # If we can't resolve it, we'll let the HTTP client fail rather than blocking
            pass
        except ValueError as exc:
            # Re-raise our own SSRF errors
            if "private/reserved" in str(exc) or "HTTPS" in str(exc):
                raise
        return v


class ActionCardSchema(BaseModel):
    """A UI action the frontend should render for secure credential collection.

    Emitted by the builder LLM when it detects the user wants an
    integration that requires credentials. The frontend renders this
    as a styled card with a button that opens a secure credential modal.
    """

    type: Literal["connect_credential", "oauth_redirect"] = Field(
        ..., description="Action type"
    )
    skill: str = Field(..., description="Skill name, e.g. 'airtable'")
    title: str = Field(..., description="Card title, e.g. 'Connect Airtable'")
    description: str = Field(..., description="Explanation of what to connect")
    help_url: str | None = Field(
        default=None, description="Link to docs on getting credentials"
    )


# ── Guardrail Config ──────────────────────────────────────────────


class GuardrailConfigSchema(BaseModel):
    """Safety guardrails for the voice agent."""

    forbidden_topics: list[str] = Field(
        default_factory=list,
        description="Topics the agent must not discuss",
    )
    escalation_triggers: list[str] = Field(
        default_factory=list,
        description="Situations that trigger human handoff",
    )
    escalation_action: str = Field(
        default="transfer",
        description="What to do on escalation: transfer | take_message | hang_up",
    )


# ── API Request/Response Schemas ──────────────────────────────────


class AgentCreate(BaseModel):
    """Request body for creating a new agent (minimal — builder fills the rest)."""

    name: str = Field(..., min_length=1, max_length=255)
    description: str | None = None


class AgentUpdate(BaseModel):
    """Request body for updating agent metadata."""

    name: str | None = Field(default=None, max_length=255)
    description: str | None = None
    status: AgentStatus | None = None
    phone_number: str | None = None


class ImportIssue(BaseModel):
    """Structured import issue for schema/fulfillment validation."""

    source: Literal["schema", "fulfillment"]
    code: str
    path: str | None = None
    message: str
    severity: Literal["error", "warning"] = "error"
    blocking: bool = True
    mappable: bool = False
    suggested_value: str | None = None


class ImportedConnection(BaseModel):
    """Connection metadata included in full config export/import."""

    provider: str
    name: str | None = None
    auth_type: str | None = None
    is_default: bool = False
    status: Literal["connected", "inherited", "missing"] = "missing"


class AgentFullConfig(BaseModel):
    """Superset export/import payload for agent portability."""

    schema_uri: Literal["https://feros.ai/schemas/agent-config-v3.schema.json"] = Field(
        default="https://feros.ai/schemas/agent-config-v3.schema.json",
        alias="$schema",
        description="Canonical schema URI — must match exactly for v3 payloads.",
    )
    name: str
    description: str | None = None
    config: dict[str, Any]
    mermaid_diagram: str | None = None
    connections: list[ImportedConnection] = Field(default_factory=list)

    model_config = {"populate_by_name": True}


class ImportValidationRequest(BaseModel):
    """Request body for import validation preview."""

    config: dict[str, Any]


class ImportValidationResponse(BaseModel):
    """Validation result for imported config."""

    schema_valid: bool
    schema_issues: list[ImportIssue]
    fulfillable: bool
    fulfillment_issues: list[ImportIssue]
    suggested_mappings: dict[str, str] = Field(default_factory=dict)
    normalized_config: dict[str, Any]


class AgentImportRequest(BaseModel):
    """Request body for final agent import."""

    name: str = Field(..., min_length=1, max_length=255)
    description: str | None = None
    full_config: AgentFullConfig
    mapping_mode: Literal["strict", "map_defaults"] = "strict"
    mappings: dict[str, str] = Field(default_factory=dict)


class AgentVersionResponse(BaseModel):
    """Response for a single agent version."""

    id: uuid.UUID
    agent_id: uuid.UUID
    version: int
    config: dict[str, Any]
    change_summary: str | None
    created_at: datetime

    model_config = {"from_attributes": True}


class AgentResponse(BaseModel):
    """Full agent response including current config."""

    id: uuid.UUID
    name: str
    description: str | None
    status: str
    active_version: int | None
    phone_number: str | None
    created_at: datetime
    updated_at: datetime
    current_config: dict[str, Any] | None = None
    version_count: int = 0
    greeting_updated: bool = False
    greeting: str | None = None
    # Non-empty when the configured TTS model doesn't support the agent's language.
    # The frontend should surface this as an amber (non-blocking) warning.
    model_warning: str | None = None

    model_config = {"from_attributes": True}


class AgentListResponse(BaseModel):
    """Paginated list of agents."""

    agents: list[AgentResponse]
    total: int
