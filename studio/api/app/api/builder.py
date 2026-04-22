"""Vibe-code builder API — the core of Voice Agent OS.

Users send natural language messages, the builder LLM generates/updates
structured agent graphs, and we persist both the conversation and config versions.
"""

import asyncio
import json
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Depends, File, HTTPException, UploadFile
from fastapi.responses import StreamingResponse
from loguru import logger
from sqlalchemy import select, update
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from app.agent_builder import builder_service
from app.agent_builder.deps import BuilderResult
from app.lib.auth import TenantContext, require_tenant
from app.lib.database import async_session, get_db
from app.lib.file_store import extract_text, file_store
from app.models.agent import Agent, AgentVersion
from app.models.conversation import BuilderConversation, BuilderMessage
from app.models.provider import ProviderConfig
from app.schemas.conversation import (
    BuilderConversationResponse,
    BuilderMessageCreate,
    BuilderMessageResponse,
)

router = APIRouter(prefix="/agents/{agent_id}/builder", tags=["builder"])

MAX_DOC_UPLOAD_BYTES = 20 * 1024 * 1024  # 20 MB
ALLOWED_DOC_EXTENSIONS = {".pdf", ".txt", ".md", ".docx"}


@dataclass
class _BuildComplete:
    """Internal wrapper — carries the build result plus DB-assigned version number."""

    result: BuilderResult
    version_num: int | None


_pending_tasks: set[asyncio.Task[Any]] = set()


