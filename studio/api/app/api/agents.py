"""Agent CRUD API routes."""

import asyncio
import uuid
from typing import Any

from fastapi import APIRouter, Depends, HTTPException
from loguru import logger
from pydantic import BaseModel
from pydantic_ai import Agent as PydanticAiAgent
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, UserPromptPart
from pydantic_core import to_jsonable_python
from sqlalchemy import func as sa_func
from sqlalchemy import or_, select
from sqlalchemy.ext.asyncio import AsyncSession

from app.api.settings import get_builder_llm_config
from app.lib.config import LLMConfig
from app.lib.config_utils import extract_secret_keys
from app.lib.database import get_db
from app.lib.llm_factory import build_model
from app.models.agent import Agent, AgentStatus, AgentVersion
from app.models.conversation import BuilderConversation, BuilderMessage
from app.models.credential import Credential
from app.schemas.agent import (
    AgentCreate,
    AgentFullConfig,
    AgentImportRequest,
    AgentListResponse,
    AgentResponse,
    AgentUpdate,
    AgentVersionResponse,
    ImportedConnection,
    ImportValidationRequest,
    ImportValidationResponse,
)
from app.services.agent_import import (
    apply_import_mappings,
    unresolved_blocking_issues,
    validate_import_config,
)

try:
    from voice_engine import (
        check_tts_model_language,
        get_supported_languages,
        get_tts_model_catalog,
    )

    _SUPPORTED_LANGUAGES: list[dict[str, Any]] = get_supported_languages()
    _TTS_MODEL_CATALOG: list[dict[str, Any]] = get_tts_model_catalog()
except Exception:  # pragma: no cover
    _SUPPORTED_LANGUAGES = [
        {"code": "en", "label": "English"},
        {"code": "es", "label": "Spanish"},
        {"code": "fr", "label": "French"},
        {"code": "de", "label": "German"},
        {"code": "pt", "label": "Portuguese"},
    ]
    _TTS_MODEL_CATALOG = []

    def check_tts_model_language(provider: str, model_id: str, language: str) -> bool:
        """Fallback: always return True when voice_engine is unavailable."""
        return True


# Map: ISO 639-1 base code → full language dict (for validation + label lookup)
_LANGUAGE_MAP: dict[str, dict[str, Any]] = {
    lang["code"]: lang for lang in _SUPPORTED_LANGUAGES
}

router = APIRouter(prefix="/agents", tags=["agents"])


@router.get("", response_model=AgentListResponse)
async def list_agents(
    skip: int = 0,
    limit: int = 50,
    q: str | None = None,
    db: AsyncSession = Depends(get_db),
) -> AgentListResponse:
    """List all agents with pagination."""
    base_query = select(Agent)
    count_query = select(sa_func.count(Agent.id))

    q_value = q.strip() if q else ""
    if q_value:
        name_filter = Agent.name.ilike(f"%{q_value}%")
        base_query = base_query.where(name_filter)
        count_query = count_query.where(name_filter)

    # Count total
    count_result = await db.execute(count_query)
    total = count_result.scalar_one()

    # Fetch agents without eager loading versions
    result = await db.execute(
        base_query.order_by(Agent.updated_at.desc()).offset(skip).limit(limit)
    )
    agents = result.scalars().all()

    agent_ids = [a.id for a in agents]
    version_counts: dict[uuid.UUID, int] = {}
    active_configs: dict[uuid.UUID, dict[str, Any]] = {}

    if agent_ids:
        # 1. Get version counts for these agents in a single query
        count_stmt = (
            select(AgentVersion.agent_id, sa_func.count(AgentVersion.id))
            .where(AgentVersion.agent_id.in_(agent_ids))
            .group_by(AgentVersion.agent_id)
        )
        count_res = await db.execute(count_stmt)
        version_counts = {row[0]: row[1] for row in count_res.all()}

        # 2. Get active configs in a single query using OR conditions
        active_conditions = [
            (AgentVersion.agent_id == a.id) & (AgentVersion.version == a.active_version)
            for a in agents
            if a.active_version is not None
        ]

        if active_conditions:
            active_stmt = select(AgentVersion).where(or_(*active_conditions))
            active_res = await db.execute(active_stmt)
            for v in active_res.scalars().all():
                active_configs[v.agent_id] = v.config_json

    # Build response
    agent_responses = []
    for agent in agents:
        agent_responses.append(
            AgentResponse(
                id=agent.id,
                name=agent.name,
                description=agent.description,
                status=agent.status,
                active_version=agent.active_version,
                phone_number=agent.phone_number,
                created_at=agent.created_at,
                updated_at=agent.updated_at,
                current_config=active_configs.get(agent.id),
                version_count=version_counts.get(agent.id, 0),
            )
        )

    return AgentListResponse(agents=agent_responses, total=total)


