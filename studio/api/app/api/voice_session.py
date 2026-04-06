"""Voice session utilities.

Voice sessions (WebRTC/WebSocket audio) are handled by the standalone
voice-server binary. Python does not start or manage that process.

Python owns:
  - Text test WebSocket (/api/voice/text-test/{agent_id})
  - AgentRunner for evaluations (headless, no audio)
"""

from __future__ import annotations

import asyncio
import json
import uuid
from time import perf_counter
from typing import Any

from fastapi import APIRouter, Depends, WebSocket, WebSocketDisconnect
from loguru import logger
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

import integrations
from app.lib.config import get_llm_config, get_settings
from app.lib.database import get_db
from app.lib.tool_activity import tool_activity_from_runner_event
from app.models.agent import Agent, AgentVersion
from voice_engine import AgentRunner

router = APIRouter(tags=["voice"])


TURN_TIMEOUT_SECS = 45.0


def _remaining_turn_timeout(deadline: float) -> float:
    remaining = deadline - perf_counter()
    if remaining <= 0:
        raise TimeoutError
    return remaining


def _parse_runner_event(raw_event: Any) -> dict[str, Any] | None:
    return raw_event if isinstance(raw_event, dict) else None


def _normalize_tool_call_id(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _track_pending_tool(
    pending_tools: list[tuple[str | None, str]],
    tool_call_id: Any,
    tool_name: Any,
) -> None:
    name = str(tool_name or "")
    if not name:
        return
    pending_tools.append((_normalize_tool_call_id(tool_call_id), name))


def _pending_tool_name_match_indices(
    pending_tools: list[tuple[str | None, str]],
    tool_name: str,
) -> list[int]:
    if not tool_name:
        return []
    return [
        index
        for index, (_pending_id, pending_name) in enumerate(pending_tools)
        if pending_name == tool_name
    ]


def _resolve_pending_tool(
    pending_tools: list[tuple[str | None, str]],
    tool_call_id: Any,
    tool_name: Any,
) -> None:
    normalized_id = _normalize_tool_call_id(tool_call_id)
    normalized_name = str(tool_name or "")
    name_match_indices = _pending_tool_name_match_indices(
        pending_tools,
        normalized_name,
    )

    if normalized_id is not None:
        for index, (pending_id, _pending_name) in enumerate(pending_tools):
            if pending_id == normalized_id:
                pending_tools.pop(index)
                return

    if len(name_match_indices) == 1:
        pending_tools.pop(name_match_indices[0])
        if normalized_id is not None:
            logger.warning(
                "Text test: resolved tool completion by name after ID mismatch "
                "(id={}, tool={})",
                normalized_id,
                normalized_name,
            )
        return

    if len(name_match_indices) > 1:
        logger.warning(
            "Text test: ambiguous tool completion match (id={}, tool={}, "
            "pending_matches={})",
            normalized_id,
            normalized_name,
            len(name_match_indices),
        )


def _orphaned_tool_activity(tool_call_id: str | None, tool_name: str) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "type": "tool_activity",
        "tool_name": tool_name,
        "status": "orphaned",
    }
    if tool_call_id is not None:
        payload["tool_call_id"] = tool_call_id
    return payload


def _stream_runner_events(
    runner: AgentRunner,
    text: str,
    loop: asyncio.AbstractEventLoop,
    event_queue: asyncio.Queue[Any | None],
) -> None:
    try:
        runner.start_turn(text)
        while True:
            raw_event = runner.recv_event()
            loop.call_soon_threadsafe(event_queue.put_nowait, raw_event)
            if raw_event is None:
                break
    except Exception as exc:
        loop.call_soon_threadsafe(
            event_queue.put_nowait,
            {"type": "__runner_exception__", "message": str(exc)},
        )
        loop.call_soon_threadsafe(event_queue.put_nowait, None)


