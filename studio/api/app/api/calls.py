"""Call log API routes."""

import uuid
from pathlib import Path
from urllib.parse import urlparse

from fastapi import APIRouter, Depends, HTTPException, Query
from fastapi.responses import FileResponse
from sqlalchemy import func as sa_func
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.api.recordings import resolve_recording_http_url
from app.lib.auth import TenantContext, require_tenant
from app.lib.database import get_db
from app.models.agent import Agent
from app.models.call import Call
from app.models.call_event import CallEvent
from app.schemas.call import (
    CallEventListResponse,
    CallEventResponse,
    CallExternalLink,
    CallListResponse,
    CallLogCapabilitiesResponse,
    CallResponse,
)

router = APIRouter(prefix="/calls", tags=["calls"])


@router.get("", response_model=CallListResponse)
async def list_calls(
    agent_id: uuid.UUID | None = None,
    agent_ids: list[uuid.UUID] | None = Query(default=None),
    skip: int = 0,
    limit: int = 50,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CallListResponse:
    """List calls with optional agent filter."""
    query = select(Call, Agent.name).outerjoin(Agent, Agent.id == Call.agent_id)
    count_query = select(sa_func.count(Call.id)).outerjoin(Agent, Agent.id == Call.agent_id)

    effective_agent_ids: list[uuid.UUID] = []
    if agent_ids:
        effective_agent_ids.extend(agent_ids)
    if agent_id and agent_id not in effective_agent_ids:
        effective_agent_ids.append(agent_id)
    if effective_agent_ids:
        query = query.where(Call.agent_id.in_(effective_agent_ids))
        count_query = count_query.where(Call.agent_id.in_(effective_agent_ids))

    count_result = await db.execute(count_query)
    total = count_result.scalar_one()

    result = await db.execute(
        query.order_by(Call.created_at.desc()).offset(skip).limit(limit)
    )
    rows = result.all()

    return CallListResponse(
        calls=[
            CallResponse(
                id=call.id,
                agent_id=call.agent_id,
                agent_name=agent_name,
                direction=call.direction,
                caller_number=call.caller_number,
                callee_number=call.callee_number,
                status=call.status,
                duration_seconds=call.duration_seconds,
                transcript_json=call.transcript_json,
                recording_url=resolve_recording_http_url(call.id, call.recording_url),
                variables_json=call.variables_json,
                outcome=call.outcome,
                sentiment_score=call.sentiment_score,
                agent_version_used=call.agent_version_used,
                started_at=call.started_at,
                ended_at=call.ended_at,
                created_at=call.created_at,
            )
            for call, agent_name in rows
        ],
        total=total,
    )


@router.get("/{call_id}", response_model=CallResponse)
async def get_call(
    call_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CallResponse:
    """Get a single call with full transcript."""
    query = (
        select(Call, Agent.name)
        .outerjoin(Agent, Agent.id == Call.agent_id)
        .where(Call.id == call_id)
    )

    result = await db.execute(query)
    row = result.one_or_none()
    if not row:
        raise HTTPException(status_code=404, detail="Call not found")
    call, agent_name = row

    return CallResponse(
        id=call.id,
        agent_id=call.agent_id,
        agent_name=agent_name,
        direction=call.direction,
        caller_number=call.caller_number,
        callee_number=call.callee_number,
        status=call.status,
        duration_seconds=call.duration_seconds,
        transcript_json=call.transcript_json,
        recording_url=resolve_recording_http_url(call.id, call.recording_url),
        variables_json=call.variables_json,
        outcome=call.outcome,
        sentiment_score=call.sentiment_score,
        agent_version_used=call.agent_version_used,
        started_at=call.started_at,
        ended_at=call.ended_at,
        created_at=call.created_at,
    )


@router.get("/{call_id}/events", response_model=CallEventListResponse)
async def get_call_events(
    call_id: uuid.UUID,
    skip: int = 0,
    limit: int = 200,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CallEventListResponse:
    """Get paginated structured call events."""
    query = (
        select(Call.id).outerjoin(Agent, Agent.id == Call.agent_id).where(Call.id == call_id)
    )
    call_result = await db.execute(query)
    if call_result.scalar_one_or_none() is None:
        raise HTTPException(status_code=404, detail="Call not found")

    total_result = await db.execute(
        select(sa_func.count(CallEvent.id)).where(CallEvent.call_id == call_id)
    )
    total = int(total_result.scalar_one())

    result = await db.execute(
        select(CallEvent)
        .where(CallEvent.call_id == call_id)
        .order_by(CallEvent.seq.asc())
        .offset(skip)
        .limit(limit)
    )
    events = result.scalars().all()

    return CallEventListResponse(
        events=[CallEventResponse.model_validate(e) for e in events],
        total=total,
        skip=skip,
        limit=limit,
    )


@router.get("/{call_id}/log-capabilities", response_model=CallLogCapabilitiesResponse)
async def get_call_log_capabilities(
    call_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> CallLogCapabilitiesResponse:
    """Get log display capabilities for call detail tabs."""
    query = (
        select(Call).outerjoin(Agent, Agent.id == Call.agent_id).where(Call.id == call_id)
    )
    result = await db.execute(query)
    call = result.scalar_one_or_none()
    if not call:
        raise HTTPException(status_code=404, detail="Call not found")

    total_events_result = await db.execute(
        select(sa_func.count(CallEvent.id)).where(CallEvent.call_id == call_id)
    )
    total_events = int(total_events_result.scalar_one())
    has_internal_logs = total_events > 0

    active_adapters: list[str] = []
    external_links: list[CallExternalLink] = []

    variables = call.variables_json if isinstance(call.variables_json, dict) else {}
    obs = variables.get("observability") if isinstance(variables, dict) else None
    if isinstance(obs, dict):
        raw_adapters = obs.get("active_adapters")
        if isinstance(raw_adapters, list):
            active_adapters = [str(v) for v in raw_adapters if isinstance(v, str)]
        raw_links = obs.get("external_links")
        if isinstance(raw_links, list):
            for item in raw_links:
                if not isinstance(item, dict):
                    continue
                adapter = item.get("adapter")
                label = item.get("label")
                url = item.get("url")
                if (
                    isinstance(adapter, str)
                    and isinstance(label, str)
                    and isinstance(url, str)
                    and url
                ):
                    external_links.append(
                        CallExternalLink(
                            adapter=adapter,
                            label=label,
                            url=url,
                        )
                    )

    if has_internal_logs and "db" not in active_adapters:
        active_adapters.append("db")

    return CallLogCapabilitiesResponse(
        has_internal_logs=has_internal_logs,
        active_adapters=active_adapters,
        external_links=external_links,
    )


_RECORDING_MEDIA_TYPES: dict[str, str] = {
    "opus": "audio/ogg; codecs=opus",
    "wav": "audio/wav",
}


@router.get("/{call_id}/recording", include_in_schema=False)
async def proxy_recording(
    call_id: uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> FileResponse:
    """Stream a session recording that lives on the local filesystem.

    Looks up the call, reads the ``recording_url`` URI from the database,
    and streams the file if it is a ``file://`` URI.  The path is taken
    entirely from the database — no user-supplied path components are used.

    For ``s3://`` URIs the caller should use the pre-signed URL returned by
    ``resolve_recording_http_url`` instead of hitting this endpoint.
    """
    query = (
        select(Call).outerjoin(Agent, Agent.id == Call.agent_id).where(Call.id == call_id)
    )
    result = await db.execute(query)
    call = result.scalar_one_or_none()
    if call is None:
        raise HTTPException(status_code=404, detail="Call not found")

    uri: str | None = call.recording_url
    if not uri:
        raise HTTPException(status_code=404, detail="No recording for this call")

    parsed = urlparse(uri)
    if parsed.scheme != "file":
        raise HTTPException(
            status_code=400,
            detail="Recording is not stored locally; use the resolved URL directly",
        )

    # parsed.path for file:///abs/path → /abs/path
    file_path = Path(parsed.path)
    if not file_path.is_absolute():
        raise HTTPException(
            status_code=500, detail="Stored recording path is not absolute"
        )

    if not file_path.is_file():
        raise HTTPException(status_code=404, detail="Recording file not found on disk")

    ext = file_path.suffix.lstrip(".").lower()
    media_type = _RECORDING_MEDIA_TYPES.get(ext, "application/octet-stream")

    return FileResponse(
        path=file_path,
        media_type=media_type,
        filename=file_path.name,
    )