@router.post("", response_model=AgentResponse, status_code=201)
async def create_agent(
    body: AgentCreate,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Create a new agent and its initial builder conversation."""
    agent = Agent(name=body.name, description=body.description)
    db.add(agent)
    await db.flush()

    # Create the initial builder conversation
    conversation = BuilderConversation(agent_id=agent.id)
    db.add(conversation)

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        current_config=None,
        version_count=0,
    )


@router.get("/{agent_id}/export", response_model=AgentFullConfig)
async def export_agent_full_config(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> AgentFullConfig:
    """Export a full agent payload (config + mermaid + connection metadata)."""
    agent_result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = agent_result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    version: AgentVersion | None = None
    if agent.active_version is not None:
        version_result = await db.execute(
            select(AgentVersion).where(
                AgentVersion.agent_id == agent_id,
                AgentVersion.version == agent.active_version,
            )
        )
        version = version_result.scalar_one_or_none()
    if version is None:
        version_result = await db.execute(
            select(AgentVersion)
            .where(AgentVersion.agent_id == agent_id)
            .order_by(AgentVersion.version.desc())
            .limit(1)
        )
        version = version_result.scalar_one_or_none()
    if version is None:
        raise HTTPException(status_code=400, detail="Agent has no config to export")

    conv_result = await db.execute(
        select(BuilderConversation)
        .where(BuilderConversation.agent_id == agent_id)
        .order_by(BuilderConversation.created_at.desc())
        .limit(1)
    )
    conversation = conv_result.scalar_one_or_none()

    mermaid_diagram: str | None = None
    imported_connections: list[ImportedConnection] = []
    if conversation is not None:
        msg_result = await db.execute(
            select(BuilderMessage)
            .where(BuilderMessage.conversation_id == conversation.id)
            .order_by(BuilderMessage.created_at.desc())
        )
        for msg in msg_result.scalars().all():
            if not isinstance(msg.metadata_json, dict):
                continue
            if mermaid_diagram is None and isinstance(
                msg.metadata_json.get("mermaid_diagram"), str
            ):
                mermaid_diagram = msg.metadata_json.get("mermaid_diagram")
            if not imported_connections and isinstance(
                msg.metadata_json.get("imported_connections"), list
            ):
                imported_connections = [
                    ImportedConnection.model_validate(item)
                    for item in msg.metadata_json.get("imported_connections", [])
                    if isinstance(item, dict)
                ]
            if mermaid_diagram is not None and imported_connections:
                break

    if not imported_connections:
        imported_connections = await _build_connection_export_rows(
            db,
            version.config_json,
            agent_id,
        )

    return AgentFullConfig(
        name=agent.name,
        description=agent.description,
        config=version.config_json,
        mermaid_diagram=mermaid_diagram,
        connections=imported_connections,
    )


@router.post("/import/validate", response_model=ImportValidationResponse)
async def validate_import(
    body: ImportValidationRequest,
    db: AsyncSession = Depends(get_db),
) -> ImportValidationResponse:
    """Validate a raw imported agent config and return structured issues."""
    return await validate_import_config(db, body.config)


@router.post("/import", response_model=AgentResponse, status_code=201)
async def import_agent(
    body: AgentImportRequest,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Create an agent from imported config with strict or map-defaults strategy."""
    initial_result = await validate_import_config(db, body.full_config.config)

    mapping_input = {
        path: value
        for path, value in body.mappings.items()
        if path in {"tts_provider", "tts_model", "voice_id"} and value
    }

    effective_config = initial_result.normalized_config
    if body.mapping_mode == "map_defaults":
        mappable_issue_paths = {
            issue.path
            for issue in initial_result.fulfillment_issues
            if issue.mappable and issue.path
        }
        merged_mappings = {
            path: value
            for path, value in initial_result.suggested_mappings.items()
            if path in mappable_issue_paths
        }
        merged_mappings.update(mapping_input)
        effective_config = apply_import_mappings(effective_config, merged_mappings)
    elif mapping_input:
        effective_config = apply_import_mappings(effective_config, mapping_input)

    final_result = await validate_import_config(db, effective_config)
    blocking_issues = unresolved_blocking_issues(final_result)
    if blocking_issues:
        raise HTTPException(
            status_code=422,
            detail={
                "code": "import_validation_failed",
                "message": "Imported config has unresolved blocking issues.",
                "issues": [issue.model_dump() for issue in blocking_issues],
                "schema_issues": [
                    issue.model_dump() for issue in final_result.schema_issues
                ],
                "fulfillment_issues": [
                    issue.model_dump() for issue in final_result.fulfillment_issues
                ],
            },
        )

    applied_change_summary = "Imported from raw config"
    if body.mapping_mode == "map_defaults":
        applied_change_summary = "Imported from raw config with default mappings"

    agent = Agent(name=body.name.strip(), description=body.description)
    db.add(agent)
    await db.flush()

    version = AgentVersion(
        agent_id=agent.id,
        version=1,
        config_json=final_result.normalized_config,
        change_summary=applied_change_summary,
    )
    db.add(version)

    agent.active_version = 1
    conversation = BuilderConversation(agent_id=agent.id)
    db.add(conversation)

    await db.flush()
    if body.full_config.mermaid_diagram or body.full_config.connections:
        imported_msg = BuilderMessage(
            conversation_id=conversation.id,
            role="assistant",
            parts=[
                {
                    "kind": "text",
                    "content": "Imported full configuration into the builder workspace.",
                }
            ],
            agent_version_id=version.id,
            metadata_json={
                "mermaid_diagram": body.full_config.mermaid_diagram,
                "imported_connections": [
                    conn.model_dump() for conn in body.full_config.connections
                ],
            },
        )
        db.add(imported_msg)
        await db.flush()

    await db.refresh(agent)

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        current_config=final_result.normalized_config,
        version_count=1,
    )


