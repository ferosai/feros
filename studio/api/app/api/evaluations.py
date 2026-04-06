"""Evaluation API routes (Phase 2).

User-facing endpoints for Auto Test configuration and run management.
"""

from __future__ import annotations

import asyncio
import json
import os
import uuid
from datetime import UTC, datetime, timedelta
from typing import Any, cast

from fastapi import APIRouter, Depends, Header, HTTPException, Query
from fastapi.responses import StreamingResponse
from loguru import logger
from pydantic_ai import Agent as PydanticAgent
from pydantic_ai.exceptions import UserError as PydanticAIUserError
from sqlalchemy import Select, and_, delete, func, select
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from app.agent_builder.service import builder_service
from app.lib.config import get_llm_config
from app.lib.database import async_session, get_db
from app.lib.llm_factory import build_model
from app.models.agent import Agent as AgentModel
from app.models.agent import AgentVersion
from app.models.evaluation import (
    EvaluationConfig,
    EvaluationConfigVersion,
    EvaluationJudgment,
    EvaluationRun,
    EvaluationRunEvent,
)
from app.models.evaluation import EvaluationRunStatus as RunStatusModel
from app.schemas.evaluation import (
    AssistantReplyEvent,
    CallerUtteranceEvent,
    EvaluationConfigCreate,
    EvaluationConfigDetailResponse,
    EvaluationConfigListResponse,
    EvaluationConfigPayload,
    EvaluationConfigResponse,
    EvaluationConfigStatus,
    EvaluationConfigVersionCreate,
    EvaluationConfigVersionResponse,
    EvaluationJudgeRequest,
    EvaluationRunCreate,
    EvaluationRunDetailResponse,
    EvaluationRunEventType,
    EvaluationRunListResponse,
    EvaluationRunRerunRequest,
    EvaluationRunsDeleteResponse,
    EvaluationRunStatus,
    EvaluationRunSummary,
    RubricPresetListResponse,
    RunFailedEvent,
    RunFinishedEvent,
    ScenarioProfile,
    ToolCallEvent,
    ToolMockOutcome,
    ToolMockResultEvent,
    ToolSandboxResolutionInput,
    TurnStartedEvent,
)
from app.services.evaluations import (
    BuilderLLMEvaluationJudge,
    EvaluationConfigService,
    EvaluationRunEventRecorder,
    EvaluationRunService,
    evaluate_hard_checks,
    evaluation_worker,
    list_rubric_presets,
    rubric_to_response,
    store_judgment_result,
)
from app.services.evaluations.sandbox import SeededDeterministicSandboxResolver
from voice_engine import AgentRunner

router = APIRouter(prefix="/agents/{agent_id}/evaluations", tags=["evaluations"])

_config_service = EvaluationConfigService()
_run_service = EvaluationRunService()
_judge = BuilderLLMEvaluationJudge()
_sandbox_resolver = SeededDeterministicSandboxResolver()

# In-process idempotency safeguards (best-effort for single process).
_idempotency_store: dict[str, uuid.UUID] = {}


