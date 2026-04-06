from __future__ import annotations

from types import SimpleNamespace

import pytest

from app.lib import config as config_lib

# ── helper ──────────────────────────────────────────────────────


def _fake_get_provider_row(rows: dict[tuple[str, str], SimpleNamespace]):
    """Return an async fake for ``_get_provider_row`` backed by *rows*."""

    async def _fake(db, provider_type: str, provider_name: str):
        del db
        return rows.get((provider_type, provider_name))

    return _fake


# ── scoped-key resolution ──────────────────────────────────────


@pytest.mark.asyncio
async def test_builder_resolves_to_scoped_key(monkeypatch: pytest.MonkeyPatch) -> None:
    """__builder__ pointer with active=openai should read builder::openai."""
    rows = {
        ("llm", "__builder__"): SimpleNamespace(config_json={"active": "openai"}),
        ("llm", "builder::openai"): SimpleNamespace(
            config_json={
                "provider": "openai",
                "model": "gpt-4o",
                "base_url": "https://api.openai.com/v1",
                "temperature": 0.5,
                "max_tokens": 512,
                "api_key": "sk-builder",
            }
        ),
    }
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row(rows))

    llm = await config_lib.get_llm_config(db=object(), provider_name="__builder__")
    assert llm.provider == "openai"
    assert llm.model == "gpt-4o"
    assert llm.api_key == "sk-builder"
    assert llm.temperature == 0.5
    assert llm.max_tokens == 512


@pytest.mark.asyncio
async def test_voice_resolves_to_scoped_key(monkeypatch: pytest.MonkeyPatch) -> None:
    """__voice__ pointer with active=openrouter should read voice::openrouter."""
    rows = {
        ("llm", "__voice__"): SimpleNamespace(config_json={"active": "openrouter"}),
        ("llm", "voice::openrouter"): SimpleNamespace(
            config_json={
                "provider": "openrouter",
                "model": "google/gemini-3-flash-preview",
                "base_url": "https://openrouter.ai/api/v1",
                "temperature": 0.2,
                "max_tokens": 128,
                "api_key": "sk-voice",
            }
        ),
    }
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row(rows))

    llm = await config_lib.get_llm_config(db=object(), provider_name="__voice__")
    assert llm.provider == "openrouter"
    assert llm.model == "google/gemini-3-flash-preview"
    assert llm.api_key == "sk-voice"
    assert llm.temperature == 0.2
    assert llm.max_tokens == 128


@pytest.mark.asyncio
async def test_builder_and_voice_same_provider_independent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Builder and voice both using openai should each get their own config."""
    rows = {
        ("llm", "__builder__"): SimpleNamespace(config_json={"active": "openai"}),
        ("llm", "__voice__"): SimpleNamespace(config_json={"active": "openai"}),
        ("llm", "builder::openai"): SimpleNamespace(
            config_json={
                "provider": "openai",
                "model": "gpt-4o",
                "base_url": "https://api.openai.com/v1",
                "temperature": 0.7,
                "max_tokens": 256,
                "api_key": "sk-builder-key",
            }
        ),
        ("llm", "voice::openai"): SimpleNamespace(
            config_json={
                "provider": "openai",
                "model": "gpt-4o-mini",
                "base_url": "https://api.openai.com/v1",
                "temperature": 0.3,
                "max_tokens": 128,
                "api_key": "sk-voice-key",
            }
        ),
    }
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row(rows))

    builder = await config_lib.get_llm_config(db=object(), provider_name="__builder__")
    voice = await config_lib.get_llm_config(db=object(), provider_name="__voice__")

    assert builder.model == "gpt-4o"
    assert builder.api_key == "sk-builder-key"
    assert builder.temperature == 0.7
    assert builder.max_tokens == 256

    assert voice.model == "gpt-4o-mini"
    assert voice.api_key == "sk-voice-key"
    assert voice.temperature == 0.3
    assert voice.max_tokens == 128


# ── fallback to default when scoped row is missing ─────────────


@pytest.mark.asyncio
async def test_missing_scoped_row_returns_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """If the pointer row exists but scoped creds row is missing, return LLMConfig defaults."""
    rows = {
        ("llm", "__builder__"): SimpleNamespace(config_json={"active": "openai"}),
        # No builder::openai row
    }
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row(rows))

    llm = await config_lib.get_llm_config(db=object(), provider_name="__builder__")
    default = config_lib.LLMConfig()
    assert llm.provider == default.provider
    assert llm.model == default.model


@pytest.mark.asyncio
async def test_no_pointer_row_returns_default(monkeypatch: pytest.MonkeyPatch) -> None:
    """If no pointer row exists at all, return LLMConfig defaults."""
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row({}))

    llm = await config_lib.get_llm_config(db=object(), provider_name="__voice__")
    default = config_lib.LLMConfig()
    assert llm.provider == default.provider


@pytest.mark.asyncio
async def test_legacy_shared_row_is_not_read(monkeypatch: pytest.MonkeyPatch) -> None:
    """A legacy shared row (provider_name='openai') must NOT be used as fallback."""
    rows = {
        ("llm", "__builder__"): SimpleNamespace(config_json={"active": "openai"}),
        # Legacy shared row — should be ignored
        ("llm", "openai"): SimpleNamespace(
            config_json={
                "provider": "openai",
                "model": "gpt-4o",
                "base_url": "https://api.openai.com/v1",
                "temperature": 0.5,
                "max_tokens": 512,
                "api_key": "sk-legacy",
            }
        ),
    }
    monkeypatch.setattr(config_lib, "_get_provider_row", _fake_get_provider_row(rows))

    llm = await config_lib.get_llm_config(db=object(), provider_name="__builder__")
    # Should NOT pick up the legacy row
    default = config_lib.LLMConfig()
    assert llm.provider == default.provider
    assert llm.api_key != "sk-legacy"