async def _build_connection_export_rows(
    db: AsyncSession,
    config: dict[str, Any],
    agent_id: uuid.UUID,
) -> list[ImportedConnection]:
    used_providers = extract_secret_keys(config)
    if not used_providers:
        return []

    creds_result = await db.execute(
        select(Credential).where(
            Credential.provider.in_(used_providers),
            (Credential.agent_id == agent_id) | Credential.agent_id.is_(None),
        )
    )
    rows = creds_result.scalars().all()

    agent_rows: dict[str, Credential] = {}
    default_rows: dict[str, Credential] = {}
    for row in rows:
        if row.agent_id == agent_id and row.provider not in agent_rows:
            agent_rows[row.provider] = row
        if row.agent_id is None and row.provider not in default_rows:
            default_rows[row.provider] = row

    out: list[ImportedConnection] = []
    for provider in sorted(used_providers):
        if provider in agent_rows:
            cred = agent_rows[provider]
            out.append(
                ImportedConnection(
                    provider=provider,
                    name=cred.name,
                    auth_type=cred.auth_type,
                    is_default=False,
                    status="connected",
                )
            )
            continue
        if provider in default_rows:
            cred = default_rows[provider]
            out.append(
                ImportedConnection(
                    provider=provider,
                    name=cred.name,
                    auth_type=cred.auth_type,
                    is_default=True,
                    status="inherited",
                )
            )
            continue
        out.append(
            ImportedConnection(
                provider=provider,
                name=None,
                auth_type=None,
                is_default=False,
                status="missing",
            )
        )
    return out


@router.get("/languages")
async def list_agent_languages() -> list[dict[str, Any]]:
    """Return the supported language options for the agent language picker.

    Source of truth: Rust ``language_config::SUPPORTED_LANGUAGES``, compiled
    into the ``voice_engine`` native extension and exposed via PyO3.
    """
    return _SUPPORTED_LANGUAGES


@router.get("/tts-models")
async def list_tts_models(language: str | None = None) -> list[dict[str, Any]]:
    """Return the curated TTS model catalog, optionally filtered by language.

    Query parameters
    ----------------
    language
        ISO 639-1 base code (e.g. ``zh``, ``ja``).  When supplied, only models
        that can synthesize the requested language are returned.  Omit to
        return the full catalog.

    Each item has the shape::

        {
          "provider": "cartesia-ws",
          "model_id": "sonic-2",
          "label": "Cartesia Sonic 2",
          "supported_languages": ["en", "es", ...],
          "language_voices": [
            {"language_code": "en", "voice_id": "...", "voice_label": "..."}
          ]
        }

    Source of truth: ``voice_engine.language_config::TTS_MODEL_CATALOG``.
    """
    catalog = _TTS_MODEL_CATALOG
    if language:
        base_lang = language.split("-")[0].lower()
        catalog = [m for m in catalog if base_lang in m.get("supported_languages", [])]
    return catalog


