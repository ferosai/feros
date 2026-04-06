"""Smoke checks for voice_rust Python bindings used by backend features."""

from __future__ import annotations

import importlib.util

import pytest


def _has_voice_rust() -> bool:
    return importlib.util.find_spec("voice_rust") is not None


@pytest.mark.skipif(not _has_voice_rust(), reason="voice_rust is not installed")
def test_voice_rust_exports_agent_runner() -> None:
    """AgentRunner must be exported for deterministic tool-mock test orchestration."""
    import voice_rust

    assert hasattr(voice_rust, "AgentRunner"), (
        "voice_rust.AgentRunner is missing; hook-based test runner is unavailable"
    )
