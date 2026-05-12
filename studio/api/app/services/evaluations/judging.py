"""Hard-check and baseline judge implementations for evaluation runs."""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass, field
from typing import Any

from pydantic_ai import Agent
from sqlalchemy.ext.asyncio import AsyncSession

from app.lib.config import LLMConfig
from app.lib.llm_factory import build_model
from app.models.evaluation import EvaluationJudgment
from app.schemas.evaluation import (
    AssistantReplyEvent,
    EvaluationJudgeRequest,
    EvaluationJudgeResponse,
    EvaluationRunEvent,
    RunFailedEvent,
    RunFinishedEvent,
    ToolCallEvent,
    ToolMockResultEvent,
)
from app.services.evaluations.contracts import EvaluationJudge
from app.services.evaluations.rubrics import (
    enforce_rubric_dimensions,
    resolve_rubric_preset,
)


def evaluate_hard_checks(events: list[EvaluationRunEvent]) -> dict[str, bool]:
    has_assistant_reply = any(isinstance(e, AssistantReplyEvent) for e in events)
    has_run_failed = any(isinstance(e, RunFailedEvent) for e in events)
    has_run_finished = any(isinstance(e, RunFinishedEvent) for e in events)

    tool_calls = sum(1 for e in events if isinstance(e, ToolCallEvent))
    tool_results = sum(1 for e in events if isinstance(e, ToolMockResultEvent))
    tools_resolved = tool_calls == tool_results

    return {
        "has_assistant_reply": has_assistant_reply,
        "run_not_failed": not has_run_failed,
        "run_finished": has_run_finished,
        "tools_resolved": tools_resolved,
    }


@dataclass(slots=True)
class BaselineEvaluationJudge(EvaluationJudge):
    """Fallback judge that returns deterministic baseline scoring.

    Phase 1 keeps this as a stable adapter; LLM-backed judge can replace
    this implementation without changing call sites.
    """

    async def judge(
        self, payload: EvaluationJudgeRequest, llm_cfg: LLMConfig | None = None
    ) -> EvaluationJudgeResponse:
        hard = payload.hard_check_results
        passed = sum(1 for ok in hard.values() if ok)
        total = max(len(hard), 1)
        completion = round((passed / total) * 100, 2)
        tool_score = 100.0 if hard.get("tools_resolved", False) else 20.0
        stability = 100.0 if hard.get("run_not_failed", False) else 0.0
        reply = 100.0 if hard.get("has_assistant_reply", False) else 0.0
        finished = 100.0 if hard.get("run_finished", False) else 0.0

        preset = resolve_rubric_preset(payload.config.judge.rubric_version)
        rubric: dict[str, float] = {}
        for dim in preset.dimensions:
            key = dim.key
            if "tool" in key:
                rubric[key] = tool_score
            elif any(
                marker in key for marker in ("safety", "compliance", "policy", "risk")
            ):
                rubric[key] = min(stability, finished)
            elif any(marker in key for marker in ("persona", "empathy", "confidence")):
                rubric[key] = round((completion + reply) / 2, 2)
            elif any(marker in key for marker in ("clarity", "consistency")):
                rubric[key] = round((completion + stability) / 2, 2)
            else:
                rubric[key] = completion
        rubric = enforce_rubric_dimensions(payload.config.judge.rubric_version, rubric)
        summary = (
            f"Baseline judge: {passed}/{total} hard checks passed. "
            f"Primary score={completion}."
        )
        highlights = []
        if not hard.get("run_not_failed", True):
            highlights.append("Run failed before completion.")
        if not hard.get("tools_resolved", True):
            highlights.append(
                "At least one tool call had no corresponding mock result."
            )

        recommendations = []
        if not hard.get("has_assistant_reply", True):
            recommendations.append(
                "Ensure the agent can always produce at least one reply."
            )
        if not hard.get("tools_resolved", True):
            recommendations.append(
                "Validate deterministic tool mocking for every tool call."
            )

        return EvaluationJudgeResponse(
            rubric_scores=rubric,
            summary=summary,
            failure_highlights=highlights,
            recommendations=recommendations,
        )


def _extract_json(text: str) -> dict[str, Any] | None:
    stripped = text.strip()
    try:
        parsed = json.loads(stripped)
        return parsed if isinstance(parsed, dict) else None
    except json.JSONDecodeError:
        pass

    start = stripped.find("{")
    end = stripped.rfind("}")
    if start == -1 or end == -1 or end <= start:
        return None
    try:
        parsed = json.loads(stripped[start : end + 1])
        return parsed if isinstance(parsed, dict) else None
    except json.JSONDecodeError:
        return None