@router.get("/{agent_id}", response_model=AgentResponse)
async def get_agent(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Get a single agent with its current config."""
    result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    # Count versions
    version_count_result = await db.execute(
        select(sa_func.count(AgentVersion.id)).where(AgentVersion.agent_id == agent.id)
    )
    version_count = version_count_result.scalar_one()

    # Get current config
    current_config = None
    if agent.active_version is not None:
        version_result = await db.execute(
            select(AgentVersion).where(
                AgentVersion.agent_id == agent.id,
                AgentVersion.version == agent.active_version,
            )
        )
        active = version_result.scalar_one_or_none()
        if active:
            current_config = active.config_json

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        current_config=current_config,
        version_count=version_count,
    )


@router.patch("/{agent_id}", response_model=AgentResponse)
async def update_agent(
    agent_id: uuid.UUID,
    body: AgentUpdate,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Update agent metadata (name, description, status, phone number)."""
    result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    update_data = body.model_dump(exclude_unset=True)
    _updatable_fields = {"name", "description", "status", "phone_number"}
    for key, value in update_data.items():
        if key in _updatable_fields:
            setattr(agent, key, value)

    # Flush so the DB assigns a fresh updated_at before we read it back
    await db.flush()
    await db.refresh(agent)

    # Fetch version count so the response is not stale
    version_count_result = await db.execute(
        select(sa_func.count(AgentVersion.id)).where(AgentVersion.agent_id == agent.id)
    )
    version_count = version_count_result.scalar_one()

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        version_count=version_count,
    )