@router.get("/conversation", response_model=BuilderConversationResponse)
async def get_conversation(
    agent_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> BuilderConversationResponse:
    """Get the builder conversation for an agent (with all messages)."""
    # Verify agent ownership first
    agent_query = select(Agent.id).where(Agent.id == agent_id)
    agent_result = await db.execute(agent_query)
    if not agent_result.scalar_one_or_none():
        raise HTTPException(status_code=404, detail="Agent not found")

    result = await db.execute(
        select(BuilderConversation)
        .where(BuilderConversation.agent_id == agent_id)
        .options(selectinload(BuilderConversation.messages))
        .order_by(BuilderConversation.created_at.desc())
    )
    conversation = result.scalars().first()

    if not conversation:
        raise HTTPException(status_code=404, detail="No builder conversation found")

    return BuilderConversationResponse(
        id=conversation.id,
        agent_id=conversation.agent_id,
        messages=[
            BuilderMessageResponse(
                id=m.id,
                role=m.role,
                parts=m.parts or [],
                agent_version_id=m.agent_version_id,
                action_cards=(
                    m.metadata_json.get("action_cards", [])
                    if isinstance(m.metadata_json, dict)
                    else []
                ),
                mermaid_diagram=(
                    m.metadata_json.get("mermaid_diagram")
                    if isinstance(m.metadata_json, dict)
                    else None
                ),
                imported_connections=(
                    m.metadata_json.get("imported_connections", [])
                    if isinstance(m.metadata_json, dict)
                    else []
                ),
                created_at=m.created_at,
            )
            for m in conversation.messages
        ],
        created_at=conversation.created_at,
    )


@router.post("/upload")
async def upload_document(
    agent_id: uuid.UUID,
    file: UploadFile = File(...),
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> dict[str, Any]:
    """Upload a document for the builder to use as knowledge context.

    Accepts PDF, TXT, MD, DOCX (max 20 MB). Extracts text and stores
    it in-memory for the builder session.
    """
    # Verify agent exists
    agent_query = select(Agent).where(Agent.id == agent_id)
    agent_result = await db.execute(agent_query)
    if not agent_result.scalar_one_or_none():
        raise HTTPException(status_code=404, detail="Agent not found")

    filename = file.filename or "unknown.txt"
    ext = Path(filename).suffix.lower()
    if ext not in ALLOWED_DOC_EXTENSIONS:
        raise HTTPException(
            status_code=415,
            detail=f"Unsupported file type '{ext}'. Allowed: {', '.join(sorted(ALLOWED_DOC_EXTENSIONS))}",
        )

    content = await file.read(MAX_DOC_UPLOAD_BYTES + 1)
    if len(content) > MAX_DOC_UPLOAD_BYTES:
        raise HTTPException(status_code=413, detail="File too large (max 20 MB)")

    try:
        text = extract_text(content, filename)
    except ValueError as e:
        raise HTTPException(status_code=422, detail=str(e)) from e

    if not text.strip():
        raise HTTPException(status_code=422, detail="No text content found in file")

    doc_id = str(uuid.uuid4())
    agent_key = str(agent_id)
    stored = file_store.add_text_file(
        agent_id=agent_key,
        file_id=doc_id,
        filename=filename,
        text=text,
    )

    summary = text[:500] + ("..." if len(text) > 500 else "")

    logger.info(
        "Builder doc upload: {} ({} lines) for agent {}",
        filename,
        stored.total_lines,
        agent_id,
    )

    return {
        "file_id": doc_id,
        "filename": filename,
        "summary": summary,
        "total_lines": stored.total_lines,
        "text_length": len(text),
    }


# ── Endpoints ────────────────────────────────────────────────────


_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


@router.post("/stream")
async def stream_message(
    agent_id: uuid.UUID,
    body: BuilderMessageCreate,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> StreamingResponse:
    """SSE streaming version of the builder endpoint.

    Streams the response as Server-Sent Events:
     - event: part       — structured streaming events (text, thinking, tool calls)
     - event: mermaid_start — mermaid generation has started
     - event: config   — updated agent config JSON
     - event: action_cards — credential/auth prompts
     - event: mermaid  — flow diagram code
     - event: done     — stream complete with metadata

    The LLM runs in a background task that always completes and persists
    its result, even if the client disconnects mid-stream.
    """
    # ── Pre-stream: load agent state from DB ─────────────────────
    agent_query = select(Agent).where(Agent.id == agent_id)
    agent_result = await db.execute(agent_query)
    agent = agent_result.scalar_one_or_none()
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")

    conv_result = await db.execute(
        select(BuilderConversation)
        .where(BuilderConversation.agent_id == agent_id)
        .options(selectinload(BuilderConversation.messages))
        .order_by(BuilderConversation.created_at.desc())
    )
    conversation = conv_result.scalars().first()
    if not conversation:
        conversation = BuilderConversation(agent_id=agent_id)
        db.add(conversation)
        await db.flush()

    current_config: dict[str, Any] | None = None
    latest_version = 0
    version_result = await db.execute(
        select(AgentVersion)
        .where(AgentVersion.agent_id == agent_id)
        .order_by(AgentVersion.version.desc())
    )
    latest = version_result.scalars().first()
    if latest:
        current_config = latest.config_json
        latest_version = latest.version

    _persisted_context = conversation.persisted_context

    # Save user message and commit immediately — this survives disconnection
    # Build parts: attachment parts (if any) + text part
    user_parts: list[dict[str, Any]] = []
    if body.attachments:
        for att in body.attachments:
            user_parts.append(
                {
                    "kind": "attachment",
                    "file_id": att.file_id,
                    "filename": att.filename,
                    "total_lines": att.total_lines,
                }
            )
    user_parts.append({"kind": "text", "content": body.content})

    user_msg = BuilderMessage(
        conversation_id=conversation.id,
        role="user",
        parts=user_parts,
    )
    db.add(user_msg)
    await db.commit()

    # Build the LLM prompt: prepend file context so the LLM knows file_ids
    llm_content = body.content
    if body.attachments:
        file_lines = [
            f"[Uploaded file: {att.filename} (file_id: {att.file_id}, "
            f"{att.total_lines} lines). "
            f'Use read_artifact("{att.file_id}") to read it.]'
            for att in body.attachments
        ]
        llm_content = "\n".join(file_lines) + "\n\n" + body.content

    # ── Extract previous mermaid diagram for continuity ──────────
    # Prefer the mermaid from the message that matches the current config version;
    # fall back to the most recent message with any mermaid diagram.
    _previous_mermaid: str | None = None
    _fallback_mermaid: str | None = None
    _latest_version_id = latest.id if latest else None
    for m in reversed(conversation.messages):
        if m.role == "assistant" and isinstance(m.metadata_json, dict):
            diagram = m.metadata_json.get("mermaid_diagram")
            if not diagram:
                continue
            if _fallback_mermaid is None:
                _fallback_mermaid = diagram
            if _latest_version_id and m.agent_version_id == _latest_version_id:
                _previous_mermaid = diagram
                break
    if _previous_mermaid is None:
        _previous_mermaid = _fallback_mermaid

    # ── Capture IDs for the background task (avoid referencing ORM objects
    # after the request session closes) ───────────────────────────
    _agent_id = agent_id
    _agent_name = agent.name
    _conv_id = conversation.id

    # ── Background task: run LLM and persist result ──────────────
    # Uses asyncio.Queue to pass items to the SSE generator.
    # The task always runs to completion and persists, even if the
    # client disconnects and the generator is cancelled.

    queue: asyncio.Queue[dict[str, Any] | _BuildComplete | None] = asyncio.Queue()

    async def _build_and_persist() -> None:
        """Run the LLM stream and persist the result in its own DB session."""
        final_result: BuilderResult | None = None
        accumulated_parts: list[dict[str, Any]] = []
        try:
            async for item in builder_service.process_message_stream(
                user_message=llm_content,
                current_config=current_config,
                agent_name=_agent_name,
                agent_id=str(_agent_id),
                current_version=latest_version or None,
                persisted_context=_persisted_context,
                previous_mermaid=_previous_mermaid,
            ):
                if isinstance(item, dict):
                    # Accumulate parts for DB persistence (mirrors frontend logic)
                    kind = item.get("kind")
                    if kind == "part_start" and item.get("part_kind"):
                        accumulated_parts.append(
                            {
                                "kind": item["part_kind"],
                                "content": "",
                                "tool_name": item.get("tool_name"),
                                "args": item.get("args"),
                            }
                        )
                    elif kind == "part_delta" and accumulated_parts:
                        accumulated_parts[-1]["content"] += item.get("content", "")
                    elif kind == "tool_return":
                        accumulated_parts.append(
                            {
                                "kind": "tool-return",
                                "content": item.get("content", ""),
                                "tool_name": item.get("tool_name"),
                            }
                        )
                    await queue.put(item)
                elif isinstance(item, BuilderResult):
                    final_result = item
        except Exception:
            logger.exception("Builder stream task error")
            # Stream the error as a text part so the frontend can display it
            error_text = (
                "I encountered an issue processing your request. "
                "Please make sure your LLM provider is configured and running. "
                "You can check provider settings at /settings."
            )
            await queue.put({"kind": "part_start", "part_kind": "text"})
            await queue.put(
                {"kind": "part_delta", "part_kind": "text", "content": error_text}
            )
            accumulated_parts.append({"kind": "text", "content": error_text})
            final_result = BuilderResult(
                config=None,
                change_summary=None,
            )

        # ── Always persist, even if client disconnected ──────────
        if final_result is None:
            final_result = BuilderResult(config=None, change_summary=None)

        new_version_num: int | None = None
        try:
            async with async_session() as session:
                new_version_id = None

                # Query actual latest version to avoid race conditions
                ver_result = await session.execute(
                    select(AgentVersion.version)
                    .where(AgentVersion.agent_id == _agent_id)
                    .order_by(AgentVersion.version.desc())
                    .limit(1)
                )
                actual_latest = ver_result.scalar_one_or_none() or 0

                # Compare-and-set: reject edit-path writes against stale base
                if (
                    final_result.config
                    and final_result.used_edit_path
                    and final_result.base_version is not None
                    and actual_latest != final_result.base_version
                ):
                    logger.warning(
                        "Edit rejected: version conflict (base={}, actual={})",
                        final_result.base_version,
                        actual_latest,
                    )
                    final_result.config = None
                    final_result.action_cards = []
                    final_result.mermaid_diagram = None
                    final_result.change_summary = (
                        "Config was modified by another session. "
                        "Please retry your change."
                    )

                if final_result.config:
                    new_version_num = actual_latest + 1

                    # Inject agent-wide defaults (not generated by LLM)
                    config = final_result.config
                    config.setdefault("language", "en")
                    config.setdefault("timezone", "")

                    # Use voice_id from default TTS provider settings
                    if "voice_id" not in config or not config["voice_id"]:
                        tts_row = (
                            await session.execute(
                                select(ProviderConfig.config_json).where(
                                    ProviderConfig.provider_type == "tts",
                                    ProviderConfig.is_default.is_(True),
                                )
                            )
                        ).scalar_one_or_none()
                        config["voice_id"] = (
                            tts_row.get("voice_id", "") if tts_row else ""
                        )

                    agent_version = AgentVersion(
                        agent_id=_agent_id,
                        version=new_version_num,
                        config_json=config,
                        change_summary=final_result.change_summary,
                    )
                    session.add(agent_version)
                    await session.flush()
                    new_version_id = agent_version.id

                    # Update active version
                    await session.execute(
                        update(Agent)
                        .where(Agent.id == _agent_id)
                        .values(active_version=new_version_num)
                    )

                # Build metadata for action_cards and mermaid
                metadata: dict[str, Any] = {}
                if final_result.action_cards:
                    metadata["action_cards"] = [
                        c.model_dump() for c in final_result.action_cards
                    ]
                if final_result.mermaid_diagram:
                    metadata["mermaid_diagram"] = final_result.mermaid_diagram
                assistant_msg = BuilderMessage(
                    conversation_id=_conv_id,
                    role="assistant",
                    parts=accumulated_parts or None,
                    agent_version_id=new_version_id,
                    metadata_json=metadata or None,
                )
                session.add(assistant_msg)

                # Persist native message history for next turn (skip on error
                # to avoid wiping existing history)
                if final_result.message_history is not None:
                    await session.execute(
                        update(BuilderConversation)
                        .where(BuilderConversation.id == _conv_id)
                        .values(persisted_context=final_result.message_history)
                    )

                await session.commit()
        except Exception:
            logger.exception("Failed to persist builder result")

        # Signal the SSE generator
        await queue.put(
            _BuildComplete(result=final_result, version_num=new_version_num)
        )
        await queue.put(None)  # sentinel

    _build_task = asyncio.create_task(_build_and_persist())
    _pending_tasks.add(_build_task)
    _build_task.add_done_callback(_pending_tasks.discard)

    # ── SSE generator: pure consumer, safe to cancel ─────────────
    async def event_stream() -> AsyncIterator[str]:
        try:
            while True:
                item = await queue.get()
                if item is None:
                    break
                if isinstance(item, dict):
                    if item.get("kind") == "mermaid_start":
                        yield "event: mermaid_start\ndata: {}\n\n"
                    else:
                        yield f"event: part\ndata: {json.dumps(item)}\n\n"
                elif isinstance(item, _BuildComplete):
                    br = item.result
                    if br.config:
                        yield f"event: config\ndata: {json.dumps(br.config)}\n\n"
                    if br.action_cards:
                        yield f"event: action_cards\ndata: {json.dumps([c.model_dump() for c in br.action_cards])}\n\n"
                    if br.mermaid_diagram:
                        yield f"event: mermaid\ndata: {json.dumps(br.mermaid_diagram)}\n\n"
                    yield f"event: done\ndata: {json.dumps({'version': item.version_num, 'change_summary': br.change_summary})}\n\n"
        except asyncio.CancelledError:
            # Client disconnected — task continues in background
            logger.debug("SSE client disconnected, build task continues")
        except GeneratorExit:
            logger.debug("SSE generator closed, build task continues")

    return StreamingResponse(
        event_stream(),
        media_type="text/event-stream",
        headers=_SSE_HEADERS,
    )