def normalize_rubric_scores(scores: dict[str, float]) -> dict[str, float]:
    """Normalize rubric outputs to a stable 0-100 scale.

    Some models may return 1-5 style scores despite prompt guidance.
    This function rescales 0-1 -> 0-100 and 0-5 -> 0-100, then clamps.
    """

    if not scores:
        return {}

    cleaned: dict[str, float] = {}
    for key, value in scores.items():
        if isinstance(value, (int, float)):
            cleaned[key] = float(value)

    if not cleaned:
        return {}

    values = list(cleaned.values())
    in_zero_to_one = all(0.0 <= v <= 1.0 for v in values)
    in_zero_to_five = all(0.0 <= v <= 5.0 for v in values)

    scale = 1.0
    if in_zero_to_one:
        scale = 100.0
    elif in_zero_to_five:
        scale = 20.0

    normalized: dict[str, float] = {}
    for key, value in cleaned.items():
        scaled = value * scale
        normalized[key] = round(min(max(scaled, 0.0), 100.0), 2)
    return normalized


@dataclass(slots=True)
class BuilderLLMEvaluationJudge(EvaluationJudge):
    """LLM judge adapter that uses current builder LLM settings."""

    fallback: EvaluationJudge = field(default_factory=BaselineEvaluationJudge)

    async def judge(
        self, payload: EvaluationJudgeRequest, llm_cfg: LLMConfig | None = None
    ) -> EvaluationJudgeResponse:
        if not llm_cfg:
            return await self.fallback.judge(payload, llm_cfg=None)

        preset = resolve_rubric_preset(payload.config.judge.rubric_version)
        dimensions = [{"key": d.key, "label": d.label} for d in preset.dimensions]

        system_prompt = (
            "You are an evaluation judge. Return strict JSON only with keys: "
            "rubric_scores (object[str,float]), summary (string), "
            "failure_highlights (string[]), recommendations (string[]). "
            "Each rubric score must be numeric in range 0..100. "
            "The rubric_scores object must include exactly these keys: "
            + ", ".join(d["key"] for d in dimensions)
            + "."
        )
        user_prompt = json.dumps(
            {
                "run_id": str(payload.run_id),
                "rubric_id": preset.id,
                "rubric_display_name": preset.display_name,
                "rubric_dimensions": dimensions,
                "config": payload.config.model_dump(mode="json"),
                "hard_check_results": payload.hard_check_results,
                "transcript": payload.transcript,
                "tool_timeline": payload.tool_timeline,
            },
            ensure_ascii=True,
        )
        try:
            raw = await _complete_with_builder_llm(
                llm_cfg=llm_cfg,
                system_prompt=system_prompt,
                user_prompt=user_prompt,
            )
            parsed = _extract_json(raw)
            if not parsed:
                return await self.fallback.judge(payload)
            response = EvaluationJudgeResponse.model_validate(parsed)
            normalized_scores = normalize_rubric_scores(response.rubric_scores)
            enforced_scores = enforce_rubric_dimensions(
                payload.config.judge.rubric_version, normalized_scores
            )
            return response.model_copy(update={"rubric_scores": enforced_scores})
        except Exception:
            return await self.fallback.judge(payload)


# Backward-compatible alias while callers migrate names.
VoiceSettingsLLMEvaluationJudge = BuilderLLMEvaluationJudge


async def _complete_with_builder_llm(
    *,
    llm_cfg: LLMConfig,
    system_prompt: str,
    user_prompt: str,
) -> str:
    model, model_settings = build_model(llm_cfg)
    judge_agent: Agent[None, str] = Agent(
        model=model,
        output_type=str,
        system_prompt=system_prompt,
    )
    run = await judge_agent.run(
        user_prompt,
        model_settings=model_settings,
    )
    return run.output


async def store_judgment_result(
    db: AsyncSession,
    *,
    run_id: uuid.UUID,
    result: EvaluationJudgeResponse,
    hard_checks: dict[str, bool],
) -> EvaluationJudgment:
    judgment = EvaluationJudgment(
        run_id=run_id,
        hard_checks=hard_checks,
        rubric_scores=result.rubric_scores,
        summary=result.summary,
        failure_highlights=result.failure_highlights,
        recommendations=result.recommendations,
    )
    db.add(judgment)
    await db.flush()
    return judgment
