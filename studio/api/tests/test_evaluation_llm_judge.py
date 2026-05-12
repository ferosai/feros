from __future__ import annotations

import uuid

import pytest

from app.lib.config import LLMConfig
from app.schemas.evaluation import (
    EvaluationConfigPayload,
    EvaluationJudgeConfig,
    EvaluationJudgeRequest,
)
from app.services.evaluations.judging import BuilderLLMEvaluationJudge


def _mock_complete(response: str):
    async def _inner(**_kwargs: object) -> str:
        return response

    return _inner


@pytest.mark.asyncio
async def test_builder_judge_uses_llm_response(monkeypatch: pytest.MonkeyPatch) -> None:

    monkeypatch.setattr(
        "app.services.evaluations.judging._complete_with_builder_llm",
        _mock_complete(
            '{"rubric_scores":{"task_completion":88},"summary":"ok","failure_highlights":[],"recommendations":[]}'
        ),
    )
    judge = BuilderLLMEvaluationJudge()
    result = await judge.judge(
        EvaluationJudgeRequest(
            run_id=uuid.uuid4(),
            config=EvaluationConfigPayload(),
            transcript=[],
            tool_timeline=[],
            hard_check_results={"run_not_failed": True},
        ),
        llm_cfg=LLMConfig(),
    )
    assert result.rubric_scores["task_completion"] == 88
    assert result.summary == "ok"


@pytest.mark.asyncio
async def test_builder_judge_falls_back_on_bad_json(
    monkeypatch: pytest.MonkeyPatch,
) -> None:

    monkeypatch.setattr(
        "app.services.evaluations.judging._complete_with_builder_llm",
        _mock_complete("not json"),
    )
    judge = BuilderLLMEvaluationJudge()
    result = await judge.judge(
        EvaluationJudgeRequest(
            run_id=uuid.uuid4(),
            config=EvaluationConfigPayload(),
            transcript=[],
            tool_timeline=[],
            hard_check_results={"run_not_failed": True, "tools_resolved": False},
        ),
        llm_cfg=LLMConfig(),
    )
    assert "task_completion" in result.rubric_scores


@pytest.mark.asyncio
async def test_builder_judge_normalizes_five_point_scale(
    monkeypatch: pytest.MonkeyPatch,
) -> None:

    monkeypatch.setattr(
        "app.services.evaluations.judging._complete_with_builder_llm",
        _mock_complete(
            '{"rubric_scores":{"task_completion":4.5,"tool_usage":3},"summary":"ok","failure_highlights":[],"recommendations":[]}'
        ),
    )
    judge = BuilderLLMEvaluationJudge()
    result = await judge.judge(
        EvaluationJudgeRequest(
            run_id=uuid.uuid4(),
            config=EvaluationConfigPayload(),
            transcript=[],
            tool_timeline=[],
            hard_check_results={"run_not_failed": True},
        ),
        llm_cfg=LLMConfig(),
    )
    assert result.rubric_scores["task_completion"] == 90.0
    assert result.rubric_scores["tool_usage"] == 60.0


@pytest.mark.asyncio
async def test_builder_judge_enforces_selected_rubric_dimensions(
    monkeypatch: pytest.MonkeyPatch,
) -> None:

    monkeypatch.setattr(
        "app.services.evaluations.judging._complete_with_builder_llm",
        _mock_complete(
            '{"rubric_scores":{"intent_detection":84,"value_delivery":75},"summary":"ok","failure_highlights":[],"recommendations":[]}'
        ),
    )
    judge = BuilderLLMEvaluationJudge()
    result = await judge.judge(
        EvaluationJudgeRequest(
            run_id=uuid.uuid4(),
            config=EvaluationConfigPayload(
                judge=EvaluationJudgeConfig(
                    enabled=True, rubric_version="sales_readiness"
                )
            ),
            transcript=[],
            tool_timeline=[],
            hard_check_results={"run_not_failed": True},
        ),
        llm_cfg=LLMConfig(),
    )
    assert list(result.rubric_scores.keys()) == [
        "intent_detection",
        "value_delivery",
        "objection_handling",
        "next_step_clarity",
        "compliance",
    ]
    assert result.rubric_scores["intent_detection"] == 84.0
    assert result.rubric_scores["objection_handling"] == 0.0