@router.delete("/{agent_id}", status_code=204)
async def delete_agent(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> None:
    """Delete an agent and all its versions, conversations, and calls."""
    result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    await db.delete(agent)


class AgentConfigPatch(BaseModel):
    """Fields that can be patched directly on the agent's active config."""

    language: str | None = None
    timezone: str | None = None
    voice_id: str | None = None
    tts_provider: str | None = None
    tts_model: str | None = None
    gemini_live_model: str | None = None
    regenerate_greeting: bool = False


@router.patch("/{agent_id}/config", response_model=AgentResponse)
async def patch_agent_config(
    agent_id: uuid.UUID,
    body: AgentConfigPatch,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Patch fields on the agent's active config (v3_graph entry node)."""
    result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")
    if agent.active_version is None:
        raise HTTPException(status_code=400, detail="Agent has no active version")

    version_result = await db.execute(
        select(AgentVersion).where(
            AgentVersion.agent_id == agent_id,
            AgentVersion.version == agent.active_version,
        )
    )
    version = version_result.scalar_one_or_none()
    if not version:
        raise HTTPException(status_code=404, detail="Active version not found")

    config = dict(version.config_json)
    patch = body.model_dump(exclude_unset=True, exclude={"regenerate_greeting"})
    force_regen = body.regenerate_greeting
    if not patch and not force_regen:
        raise HTTPException(status_code=400, detail="No fields to update")

    # ── Language validation ──────────────────────────────────────────────
    if "language" in patch and patch["language"]:
        raw_lang: str = patch["language"]
        base_lang = raw_lang.split("-")[0].lower()  # BCP-47 normalisation
        if base_lang not in _LANGUAGE_MAP:
            raise HTTPException(
                status_code=422,
                detail=(
                    f"Unsupported language '{raw_lang}'. "
                    f"Supported codes: {sorted(_LANGUAGE_MAP.keys())}"
                ),
            )
        patch["language"] = base_lang

    # ── Model-language compatibility check ───────────────────────────────────
    # Non-blocking: we write the warning into the response so the frontend can
    # show an amber alert. The config is saved regardless — forcing a hard error
    # here would prevent users from fixing the model first.
    model_warning: str | None = None
    effective_language = patch.get("language") or config.get("language", "en")
    if effective_language and effective_language != "en":
        tts_provider: str = patch.get("tts_provider") or config.get("tts_provider", "")
        tts_model: str = patch.get("tts_model") or config.get("tts_model", "")
        if (
            tts_provider
            and tts_model
            and not check_tts_model_language(
                tts_provider, tts_model, effective_language
            )
        ):
            lang_label_for_warn = _LANGUAGE_MAP.get(effective_language, {}).get(
                "label", effective_language
            )
            # Suggest the first compatible multilingual model for this provider
            compatible = [
                m["model_id"]
                for m in _TTS_MODEL_CATALOG
                if m["provider"] == tts_provider
                and effective_language in m.get("supported_languages", [])
            ][:2]
            suggestion = ", ".join(compatible) if compatible else "a multilingual model"
            model_warning = (
                f"'{tts_model}' does not support {lang_label_for_warn}. "
                f"Switch to: {suggestion}."
            )

    # Detect language change for greeting regeneration
    old_language = config.get("language", "en")
    new_language = patch.get("language")
    language_changed = new_language is not None and new_language != old_language

    for key, value in patch.items():
        config[key] = value

    # ── Greeting regeneration ──────────────────────────────────────────
    greeting_updated = False
    new_greeting: str | None = None

    if language_changed or (force_regen and effective_language):
        regen_lang = new_language or effective_language
        lang_label = _LANGUAGE_MAP.get(regen_lang, {}).get("label") or regen_lang
        try:
            builder_llm_cfg = await get_builder_llm_config(db)
            new_greeting = await _regenerate_greeting(
                agent_name=agent.name,
                config=config,
                language_code=regen_lang,
                language_label=lang_label,
                llm_config=builder_llm_cfg,
            )
            entry_key = config.get("entry", "entry")
            if "nodes" in config and entry_key in config["nodes"]:
                config["nodes"][entry_key]["greeting"] = new_greeting
                greeting_updated = True
        except Exception as exc:
            logger.warning(
                "Failed to regenerate greeting for language {}: {}", new_language, exc
            )

    version.config_json = config
    await db.flush()

    # Track manual change in builder conversation history
    # (so the LLM knows we changed language/timezone/voice)
    await _inject_config_change_event(
        db,
        agent_id=agent.id,
        patch=patch,
        greeting_updated=greeting_updated,
        new_greeting=new_greeting,
    )

    version_count_result = await db.execute(
        select(sa_func.count(AgentVersion.id)).where(AgentVersion.agent_id == agent.id)
    )
    version_count = version_count_result.scalar_one()

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        current_config=config,
        version_count=version_count,
        greeting_updated=greeting_updated,
        greeting=new_greeting,
        model_warning=model_warning,
    )


async def _inject_config_change_event(
    db: AsyncSession,
    agent_id: uuid.UUID,
    patch: dict[str, str],
    greeting_updated: bool = False,
    new_greeting: str | None = None,
) -> None:
    """Inject a manual configuration change event into the builder conversation history.

    This ensures the builder LLM is aware of the change and doesn't make
    conflicting or redundant edits in the future.
    """
    # 1. Fetch the conversation
    conv_result = await db.execute(
        select(BuilderConversation).where(BuilderConversation.agent_id == agent_id)
    )
    conversation = conv_result.scalars().first()
    if not conversation:
        return

    # 2. Build the notification text
    changes = []
    if "language" in patch:
        changes.append(f"language set to {patch['language']}")
    if "timezone" in patch:
        changes.append(f"timezone set to {patch['timezone']}")
    if "voice_id" in patch:
        changes.append(f"voice_id set to {patch['voice_id']}")
    if "gemini_live_model" in patch:
        mode = (
            "Native Multimodal (Gemini Live)"
            if patch["gemini_live_model"]
            else "Standard Pipeline"
        )
        changes.append(f"conversation mode set to {mode}")

    if not changes:
        return

    change_str = ", ".join(changes)
    msg_text = (
        f"I manually updated the agent configuration in the settings: {change_str}."
    )
    if greeting_updated and new_greeting:
        msg_text += f" The greeting was automatically updated to: '{new_greeting}'."

    # 3. Add to persisted_context (the LLM's memory)
    # We add a User Request + Assistant Response pair to keep the history balanced.
    user_req = ModelRequest(parts=[UserPromptPart(content=msg_text)])
    assistant_res = ModelResponse(
        parts=[
            TextPart(
                content="Got it. I've noted those changes to the agent's configuration."
            )
        ]
    )

    history = (
        list(conversation.persisted_context) if conversation.persisted_context else []
    )
    history.append(to_jsonable_python(user_req))
    history.append(to_jsonable_python(assistant_res))
    conversation.persisted_context = history


# ── Greeting regeneration helper ───────────────────────────────────────────

_GREETING_PROMPT = """\
You are updating a voice AI agent's greeting message.

Internal agent identifier: {agent_name}
Agent purpose (excerpt): {system_prompt_excerpt}
New language: {language_label} ({language_code})

Write a natural, brief greeting (1-2 sentences) that:
- Is entirely in {language_label}
- Welcomes the caller warmly
- Fits the agent's purpose
- Does NOT mention the agent's name, ID, or any internal identifier
- Does NOT say "I'm ..." or otherwise introduce the agent by name unless the
  agent purpose explicitly requires a branded introduction

Respond with ONLY the greeting text. No quotes. No explanation.\
"""


async def _regenerate_greeting(
    agent_name: str,
    config: dict[str, Any],
    language_code: str,
    language_label: str,
    llm_config: LLMConfig | None = None,
) -> str:
    """Call the builder LLM to produce a greeting in the new language."""
    entry_key = config.get("entry", "entry")
    system_prompt_excerpt = (
        config.get("nodes", {}).get(entry_key, {}).get("system_prompt", "")
    )[:300]

    prompt = _GREETING_PROMPT.format(
        agent_name=agent_name,
        system_prompt_excerpt=system_prompt_excerpt,
        language_label=language_label,
        language_code=language_code,
    )

    model, _ = build_model(llm_config or LLMConfig())
    runner: PydanticAiAgent[None, str] = PydanticAiAgent(model=model, output_type=str)
    result = await asyncio.wait_for(runner.run(prompt), timeout=30)
    return result.output.strip()


# ── Version Management ────────────────────────────────────────────


@router.get("/{agent_id}/versions", response_model=list[AgentVersionResponse])
async def list_versions(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> list[AgentVersionResponse]:
    """List all versions of an agent's config."""
    result = await db.execute(
        select(AgentVersion)
        .where(AgentVersion.agent_id == agent_id)
        .order_by(AgentVersion.version.desc())
    )
    versions = result.scalars().all()

    return [
        AgentVersionResponse(
            id=v.id,
            agent_id=v.agent_id,
            version=v.version,
            config=v.config_json,
            change_summary=v.change_summary,
            created_at=v.created_at,
        )
        for v in versions
    ]


@router.post("/{agent_id}/deploy/{version}", response_model=AgentResponse)
async def deploy_version(
    agent_id: uuid.UUID,
    version: int,
    db: AsyncSession = Depends(get_db),
) -> AgentResponse:
    """Deploy a specific version of the agent (make it active)."""
    # Verify agent exists
    agent_result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = agent_result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    # Verify version exists
    version_result = await db.execute(
        select(AgentVersion).where(
            AgentVersion.agent_id == agent_id,
            AgentVersion.version == version,
        )
    )
    agent_version = version_result.scalar_one_or_none()
    if not agent_version:
        raise HTTPException(status_code=404, detail="Version not found")

    agent.active_version = version
    agent.status = AgentStatus.ACTIVE

    return AgentResponse(
        id=agent.id,
        name=agent.name,
        description=agent.description,
        status=agent.status,
        active_version=agent.active_version,
        phone_number=agent.phone_number,
        created_at=agent.created_at,
        updated_at=agent.updated_at,
        current_config=agent_version.config_json,
    )


@router.post("/{agent_id}/revert/{version}", response_model=AgentVersionResponse)
async def revert_version(
    agent_id: uuid.UUID,
    version: int,
    db: AsyncSession = Depends(get_db),
) -> AgentVersionResponse:
    """Copy a historical version as a new latest version without changing active version."""
    agent_result = await db.execute(select(Agent).where(Agent.id == agent_id))
    agent = agent_result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    version_result = await db.execute(
        select(AgentVersion).where(
            AgentVersion.agent_id == agent_id,
            AgentVersion.version == version,
        )
    )
    source_version = version_result.scalar_one_or_none()
    if not source_version:
        raise HTTPException(status_code=404, detail="Version not found")

    latest_result = await db.execute(
        select(sa_func.max(AgentVersion.version)).where(
            AgentVersion.agent_id == agent_id
        )
    )
    latest_version = latest_result.scalar_one_or_none() or 0

    new_version = AgentVersion(
        agent_id=agent_id,
        version=latest_version + 1,
        config_json=source_version.config_json,
        change_summary=f"Reverted from v{version}",
    )
    db.add(new_version)
    await db.flush()
    await db.refresh(new_version)

    return AgentVersionResponse(
        id=new_version.id,
        agent_id=new_version.agent_id,
        version=new_version.version,
        config=new_version.config_json,
        change_summary=new_version.change_summary,
        created_at=new_version.created_at,
    )
