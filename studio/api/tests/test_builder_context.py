"""Tests for agent builder context (history compression)."""

import pytest
from pydantic_ai.messages import (
    ModelMessage,
    ModelRequest,
    ModelResponse,
    TextPart,
    ToolCallPart,
    ToolReturnPart,
    UserPromptPart,
)
from pydantic_ai.models.test import TestModel

from app.agent_builder.context import (
    COMPRESSION_THRESHOLD,
    KEEP_RECENT,
    _find_safe_split,
    _format_messages_for_compression,
    compress_history,
)

# ── Helpers ──────────────────────────────────────────────────────


def _msg(role: str, content: str = "") -> ModelMessage:
    """Shorthand for building a ModelMessage."""
    content = content or f"{role} message"
    if role == "user":
        return ModelRequest(parts=[UserPromptPart(content=content, timestamp=None)])
    elif role == "assistant":
        return ModelResponse(parts=[TextPart(content=content)], timestamp=None)
    elif role == "tool_call":
        return ModelResponse(
            parts=[
                ToolCallPart(
                    tool_name="test_tool",
                    args={"arg": content},
                    tool_call_id="call_123",
                )
            ],
            timestamp=None,
        )
    elif role == "tool":
        return ModelRequest(
            parts=[
                ToolReturnPart(
                    tool_name="test_tool",
                    content=content,
                    tool_call_id="call_123",
                    timestamp=None,
                )
            ]
        )
    raise ValueError(f"Unknown role {role}")


def _conversation(n: int) -> list[ModelMessage]:
    """Build a simple alternating user/assistant conversation of length n."""
    msgs: list[ModelMessage] = []
    for i in range(n):
        role = "user" if i % 2 == 0 else "assistant"
        msgs.append(_msg(role, f"msg-{i}"))
    return msgs


# ── _find_safe_split ─────────────────────────────────────────────


class TestFindSafeSplit:
    """Tests for the turn-boundary-aware splitter.

    Invariant: recent portion always starts with a ``user`` message.
    """

    def test_no_tool_messages(self) -> None:
        """Plain user/assistant conversation — split lands on user."""
        msgs = [_msg("user"), _msg("assistant"), _msg("user"), _msg("assistant")]
        assert _find_safe_split(msgs, 2) == 2  # messages[2] is user ✓

    def test_split_on_tool_walks_back_to_user(self) -> None:
        """If desired split lands on tool/assistant, walk back to the user."""
        msgs = [
            _msg("user"),
            _msg("tool_call", "calling tool"),
            _msg("tool", "result1"),
            _msg("tool", "result2"),
            _msg("user", "thanks"),
        ]
        # desired=2 → tool → walk back to user at 0 → clamp to 1
        assert _find_safe_split(msgs, 2) == 1
        # desired=3 → tool → same
        assert _find_safe_split(msgs, 3) == 1

    def test_split_on_assistant_walks_back_to_user(self) -> None:
        """Splitting on an assistant message walks back to prior user."""
        msgs = [
            _msg("user", "q1"),  # 0
            _msg("assistant", "a1"),  # 1
            _msg("user", "q2"),  # 2
            _msg("assistant", "a2"),  # 3
            _msg("user", "q3"),  # 4
        ]
        # desired=1 → assistant → walk back to user at 0 → clamp to 1
        assert _find_safe_split(msgs, 1) == 1
        # desired=3 → assistant → walk back to user at 2
        assert _find_safe_split(msgs, 3) == 2

    def test_split_after_tool_group_is_safe(self) -> None:
        """Splitting right on a user msg after a tool group is fine."""
        msgs = [
            _msg("user"),
            _msg("tool_call", "calling tool"),
            _msg("tool", "result"),
            _msg("user", "next question"),
            _msg("assistant", "answer"),
        ]
        assert _find_safe_split(msgs, 3) == 3

    def test_multiple_tool_groups(self) -> None:
        """Multiple tool-call groups — split lands on user boundaries."""
        msgs = [
            _msg("user", "q1"),  # 0
            _msg("tool_call", "call1"),  # 1
            _msg("tool", "r1"),  # 2
            _msg("user", "q2"),  # 3
            _msg("tool_call", "call2"),  # 4
            _msg("tool", "r2a"),  # 5
            _msg("tool", "r2b"),  # 6
            _msg("user", "q3"),  # 7
        ]
        # desired=5 → tool → walk back to user at 3
        assert _find_safe_split(msgs, 5) == 3
        # desired=6 → tool → walk back to user at 3
        assert _find_safe_split(msgs, 6) == 3
        # desired=3 → user → safe
        assert _find_safe_split(msgs, 3) == 3
        # desired=7 → user → safe
        assert _find_safe_split(msgs, 7) == 7

    def test_minimum_return_is_one(self) -> None:
        """Should always return at least 1 so we compress something."""
        msgs = [_msg("tool"), _msg("tool"), _msg("user")]
        assert _find_safe_split(msgs, 1) >= 1

    def test_desired_beyond_length(self) -> None:
        """Desired split beyond message count is clamped."""
        msgs = [_msg("user"), _msg("assistant")]
        # desired=100 → clamped to len-1=1, assistant → walk back to user at 0 → clamp to 1
        assert _find_safe_split(msgs, 100) == 1

    def test_tool_at_very_end(self) -> None:
        """Tool result as the last message — split walks back to user."""
        msgs = [
            _msg("user"),
            _msg("assistant"),
            _msg("tool"),
        ]
        # desired=2 → tool → walk back past assistant to user at 0 → clamp to 1
        assert _find_safe_split(msgs, 2) == 1