@router.websocket("/voice/text-test/{agent_id}")
async def text_test_socket(
    websocket: WebSocket,
    agent_id: str,
    db: AsyncSession = Depends(get_db),
) -> None:
    await websocket.accept()

    llm_cfg = await get_llm_config(db, "__voice__")
    system_prompt = (
        "You are a helpful voice assistant. Keep responses concise and conversational."
    )
    greeting = "Hello! How can I help you today?"
    graph_json: str | None = None

    try:
        agent_uuid = uuid.UUID(agent_id)
        result = await db.execute(select(Agent).where(Agent.id == agent_uuid))
        agent = result.scalar_one_or_none()
        if agent and agent.active_version is not None:
            version_res = await db.execute(
                select(AgentVersion).where(
                    AgentVersion.agent_id == agent_uuid,
                    AgentVersion.version == agent.active_version,
                )
            )
            active_version = version_res.scalar_one_or_none()
            if active_version:
                cfg = active_version.config_json
                if cfg.get("config_schema_version") == "v3_graph":
                    entry_id = cfg.get("entry")
                    if entry_id:
                        entry_node = cfg.get("nodes", {}).get(entry_id, {})
                        system_prompt = entry_node.get("system_prompt", system_prompt)
                        greeting = entry_node.get("greeting", greeting)
                    graph_json = json.dumps(cfg)
                else:
                    system_prompt = cfg.get("system_prompt", system_prompt)
                    greeting = cfg.get("greeting", greeting)
    except Exception:
        logger.warning("Text test: failed to resolve agent config for {}", agent_id)

    # Resolve secrets using the canonical Rust secret resolver so voice test
    # matches production secret precedence and field shaping.
    secrets: dict[str, str] = {}
    try:
        raw_url = str(get_settings().database.url)
        db_url = raw_url.replace("+asyncpg", "", 1)
        secrets = integrations.resolve_agent_secrets(
            db_url=db_url,
            secret_key=get_settings().auth.secret_key,
            agent_id=agent_id,
        )
    except Exception as exc:
        logger.warning(
            "Text test: failed to resolve secrets for {} — {}", agent_id, exc
        )

    def _build_runner() -> AgentRunner:
        return AgentRunner(
            llm_url=llm_cfg.base_url,
            llm_api_key=llm_cfg.api_key,
            llm_model=llm_cfg.model,
            system_prompt=system_prompt,
            llm_provider=llm_cfg.provider,
            graph_json=graph_json,
            greeting=greeting,
            temperature=float(getattr(llm_cfg, "temperature", 0.7)),
            max_tokens=int(getattr(llm_cfg, "max_tokens", 512)),
            secrets=secrets or None,
        )

    runner = _build_runner()
    cancel_handle = runner.cancel_handle()

    await websocket.send_json(
        {
            "type": "ready",
            "greeting": greeting,
        }
    )

    try:
        while True:
            raw = await websocket.receive_text()
            try:
                msg = json.loads(raw)
            except Exception:
                continue

            if msg.get("type") != "text":
                continue
            text = str(msg.get("text", "")).strip()
            if not text:
                continue

            await websocket.send_json(
                {"type": "transcript", "role": "user", "text": text}
            )
            await websocket.send_json({"type": "processing"})

            t0 = perf_counter()
            deadline = t0 + TURN_TIMEOUT_SECS
            assembled_text = ""
            finished_text: str | None = None
            runtime_error: str | None = None
            ttft_ms: int | None = None
            pending_tools: list[tuple[str | None, str]] = []
            saw_hang_up = False
            event_queue: asyncio.Queue[Any | None] = asyncio.Queue()
            loop = asyncio.get_running_loop()
            runner_task = asyncio.create_task(
                asyncio.to_thread(
                    _stream_runner_events,
                    runner,
                    text,
                    loop,
                    event_queue,
                )
            )
            timed_out = False

            try:
                while True:
                    raw_event = await asyncio.wait_for(
                        event_queue.get(),
                        timeout=_remaining_turn_timeout(deadline),
                    )
                    if raw_event is None:
                        break

                    event = _parse_runner_event(raw_event)
                    if event is None:
                        continue

                    event_type = str(event.get("type", ""))
                    if event_type == "__runner_exception__":
                        runtime_error = str(event.get("message", "agent error"))
                    elif event_type == "token":
                        assembled_text += str(event.get("text", ""))
                    elif event_type == "tool_call_started":
                        tool_call_id = event.get("id")
                        tool_name = str(event.get("name", ""))
                        _track_pending_tool(pending_tools, tool_call_id, tool_name)
                        await websocket.send_json(
                            {
                                "type": "tool_activity",
                                "tool_call_id": tool_call_id,
                                "tool_name": tool_name,
                                "status": "executing",
                            }
                        )
                    elif event_type == "tool_call_completed":
                        _resolve_pending_tool(
                            pending_tools,
                            event.get("id"),
                            event.get("name"),
                        )
                        await websocket.send_json(
                            tool_activity_from_runner_event(event)
                        )
                    elif event_type == "finished":
                        text_value = event.get("text")
                        if isinstance(text_value, str):
                            finished_text = text_value
                    elif event_type == "hang_up":
                        saw_hang_up = True
                        text_value = event.get("content")
                        if isinstance(text_value, str) and text_value.strip():
                            finished_text = text_value
                    elif event_type == "error":
                        runtime_error = str(event.get("text", "agent error"))
                    elif event_type == "llm_complete":
                        raw_ttfb = event.get("ttfb_ms")
                        if ttft_ms is None and isinstance(raw_ttfb, (int, float)):
                            ttft_ms = int(raw_ttfb)
            except TimeoutError:
                timed_out = True
                await websocket.send_json(
                    {"type": "error", "message": "Timed out waiting for agent reply."}
                )
                await websocket.send_json({"type": "turn_complete"})
                continue  # finally runs before continue — runner is drained/rebuilt
            except Exception as exc:
                await websocket.send_json({"type": "error", "message": str(exc)})
                await websocket.send_json({"type": "turn_complete"})
                continue
            finally:
                if timed_out:
                    # Signal the Rust backend to abort the in-flight turn.
                    # This wakes recv_event() via tokio::select! and calls
                    # backend.cancel(), dropping the LLM stream and tool
                    # channels so the background thread exits promptly.
                    cancel_handle.cancel()
                    try:
                        await asyncio.wait_for(runner_task, timeout=5.0)
                    except (TimeoutError, Exception):
                        logger.warning("Text test: runner thread stuck after cancel")
                    # Rebuild: the timed-out turn wrote partial state into
                    # the conversation history that the user never saw.
                    runner = _build_runner()
                    cancel_handle = runner.cancel_handle()
                else:
                    await runner_task

            for tool_call_id, tool_name in pending_tools:
                logger.warning(
                    "Text test: orphaned tool after turn end "
                    "(id={} name={} saw_hang_up={} timed_out={})",
                    tool_call_id,
                    tool_name,
                    saw_hang_up,
                    timed_out,
                )
                await websocket.send_json(
                    _orphaned_tool_activity(tool_call_id, tool_name)
                )

            assistant_text = (finished_text or assembled_text).strip() or None
            turn_ms = int((perf_counter() - t0) * 1000)
            if assistant_text:
                token_est = max(1, len(assistant_text.split()))
                tps = round(token_est / max(turn_ms / 1000, 0.001), 2)
                await websocket.send_json(
                    {"type": "transcript", "role": "assistant", "text": assistant_text}
                )
                await websocket.send_json(
                    {
                        "type": "metrics",
                        "ttft_ms": ttft_ms,
                        "first_sentence_ms": None,
                        "total_turn_ms": turn_ms,
                        "tokens_per_second": tps,
                    }
                )

            if runtime_error:
                await websocket.send_json({"type": "error", "message": runtime_error})
            elif not assistant_text and not saw_hang_up:
                await websocket.send_json(
                    {"type": "error", "message": "Agent produced no text response."}
                )

            await websocket.send_json({"type": "turn_complete"})
    except WebSocketDisconnect:
        return