@router.get("/rubrics", response_model=RubricPresetListResponse)
async def list_rubrics(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> RubricPresetListResponse:
    await _require_agent(db, agent_id)
    return RubricPresetListResponse(
        rubrics=[rubric_to_response(rubric) for rubric in list_rubric_presets()]
    )


def _cfg_to_response(cfg: EvaluationConfig) -> EvaluationConfigResponse:
    return EvaluationConfigResponse(
        id=cfg.id,
        agent_id=cfg.agent_id,
        name=cfg.name,
        status=EvaluationConfigStatus(cfg.status),
        latest_version=cfg.latest_version,
        created_at=cfg.created_at,
        updated_at=cfg.updated_at,
    )


def _cfg_version_to_response(
    v: EvaluationConfigVersion,
) -> EvaluationConfigVersionResponse:
    return EvaluationConfigVersionResponse(
        id=v.id,
        config_id=v.config_id,
        version=v.version,
        config=EvaluationConfigPayload.model_validate(v.config_json),
        created_at=v.created_at,
    )


def _run_to_summary(run: EvaluationRun) -> EvaluationRunSummary:
    return EvaluationRunSummary(
        id=run.id,
        agent_id=run.agent_id,
        config_id=run.config_id,
        config_version=run.config_version.version if run.config_version else 0,
        target_agent_version=run.target_agent_version,
        status=EvaluationRunStatus(run.status),
        aggregate_score=run.aggregate_score,
        started_at=run.started_at,
        ended_at=run.ended_at,
        created_at=run.created_at,
    )


async def _require_agent(db: AsyncSession, agent_id: uuid.UUID) -> AgentModel:
    agent = await db.scalar(select(AgentModel).where(AgentModel.id == agent_id))
    if not agent:
        raise HTTPException(status_code=404, detail="Agent not found")
    return agent


def _idempotency_key(
    *,
    op: str,
    agent_id: uuid.UUID,
    config_id: uuid.UUID | None = None,
    run_id: uuid.UUID | None = None,
    key: str | None = None,
) -> str | None:
    if not key:
        return None
    return f"{op}:{agent_id}:{config_id}:{run_id}:{key}"


def _extract_target_prompt_and_graph(
    version: AgentVersion | None,
) -> tuple[str, str | None]:
    default_prompt = "You are a helpful voice agent. Keep replies concise, factual, and conversational."
    if not version:
        return default_prompt, None

    config = version.config_json
    # Validation
    if not config:
        return default_prompt, None

    if config.get("config_schema_version") == "v3_graph":
        entry = config.get("entry")
        nodes = config.get("nodes", {})
        if isinstance(nodes, dict) and isinstance(entry, str):
            entry_node = nodes.get(entry, {})
            if isinstance(entry_node, dict):
                prompt = str(entry_node.get("system_prompt", default_prompt))
                return prompt, json.dumps(config, ensure_ascii=True)
        return default_prompt, json.dumps(config, ensure_ascii=True)

    prompt = str(config.get("system_prompt", default_prompt))
    return prompt, None


def _safe_json_object(raw: str | None) -> dict[str, Any]:
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except Exception:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _build_tool_stub(
    outcome: ToolMockOutcome,
    *,
    body_json: dict[str, Any] | None,
    error_message: str | None,
) -> str:
    if outcome in {
        ToolMockOutcome.SUCCESS,
        ToolMockOutcome.EMPTY,
        ToolMockOutcome.PARTIAL,
    }:
        return json.dumps(body_json or {}, ensure_ascii=True)
    if outcome == ToolMockOutcome.MALFORMED:
        return "{malformed-response"
    return f"Error: {error_message or outcome.value}"


def _is_transient_transport_error(message: str | None) -> bool:
    if not message:
        return False
    text = message.lower()
    markers = (
        "transport: error sending request",
        "connection reset",
        "connection refused",
        "timed out",
        "temporary failure",
    )
    return any(marker in text for marker in markers)


async def _generate_test_agent_utterance(
    *,
    turn_id: int,
    payload: dict[str, Any],
    conversation: list[dict[str, Any]],
) -> str | None:
    goals = payload.get("goals", [])
    if turn_id == 1:
        if goals and isinstance(goals, list):
            first = goals[0]
            if isinstance(first, dict) and first.get("title"):
                return f"Hi, I need help with: {first['title']}."
        return "Hi, I need help with my request."

    llm_cfg = builder_service.current_llm_config()
    model, model_settings = build_model(llm_cfg)
    try:
        system_prompt = (
            "You are a simulated caller for automated voice-agent evaluation. "
            "Return ONLY the next caller utterance as plain text. "
            "If the conversation should stop, return exactly: [END_TEST]."
        )
        user_payload = {
            "turn": turn_id,
            "persona_preset": payload.get("persona_preset", "cooperative"),
            "persona_instructions": payload.get("persona_instructions"),
            "scenario_profile": payload.get("scenario_profile", "balanced"),
            "goals": goals,
            "conversation": conversation[-20:],
        }
        caller_agent: PydanticAgent[None, str] = PydanticAgent(
            model=model,
            output_type=str,
            system_prompt=system_prompt,
        )
        run = await caller_agent.run(
            json.dumps(user_payload, ensure_ascii=True),
            model_settings=model_settings,
        )
        raw = run.output
        text = (raw or "").strip()
        if not text:
            return None
        if text.upper() == "[END_TEST]":
            return None
        return text
    except Exception:
        logger.exception(
            "Test-agent utterance generation failed (turn={}, persona={}, scenario={})",
            turn_id,
            payload.get("persona_preset", "cooperative"),
            payload.get("scenario_profile", "balanced"),
        )
        # Keep runs resilient even when LLM generation is unavailable.
        if turn_id >= int(payload.get("max_turns", 12)):
            return None
        return "Can you continue and help me complete this request?"


def _collect_target_runner_result(
    target_runner: AgentRunner,
    caller_text: str,
) -> tuple[str, str | None, str | None]:
    """Run a single target-agent turn in a worker thread and normalize events."""
    raw_events = target_runner.send(caller_text)
    assembled_text = ""
    finished_text: str | None = None
    runtime_error: str | None = None

    for raw_event in raw_events:
        if not isinstance(raw_event, dict):
            continue
        event_type = str(raw_event.get("type", ""))
        if event_type == "token":
            assembled_text += str(raw_event.get("text", ""))
        elif event_type == "finished":
            text_value = raw_event.get("text")
            if isinstance(text_value, str):
                finished_text = text_value
        elif event_type == "error":
            runtime_error = str(raw_event.get("text", "agent error"))

    return assembled_text, finished_text, runtime_error


async def _reconcile_orphaned_runs(db: AsyncSession, *, agent_id: uuid.UUID) -> None:
    """Fail stale queued/running runs with no in-process worker task.

    This prevents permanent `queued` runs after reload/restart from forcing
    endless UI polling.
    """
    now = datetime.now(UTC)
    stmt = select(EvaluationRun).where(
        EvaluationRun.agent_id == agent_id,
        EvaluationRun.status.in_([RunStatusModel.QUEUED, RunStatusModel.RUNNING]),
    )
    candidates = list((await db.scalars(stmt)).all())
    changed = False
    for run in candidates:
        if evaluation_worker.is_running(run.id):
            continue
        created_age = now - run.created_at
        started_age = (now - run.started_at) if run.started_at else None
        should_fail = (
            run.status == RunStatusModel.QUEUED and created_age > timedelta(seconds=30)
        ) or (
            run.status == RunStatusModel.RUNNING
            and (started_age is None or started_age > timedelta(minutes=10))
        )
        if not should_fail:
            continue
        run.status = RunStatusModel.FAILED
        run.summary = "Execution failed: Worker was unavailable after a reload/restart."
        run.ended_at = now
        changed = True
    if changed:
        await db.flush()
        await db.commit()


async def _run_execution_task(run_id: uuid.UUID) -> None:
    """Realistic text-based auto-e2e runner with deterministic tool hooks."""
    async with async_session() as db:
        try:
            run = await db.scalar(
                select(EvaluationRun)
                .where(EvaluationRun.id == run_id)
                .join(
                    EvaluationConfigVersion,
                    EvaluationConfigVersion.id == EvaluationRun.config_version_id,
                )
                .options(selectinload(EvaluationRun.config_version))
            )
            if not run:
                return
            if run.status != RunStatusModel.QUEUED:
                return

            await _run_service.mark_running(db, run)
            # Persist running state immediately so other sessions/UI can observe
            # progress while the subprocess executes the scenario.
            await db.commit()
            recorder = EvaluationRunEventRecorder(db, run_id)

            config_payload_raw = (
                run.config_version.config_json if run.config_version else {}
            )
            config_payload = EvaluationConfigPayload.model_validate(config_payload_raw)
            # Capture for nested functions to satisfy mypy
            execution_run = run

            version = await db.scalar(
                select(AgentVersion).where(
                    AgentVersion.agent_id == run.agent_id,
                    AgentVersion.version == run.target_agent_version,
                )
            )
            target_prompt, graph_json = _extract_target_prompt_and_graph(version)
            voice_llm = await get_llm_config(db, "__voice__")
            hook_toggle = os.getenv("EVAL_TOOL_HOOK_ENABLED", "").strip().lower()
            hook_enabled = hook_toggle not in {"0", "false", "no", "off"}

            seq_no = 0
            event_models: list[Any] = []
            conversation_for_test_agent: list[dict[str, Any]] = []
            tool_timeline: list[dict[str, Any]] = []

            current_turn = 0
            tool_call_index = 0
            hook_records: list[dict[str, Any]] = []

            def before_tool_call(tool_name: str, arguments: str) -> str:
                nonlocal tool_call_index
                tool_call_index += 1
                args_json = _safe_json_object(arguments)
                try:
                    scenario_profile = ScenarioProfile(config_payload.scenario_profile)
                except Exception:
                    scenario_profile = ScenarioProfile.BALANCED
                resolution = _sandbox_resolver.resolve(
                    ToolSandboxResolutionInput(
                        scenario_profile=scenario_profile,
                        seed=execution_run.seed,
                        turn_id=current_turn,
                        tool_call_index=tool_call_index,
                        tool_id=tool_name,
                        args_json=args_json,
                    )
                )
                tool_call_id = f"tool-{current_turn}-{tool_call_index}"
                hook_records.append(
                    {
                        "tool_call_id": tool_call_id,
                        "tool_id": tool_name,
                        "args_json": args_json,
                        "resolution": resolution,
                    }
                )
                return _build_tool_stub(
                    resolution.outcome,
                    body_json=resolution.body_json,
                    error_message=resolution.error_message,
                )

            runner_kwargs: dict[str, Any] = {
                "llm_url": voice_llm.base_url,
                "llm_api_key": voice_llm.api_key,
                "llm_model": voice_llm.model,
                "llm_provider": voice_llm.provider,
                "system_prompt": target_prompt,
                "graph_json": graph_json,
                "temperature": float(getattr(voice_llm, "temperature", 0.7)),
                "max_tokens": int(getattr(voice_llm, "max_tokens", 512)),
            }
            if hook_enabled:
                runner_kwargs["before_tool_call"] = before_tool_call
            target_runner = AgentRunner(**runner_kwargs)

            for turn in range(1, config_payload.max_turns + 1):
                await db.refresh(run, attribute_names=["status"])
                # Cast to break mypy narrowing from line 358
                if cast(RunStatusModel, run.status) == RunStatusModel.CANCELLED:
                    await db.commit()
                    return

                current_turn = turn
                tool_call_index = 0
                hook_records.clear()

                caller_text = await _generate_test_agent_utterance(
                    turn_id=turn,
                    payload=config_payload.model_dump(mode="json"),
                    conversation=conversation_for_test_agent,
                )
                if not caller_text:
                    break

                now = datetime.now(UTC)

                seq_no += 1
                turn_started = TurnStartedEvent(
                    event_type=EvaluationRunEventType.TURN_STARTED,
                    seq_no=seq_no,
                    timestamp=now,
                    turn_id=turn,
                )
                await recorder.append(
                    event_type=turn_started.event_type,
                    payload_json=turn_started.model_dump(mode="json"),
                    event_timestamp=now,
                )
                event_models.append(turn_started)

                seq_no += 1
                caller_event = CallerUtteranceEvent(
                    event_type=EvaluationRunEventType.CALLER_UTTERANCE,
                    seq_no=seq_no,
                    timestamp=now,
                    turn_id=turn,
                    text=caller_text,
                )
                await recorder.append(
                    event_type=caller_event.event_type,
                    payload_json=caller_event.model_dump(mode="json"),
                    event_timestamp=now,
                )
                event_models.append(caller_event)
                conversation_for_test_agent.append(
                    {"role": "test_agent", "text": caller_text}
                )
                # Make turn-start + caller utterance visible to live SSE listeners
                # before running the potentially long target-agent turn.
                await db.commit()

                per_turn_timeout = max(5, min(int(config_payload.timeout_seconds), 45))
                attempts = 0
                while True:
                    try:
                        (
                            assembled_text,
                            finished_text,
                            runtime_error,
                        ) = await asyncio.wait_for(
                            asyncio.to_thread(
                                _collect_target_runner_result,
                                target_runner,
                                caller_text,
                            ),
                            timeout=per_turn_timeout,
                        )
                    except TimeoutError as exc:
                        raise RuntimeError(
                            f"Target agent turn timed out after {per_turn_timeout}s"
                        ) from exc

                    if attempts < 1 and _is_transient_transport_error(runtime_error):
                        attempts += 1
                        await asyncio.sleep(1)
                        continue
                    break

                for hook in hook_records:
                    resolution = hook["resolution"]
                    tool_call_id = str(hook["tool_call_id"])
                    tool_id = str(hook["tool_id"])
                    args_json = hook["args_json"]

                    seq_no += 1
                    tool_call_event = ToolCallEvent(
                        event_type=EvaluationRunEventType.TOOL_CALL,
                        seq_no=seq_no,
                        timestamp=now,
                        turn_id=turn,
                        tool_call_id=tool_call_id,
                        tool_id=tool_id,
                        args_json=args_json,
                    )
                    await recorder.append(
                        event_type=tool_call_event.event_type,
                        payload_json=tool_call_event.model_dump(mode="json"),
                        event_timestamp=now,
                    )
                    event_models.append(tool_call_event)

                    seq_no += 1
                    tool_result_event = ToolMockResultEvent(
                        event_type=EvaluationRunEventType.TOOL_MOCK_RESULT,
                        seq_no=seq_no,
                        timestamp=now,
                        turn_id=turn,
                        tool_call_id=tool_call_id,
                        tool_id=tool_id,
                        outcome=ToolMockOutcome(resolution.outcome),
                        status_code=resolution.status_code,
                        body_json=resolution.body_json,
                        error_message=resolution.error_message,
                        decision_source=resolution.source,
                        hook_stage="before",
                    )
                    await recorder.append(
                        event_type=tool_result_event.event_type,
                        payload_json=tool_result_event.model_dump(mode="json"),
                        event_timestamp=now,
                    )
                    event_models.append(tool_result_event)
                    tool_timeline.append(tool_result_event.model_dump(mode="json"))
                    await db.commit()

                assistant_text = (finished_text or assembled_text).strip()
                if assistant_text:
                    seq_no += 1
                    assistant_event = AssistantReplyEvent(
                        event_type=EvaluationRunEventType.ASSISTANT_REPLY,
                        seq_no=seq_no,
                        timestamp=now,
                        turn_id=turn,
                        text=assistant_text,
                    )
                    await recorder.append(
                        event_type=assistant_event.event_type,
                        payload_json=assistant_event.model_dump(mode="json"),
                        event_timestamp=now,
                    )
                    event_models.append(assistant_event)
                    conversation_for_test_agent.append(
                        {"role": "target_agent", "text": assistant_text}
                    )
                    await db.commit()

                if runtime_error:
                    raise RuntimeError(runtime_error)

            finished_at = datetime.now(UTC)
            seq_no += 1
            run_finished = RunFinishedEvent(
                event_type=EvaluationRunEventType.RUN_FINISHED,
                seq_no=seq_no,
                timestamp=finished_at,
                aggregate_score=None,
            )
            await recorder.append(
                event_type=run_finished.event_type,
                payload_json=run_finished.model_dump(mode="json"),
                event_timestamp=finished_at,
            )
            event_models.append(run_finished)

            checks = evaluate_hard_checks(event_models)
            judge_result = await _judge.judge(
                EvaluationJudgeRequest(
                    run_id=run_id,
                    config=config_payload,
                    transcript=[e.model_dump(mode="json") for e in event_models],
                    tool_timeline=tool_timeline,
                    hard_check_results=checks,
                )
            )
            await store_judgment_result(
                db,
                run_id=run_id,
                result=judge_result,
                hard_checks=checks,
            )
            score = None
            if judge_result.rubric_scores:
                vals = list(judge_result.rubric_scores.values())
                score = round(sum(vals) / len(vals), 2)
            await _run_service.mark_completed(
                db,
                run,
                aggregate_score=score,
                summary=judge_result.summary,
            )
            await db.commit()
        except asyncio.CancelledError:
            await db.rollback()
            return
        except Exception as exc:
            await db.rollback()
            run = await db.scalar(
                select(EvaluationRun).where(EvaluationRun.id == run_id)
            )
            if run and run.status in {RunStatusModel.QUEUED, RunStatusModel.RUNNING}:
                recorder = EvaluationRunEventRecorder(db, run_id)
                now = datetime.now(UTC)
                max_seq_stmt = select(
                    func.coalesce(func.max(EvaluationRunEvent.seq_no), 0)
                ).where(EvaluationRunEvent.run_id == run_id)
                next_seq = int((await db.scalar(max_seq_stmt)) or 0) + 1
                run_failed = RunFailedEvent(
                    event_type=EvaluationRunEventType.RUN_FAILED,
                    seq_no=next_seq,
                    timestamp=now,
                    error_code="runtime_error",
                    error_message=str(exc),
                )
                await recorder.append(
                    event_type=run_failed.event_type,
                    payload_json=run_failed.model_dump(mode="json"),
                    event_timestamp=now,
                )
                run.status = RunStatusModel.FAILED
                run.summary = f"Execution failed: {exc}"
                run.ended_at = now
                await db.flush()
                await db.commit()


def _run_execution_subprocess_main(run_id_text: str) -> None:
    """Isolate evaluation execution from API server event loop/GIL."""

    async def _bootstrap() -> None:
        # Spawned subprocess does not run FastAPI lifespan hooks, so we must
        # reload persisted provider overrides explicitly.
        async with async_session() as db:
            builder_llm_cfg = await get_llm_config(db, "__builder__")
            try:
                builder_service.reconfigure(builder_llm_cfg)
            except PydanticAIUserError as exc:
                logger.warning(
                    "Builder LLM config invalid for evaluation subprocess; "
                    "keeping default builder model. provider={}, model={}, error={}",
                    builder_llm_cfg.provider,
                    builder_llm_cfg.model,
                    exc,
                )
        await _run_execution_task(uuid.UUID(run_id_text))

    asyncio.run(_bootstrap())


@router.post("/configs", response_model=EvaluationConfigResponse, status_code=201)
async def create_config(
    agent_id: uuid.UUID,
    body: EvaluationConfigCreate,
    db: AsyncSession = Depends(get_db),
) -> EvaluationConfigResponse:
    await _require_agent(db, agent_id)
    cfg, _ = await _config_service.create_config(
        db,
        agent_id=agent_id,
        name=body.name,
        payload=body.config,
    )
    await db.commit()
    await db.refresh(cfg)
    return _cfg_to_response(cfg)


@router.get("/configs", response_model=EvaluationConfigListResponse)
async def list_configs(
    agent_id: uuid.UUID,
    include_archived: bool = False,
    skip: int = Query(default=0, ge=0),
    limit: int = Query(default=50, ge=1, le=200),
    db: AsyncSession = Depends(get_db),
) -> EvaluationConfigListResponse:
    await _require_agent(db, agent_id)
    items, total = await _config_service.list_configs(
        db,
        agent_id=agent_id,
        include_archived=include_archived,
        offset=skip,
        limit=limit,
    )
    return EvaluationConfigListResponse(
        configs=[_cfg_to_response(item) for item in items],
        total=total,
    )


@router.get("/configs/{config_id}", response_model=EvaluationConfigDetailResponse)
async def get_config_detail(
    agent_id: uuid.UUID,
    config_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> EvaluationConfigDetailResponse:
    cfg = await _config_service.get_config(
        db,
        agent_id=agent_id,
        config_id=config_id,
        include_versions=True,
    )
    if not cfg:
        raise HTTPException(status_code=404, detail="Evaluation config not found")
    return EvaluationConfigDetailResponse(
        config=_cfg_to_response(cfg),
        versions=[_cfg_version_to_response(v) for v in cfg.versions],
    )


@router.post(
    "/configs/{config_id}/versions",
    response_model=EvaluationConfigVersionResponse,
    status_code=201,
)
async def create_config_version(
    agent_id: uuid.UUID,
    config_id: uuid.UUID,
    body: EvaluationConfigVersionCreate,
    db: AsyncSession = Depends(get_db),
) -> EvaluationConfigVersionResponse:
    version = await _config_service.create_version(
        db,
        agent_id=agent_id,
        config_id=config_id,
        payload=body.config,
    )
    if not version:
        raise HTTPException(status_code=404, detail="Evaluation config not found")
    await db.commit()
    await db.refresh(version)
    return _cfg_version_to_response(version)


@router.post(
    "/configs/{config_id}/run", response_model=EvaluationRunSummary, status_code=202
)
async def run_config(
    agent_id: uuid.UUID,
    config_id: uuid.UUID,
    body: EvaluationRunCreate,
    db: AsyncSession = Depends(get_db),
    idempotency_key: str | None = Header(default=None, alias="Idempotency-Key"),
) -> EvaluationRunSummary:
    await _require_agent(db, agent_id)
    id_key = _idempotency_key(
        op="run",
        agent_id=agent_id,
        config_id=config_id,
        key=idempotency_key,
    )
    if id_key and id_key in _idempotency_store:
        existing_id = _idempotency_store[id_key]
        existing = await db.scalar(
            select(EvaluationRun)
            .where(EvaluationRun.id == existing_id, EvaluationRun.agent_id == agent_id)
            .join(
                EvaluationConfigVersion,
                EvaluationConfigVersion.id == EvaluationRun.config_version_id,
            )
            .options(selectinload(EvaluationRun.config_version))
        )
        if existing:
            return _run_to_summary(existing)

    run = await _run_service.create_run(
        db,
        agent_id=agent_id,
        config_id=config_id,
        version=body.config_version,
    )
    if not run:
        raise HTTPException(status_code=404, detail="Config or version not found")
    if id_key:
        _idempotency_store[id_key] = run.id

    await db.commit()
    evaluation_worker.submit_process(
        run.id,
        _run_execution_subprocess_main,
        str(run.id),
    )
    run = await db.scalar(
        select(EvaluationRun)
        .where(EvaluationRun.id == run.id)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
    )
    if not run:
        raise HTTPException(status_code=500, detail="Run creation failed")
    return _run_to_summary(run)


@router.get("/runs", response_model=EvaluationRunListResponse)
async def list_runs(
    agent_id: uuid.UUID,
    status: EvaluationRunStatus | None = None,
    config_id: uuid.UUID | None = None,
    started_from: datetime | None = None,
    started_to: datetime | None = None,
    skip: int = Query(default=0, ge=0),
    limit: int = Query(default=50, ge=1, le=200),
    db: AsyncSession = Depends(get_db),
) -> EvaluationRunListResponse:
    await _require_agent(db, agent_id)
    await _reconcile_orphaned_runs(db, agent_id=agent_id)

    filters = [EvaluationRun.agent_id == agent_id]
    if status is not None:
        filters.append(EvaluationRun.status == RunStatusModel(status.value))
    if config_id is not None:
        filters.append(EvaluationRun.config_id == config_id)
    if started_from is not None:
        filters.append(EvaluationRun.started_at >= started_from)
    if started_to is not None:
        filters.append(EvaluationRun.started_at <= started_to)

    stmt: Select[tuple[EvaluationRun]] = (
        select(EvaluationRun)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
        .where(and_(*filters))
        .order_by(EvaluationRun.created_at.desc())
        .offset(skip)
        .limit(limit)
    )
    runs = list((await db.scalars(stmt)).all())

    total_stmt = select(func.count(EvaluationRun.id)).where(and_(*filters))
    total = int((await db.scalar(total_stmt)) or 0)
    return EvaluationRunListResponse(
        runs=[_run_to_summary(r) for r in runs],
        total=total,
    )


@router.get("/runs/{run_id}", response_model=EvaluationRunDetailResponse)
async def get_run_detail(
    agent_id: uuid.UUID,
    run_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> EvaluationRunDetailResponse:
    run = await db.scalar(
        select(EvaluationRun)
        .where(EvaluationRun.id == run_id, EvaluationRun.agent_id == agent_id)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
    )
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")
    judgment = await db.scalar(
        select(EvaluationJudgment)
        .where(EvaluationJudgment.run_id == run_id)
        .order_by(EvaluationJudgment.created_at.desc())
    )
    return EvaluationRunDetailResponse(
        run=_run_to_summary(run),
        hard_checks=judgment.hard_checks if judgment else {},
        rubric_scores=judgment.rubric_scores if judgment else {},
        summary=(judgment.summary if judgment else run.summary),
    )


@router.delete("/runs/{run_id}", response_model=EvaluationRunsDeleteResponse)
async def delete_run(
    agent_id: uuid.UUID,
    run_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> EvaluationRunsDeleteResponse:
    run = await db.scalar(
        select(EvaluationRun).where(
            EvaluationRun.id == run_id,
            EvaluationRun.agent_id == agent_id,
        )
    )
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")
    if run.status in {RunStatusModel.QUEUED, RunStatusModel.RUNNING}:
        raise HTTPException(
            status_code=409,
            detail="Cannot delete an active run. Cancel it first.",
        )

    await db.delete(run)
    await db.commit()
    return EvaluationRunsDeleteResponse(deleted_count=1, skipped_active_count=0)


@router.delete("/runs", response_model=EvaluationRunsDeleteResponse)
async def clear_run_history(
    agent_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
) -> EvaluationRunsDeleteResponse:
    await _require_agent(db, agent_id)
    active_count_stmt = select(func.count(EvaluationRun.id)).where(
        EvaluationRun.agent_id == agent_id,
        EvaluationRun.status.in_([RunStatusModel.QUEUED, RunStatusModel.RUNNING]),
    )
    skipped_active_count = int((await db.scalar(active_count_stmt)) or 0)

    delete_stmt = (
        delete(EvaluationRun)
        .where(
            EvaluationRun.agent_id == agent_id,
            EvaluationRun.status.in_(
                [
                    RunStatusModel.COMPLETED,
                    RunStatusModel.FAILED,
                    RunStatusModel.CANCELLED,
                ]
            ),
        )
        .returning(EvaluationRun.id)
    )
    deleted_ids = list((await db.scalars(delete_stmt)).all())
    await db.commit()
    return EvaluationRunsDeleteResponse(
        deleted_count=len(deleted_ids),
        skipped_active_count=skipped_active_count,
    )


@router.get("/runs/{run_id}/events")
async def stream_run_events(
    agent_id: uuid.UUID,
    run_id: uuid.UUID,
    from_seq: int = Query(default=0, ge=0),
) -> StreamingResponse:
    async def event_gen() -> Any:
        last_seq = from_seq
        terminal_seen = False
        while True:
            async with async_session() as db:
                run = await db.scalar(
                    select(EvaluationRun).where(
                        EvaluationRun.id == run_id,
                        EvaluationRun.agent_id == agent_id,
                    )
                )
                if not run:
                    yield 'event: error\ndata: {"message":"Run not found"}\n\n'
                    return

                rows = list(
                    (
                        await db.scalars(
                            select(EvaluationRunEvent)
                            .where(
                                EvaluationRunEvent.run_id == run_id,
                                EvaluationRunEvent.seq_no > last_seq,
                            )
                            .order_by(EvaluationRunEvent.seq_no.asc())
                        )
                    ).all()
                )
                for row in rows:
                    last_seq = row.seq_no
                    payload = {
                        "run_id": str(run_id),
                        "event": {
                            "event_type": row.event_type,
                            "seq_no": row.seq_no,
                            "timestamp": row.event_timestamp.isoformat(),
                            **row.payload_json,
                        },
                    }
                    yield f"event: run_event\ndata: {json.dumps(payload, ensure_ascii=True)}\n\n"

                if run.status in {
                    RunStatusModel.COMPLETED,
                    RunStatusModel.FAILED,
                    RunStatusModel.CANCELLED,
                }:
                    if terminal_seen:
                        return
                    terminal_seen = True

            await asyncio.sleep(1.0)

    return StreamingResponse(event_gen(), media_type="text/event-stream")


@router.post("/runs/{run_id}/cancel", response_model=EvaluationRunSummary)
async def cancel_run(
    agent_id: uuid.UUID,
    run_id: uuid.UUID,
    db: AsyncSession = Depends(get_db),
    idempotency_key: str | None = Header(default=None, alias="Idempotency-Key"),
) -> EvaluationRunSummary:
    run = await db.scalar(
        select(EvaluationRun)
        .where(EvaluationRun.id == run_id, EvaluationRun.agent_id == agent_id)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
    )
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")

    id_key = _idempotency_key(
        op="cancel", agent_id=agent_id, run_id=run_id, key=idempotency_key
    )
    if id_key and id_key in _idempotency_store:
        return _run_to_summary(run)

    if run.status in {
        RunStatusModel.COMPLETED,
        RunStatusModel.FAILED,
        RunStatusModel.CANCELLED,
    }:
        if id_key:
            _idempotency_store[id_key] = run.id
        return _run_to_summary(run)

    await _run_service.cancel_run(db, run)
    if id_key:
        _idempotency_store[id_key] = run.id
    await db.commit()
    return _run_to_summary(run)


@router.post(
    "/runs/{run_id}/rerun", response_model=EvaluationRunSummary, status_code=202
)
async def rerun_exact(
    agent_id: uuid.UUID,
    run_id: uuid.UUID,
    body: EvaluationRunRerunRequest,
    db: AsyncSession = Depends(get_db),
    idempotency_key: str | None = Header(default=None, alias="Idempotency-Key"),
) -> EvaluationRunSummary:
    prior = await db.scalar(
        select(EvaluationRun)
        .where(EvaluationRun.id == run_id, EvaluationRun.agent_id == agent_id)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
    )
    if not prior:
        raise HTTPException(status_code=404, detail="Run not found")

    id_key = _idempotency_key(
        op="rerun", agent_id=agent_id, run_id=run_id, key=idempotency_key
    )
    if id_key and id_key in _idempotency_store:
        existing = await db.scalar(
            select(EvaluationRun)
            .where(
                EvaluationRun.id == _idempotency_store[id_key],
                EvaluationRun.agent_id == agent_id,
            )
            .join(
                EvaluationConfigVersion,
                EvaluationConfigVersion.id == EvaluationRun.config_version_id,
            )
            .options(selectinload(EvaluationRun.config_version))
        )
        if existing:
            return _run_to_summary(existing)

    version = prior.config_version.version if prior.config_version else None
    new_run = await _run_service.create_run(
        db,
        agent_id=agent_id,
        config_id=prior.config_id,
        version=version,
        seed_override=(
            body.seed_override if body.seed_override is not None else prior.seed
        ),
    )
    if not new_run:
        raise HTTPException(status_code=400, detail="Unable to rerun")
    if id_key:
        _idempotency_store[id_key] = new_run.id

    await db.commit()
    evaluation_worker.submit_process(
        new_run.id,
        _run_execution_subprocess_main,
        str(new_run.id),
    )
    new_run = await db.scalar(
        select(EvaluationRun)
        .where(EvaluationRun.id == new_run.id)
        .join(
            EvaluationConfigVersion,
            EvaluationConfigVersion.id == EvaluationRun.config_version_id,
        )
        .options(selectinload(EvaluationRun.config_version))
    )
    if not new_run:
        raise HTTPException(status_code=500, detail="Rerun creation failed")
    return _run_to_summary(new_run)