# ── _format_messages_for_compression ─────────────────────────────


class TestFormatMessages:
    """Tests for the compression transcript formatter."""

    def test_basic_formatting(self) -> None:
        msgs = [_msg("user", "hello"), _msg("assistant", "hi")]
        result = _format_messages_for_compression(msgs)
        assert "USER: hello" in result
        assert "ASSISTANT: hi" in result

    def test_tool_messages_included(self) -> None:
        msgs = [_msg("tool", "some result")]
        result = _format_messages_for_compression(msgs)
        assert "TOOL_RESULT" in result
        assert "some result" in result

    def test_empty_list(self) -> None:
        assert _format_messages_for_compression([]) == ""

    def test_messages_separated_by_double_newline(self) -> None:
        msgs = [_msg("user", "a"), _msg("assistant", "b")]
        result = _format_messages_for_compression(msgs)
        assert "\n\n" in result


# ── compress_history ─────────────────────────────────────────────


class TestCompressHistory:
    """Tests for the main compress_history function."""

    @pytest.mark.asyncio
    async def test_short_history_unchanged(self) -> None:
        """History below threshold is returned as-is."""
        model = TestModel()
        msgs = _conversation(10)
        result = await compress_history(model, msgs)
        assert result is msgs  # Same object, no compression

    @pytest.mark.asyncio
    async def test_at_threshold_unchanged(self) -> None:
        """History exactly at threshold is returned as-is."""
        model = TestModel()
        msgs = _conversation(COMPRESSION_THRESHOLD)
        result = await compress_history(model, msgs)
        assert result is msgs

    @pytest.mark.asyncio
    async def test_above_threshold_triggers_compression(self) -> None:
        """History above threshold triggers LLM compression."""
        model = TestModel(custom_output_text="OVERALL GOAL:\nBuild a hotel agent")

        msgs = _conversation(COMPRESSION_THRESHOLD + 10)
        result = await compress_history(model, msgs)

        # Result starts with summary context (user prompt) + assistant ack
        assert isinstance(result[0], ModelRequest)
        assert isinstance(result[0].parts[0], UserPromptPart)
        assert "Context from earlier" in str(result[0].parts[0].content)
        assert "OVERALL GOAL:\nBuild a hotel agent" in str(result[0].parts[0].content)

        assert isinstance(result[1], ModelResponse)
        assert isinstance(result[1].parts[0], TextPart)
        assert "Got it" in str(result[1].parts[0].content)

        # Recent messages are preserved
        assert len(result) >= KEEP_RECENT

    @pytest.mark.asyncio
    async def test_llm_failure_falls_back_to_recent(self) -> None:
        """If LLM compression fails, fall back to recent messages only."""

        class FailingModel(TestModel):
            async def request(self, *args, **kwargs):
                raise RuntimeError("LLM down")

        model = FailingModel()
        msgs = _conversation(COMPRESSION_THRESHOLD + 10)
        result = await compress_history(model, msgs)

        # Should get the recent tail, not crash
        assert len(result) > 0
        assert len(result) <= COMPRESSION_THRESHOLD

    @pytest.mark.asyncio
    async def test_empty_summary_falls_back(self) -> None:
        """If LLM returns empty string, fall back to recent messages."""
        model = TestModel(custom_output_text="   ")

        msgs = _conversation(COMPRESSION_THRESHOLD + 10)
        result = await compress_history(model, msgs)

        # Should not include a summary, just recent messages
        assert not any(
            isinstance(m, ModelRequest)
            and isinstance(m.parts[0], UserPromptPart)
            and "Context from earlier" in str(m.parts[0].content)
            for m in result
        )

    @pytest.mark.asyncio
    async def test_tool_messages_not_orphaned(self) -> None:
        """Compression split should not orphan tool results."""
        model = TestModel(custom_output_text="OVERALL GOAL:\nTest agent")

        # Build history: normal messages, then a tool group near the split point
        msgs: list[ModelMessage] = []
        # Fill with enough messages to exceed threshold
        for i in range(COMPRESSION_THRESHOLD + 5):
            msgs.append(_msg("user" if i % 2 == 0 else "assistant", f"m{i}"))

        # Insert a tool group right where the naive split would land
        split_area = len(msgs) - KEEP_RECENT
        msgs.insert(split_area, _msg("tool", "tool-result-b"))
        msgs.insert(split_area, _msg("tool", "tool-result-a"))

        result = await compress_history(model, msgs)

        recently_kept = result[2:]  # skip the first two (context summary + ack)
        if len(recently_kept) > 0:
            first_kept = recently_kept[0]
            if isinstance(first_kept, ModelRequest):
                # Ensure the first kept message doesn't start with a ToolReturnPart
                assert not isinstance(first_kept.parts[0], ToolReturnPart), (
                    "Recent portion starts with orphaned tool result"
                )
