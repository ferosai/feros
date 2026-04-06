"""Tests for the ask_user tool that pauses the agent loop."""

from typing import Any

import pytest
from pydantic_ai import Agent
from pydantic_ai.models.test import TestModel

from app.agent_builder.service import BuilderDeps


@pytest.fixture
def deps() -> BuilderDeps:
    """Create minimal BuilderDeps for testing."""
    return BuilderDeps(
        agent_id="test-agent-id",
        agent_name="Test Agent",
        current_config=None,
    )


def _build_service(
    call_tools: list[str],
    custom_output_text: str = "fallback output",
    extra_tools: bool = False,
) -> Any:
    """Create a lightweight BuilderService with a TestModel."""
    from app.agent_builder.service import BuilderService

    service = BuilderService.__new__(BuilderService)
    model = TestModel(call_tools=call_tools, custom_output_text=custom_output_text)
    service.stream_agent = Agent(
        model=model,
        output_type=str,
        deps_type=BuilderDeps,
    )
    service._HIDDEN_TOOLS = {"save_agent_config", "ask_user"}
    service._model_settings = None

    # Register the ask_user tool on this agent instance
    @service.stream_agent.tool
    async def ask_user(ctx, question: str) -> str:  # type: ignore[no-untyped-def]
        """Pause and wait for user response."""
        ctx.deps.pause_requested = True
        return "Paused. Waiting for user response."

    if extra_tools:

        @service.stream_agent.tool
        async def discover_schema(ctx, source: str) -> str:  # type: ignore[no-untyped-def]
            """Discover a schema from a data source."""
            return "SCHEMA_MARKER: name, phone, date"

    return service


async def _collect_events(
    service: Any,
    deps: BuilderDeps,
    prompt: str = "Build a restaurant booking agent",
) -> list[dict]:
    """Run _iter_agent_events and collect all dict events."""
    events: list[dict] = []
    async for item in service._iter_agent_events(
        user_prompt=prompt,
        deps=deps,
        message_history=[],
    ):
        if isinstance(item, dict):
            events.append(item)
    return events


class TestAskUserLoopBreak:
    """Verify that the ask_user tool breaks the agent loop."""

    @pytest.mark.asyncio
    async def test_ask_user_sets_pause_flag(self, deps: BuilderDeps) -> None:
        """Calling ask_user should set pause_requested = True."""
        service = _build_service(call_tools=["ask_user"])
        await _collect_events(service, deps)
        assert deps.pause_requested is True

    @pytest.mark.asyncio
    async def test_ask_user_prevents_second_model_round(
        self, deps: BuilderDeps
    ) -> None:
        """The LLM's second-round output should NOT appear when ask_user
        breaks the loop after the first round.

        TestModel behavior:
          round 1: calls ask_user → tool result
          round 2: would emit custom_output_text

        If the break works, custom_output_text never appears.
        """
        service = _build_service(
            call_tools=["ask_user"],
            custom_output_text="THIS_SHOULD_NOT_APPEAR",
        )
        events = await _collect_events(service, deps)

        all_content = " ".join(str(e.get("content", "")) for e in events)
        assert "THIS_SHOULD_NOT_APPEAR" not in all_content

    @pytest.mark.asyncio
    async def test_ask_user_hidden_from_chat(self, deps: BuilderDeps) -> None:
        """ask_user tool call/return events should not appear in the stream."""
        service = _build_service(call_tools=["ask_user"])
        events = await _collect_events(service, deps)

        tool_events = [
            e
            for e in events
            if e.get("kind") in ("tool_return",) and e.get("tool_name") == "ask_user"
        ]
        assert tool_events == [], "ask_user events leaked to chat stream"

        tool_call_events = [
            e
            for e in events
            if e.get("kind") == "part_start"
            and e.get("part_kind") == "tool-call"
            and e.get("tool_name") == "ask_user"
        ]
        assert tool_call_events == [], "ask_user tool-call events leaked to chat stream"

    @pytest.mark.asyncio
    async def test_no_pause_without_ask_user(self, deps: BuilderDeps) -> None:
        """Without ask_user, the loop should complete normally."""
        service = _build_service(
            call_tools=[],
            custom_output_text="All done!",
        )
        events = await _collect_events(service, deps, prompt="Hello")

        assert deps.pause_requested is False

        # The output text should appear (TestModel streams words separately)
        all_content = " ".join(str(e.get("content", "")) for e in events)
        assert "done!" in all_content

    @pytest.mark.asyncio
    async def test_parallel_tools_all_results_present(self, deps: BuilderDeps) -> None:
        """When ask_user is called alongside another tool in the same round,
        both tools should execute and their results should be available.

        TestModel calls all tools in `call_tools` on the first round.
        We register discover_schema + ask_user.  After the break:
          - discover_schema result should appear in the stream (not hidden)
          - ask_user should be hidden
          - The loop should still break (no second round)
        """
        service = _build_service(
            call_tools=["discover_schema", "ask_user"],
            custom_output_text="THIS_SHOULD_NOT_APPEAR",
            extra_tools=True,
        )
        events = await _collect_events(service, deps)

        # Loop was broken
        assert deps.pause_requested is True

        # Second round output should NOT appear
        all_content = " ".join(str(e.get("content", "")) for e in events)
        assert "THIS_SHOULD_NOT_APPEAR" not in all_content

        # discover_schema result SHOULD appear in stream (it's not hidden)
        discover_returns = [
            e
            for e in events
            if e.get("kind") == "tool_return"
            and e.get("tool_name") == "discover_schema"
        ]
        assert len(discover_returns) == 1, "discover_schema result missing from stream"
        assert "SCHEMA_MARKER" in discover_returns[0]["content"]

        # ask_user should NOT appear in stream
        ask_returns = [
            e
            for e in events
            if e.get("kind") == "tool_return" and e.get("tool_name") == "ask_user"
        ]
        assert ask_returns == [], "ask_user result leaked to stream"
