from __future__ import annotations

import pytest

from app.lib.config import TTSConfig
from app.models.provider import ProviderConfig
from app.services import agent_import


def _minimal_graph(**overrides: object) -> dict[str, object]:
    cfg: dict[str, object] = {
        "entry": "main",
        "nodes": {
            "main": {
                "system_prompt": "You are helpful.",
                "tools": [],
                "edges": [],
            }
        },
        "tools": {},
        "language": "en",
        "tts_provider": "cartesia-ws",
        "tts_model": "sonic-2",
        "voice_id": "en_voice_1",
    }
    cfg.update(overrides)
    return cfg


@pytest.fixture
def tts_catalog(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        agent_import,
        "_TTS_MODEL_CATALOG",
        [
            {
                "provider": "cartesia-ws",
                "model_id": "sonic-2",
                "supported_languages": ["en", "es"],
                "language_voices": [
                    {
                        "language_code": "en",
                        "voice_id": "en_voice_1",
                        "voice_label": "English One",
                    },
                    {
                        "language_code": "es",
                        "voice_id": "es_voice_1",
                        "voice_label": "Spanish One",
                    },
                ],
            },
            {
                "provider": "elevenlabs",
                "model_id": "eleven_turbo_v2_5",
                "supported_languages": ["en", "es"],
                "language_voices": [
                    {
                        "language_code": "en",
                        "voice_id": "el_voice_1",
                        "voice_label": "Eleven English",
                    }
                ],
            },
            {
                "provider": "deepgram",
                "model_id": "aura-2-thalia-en",
                "supported_languages": ["en"],
                "language_voices": [
                    {
                        "language_code": "en",
                        "voice_id": "deep_voice_1",
                        "voice_label": "Deepgram One",
                    }
                ],
            },
        ],
    )


@pytest.fixture
def import_defaults(monkeypatch: pytest.MonkeyPatch) -> None:
    async def _fake_get_tts_config(_db: object) -> TTSConfig:
        return TTSConfig(
            provider="cartesia-ws",
            model="sonic-2",
            voice_id="en_voice_1",
            base_url="",
            api_key="",
        )

    async def _fake_rows(_db: object) -> dict[str, ProviderConfig]:
        return {
            "cartesia-ws": ProviderConfig(
                provider_type="tts",
                provider_name="cartesia-ws",
                display_name="Cartesia",
                config_json={"api_key": "sk-test"},
                is_default=False,
            )
        }

    monkeypatch.setattr(agent_import, "get_tts_config", _fake_get_tts_config)
    monkeypatch.setattr(agent_import, "_get_tts_provider_rows", _fake_rows)
    monkeypatch.setattr(agent_import, "validate_javascript", lambda _script: [])
    monkeypatch.setattr(
        agent_import,
        "check_tts_model_language",
        lambda _provider, _model, _language: True,
    )


@pytest.mark.asyncio
async def test_validate_import_valid_path(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    result = await agent_import.validate_import_config(
        db=object(), config=_minimal_graph()
    )

    assert result.schema_valid is True
    assert result.fulfillable is True
    assert result.schema_issues == []
    assert result.fulfillment_issues == []
    assert result.normalized_config["config_schema_version"] == "v3_graph"


@pytest.mark.asyncio
async def test_validate_import_schema_failure_has_detailed_issues(
    monkeypatch: pytest.MonkeyPatch,
    tts_catalog: None,
    import_defaults: None,
) -> None:
    monkeypatch.setattr(
        agent_import,
        "validate_javascript",
        lambda _script: ["Unexpected token"],
    )
    bad = {
        "nodes": {
            "main": {
                "system_prompt": "",
                "tools": ["missing_tool"],
                "edges": ["missing_node"],
            }
        },
        "tools": {"bad_tool": {"description": "x", "script": "broken("}},
    }

    result = await agent_import.validate_import_config(db=object(), config=bad)

    assert result.schema_valid is False
    codes = {issue.code for issue in result.schema_issues}
    assert "graph_validation_error" in codes
    assert "tool_script_invalid" in codes
    assert any(issue.path == "tools.bad_tool.script" for issue in result.schema_issues)


@pytest.mark.asyncio
async def test_validate_import_unknown_provider_is_mappable(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(tts_provider="does-not-exist"),
    )

    issue = next(
        i for i in result.fulfillment_issues if i.code == "unknown_tts_provider"
    )
    assert issue.mappable is True
    assert issue.suggested_value == "cartesia-ws"


@pytest.mark.asyncio
async def test_validate_import_invalid_model_for_provider(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(tts_model="nonexistent"),
    )

    issue = next(i for i in result.fulfillment_issues if i.code == "invalid_tts_model")
    assert issue.path == "tts_model"
    assert issue.mappable is True


@pytest.mark.asyncio
async def test_validate_import_model_language_mismatch(
    monkeypatch: pytest.MonkeyPatch,
    tts_catalog: None,
    import_defaults: None,
) -> None:
    monkeypatch.setattr(
        agent_import,
        "check_tts_model_language",
        lambda _provider, _model, _language: False,
    )

    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(language="es"),
    )

    issue = next(
        i
        for i in result.fulfillment_issues
        if i.code == "tts_model_language_unsupported"
    )
    assert issue.path == "tts_model"


@pytest.mark.asyncio
async def test_validate_import_invalid_voice_id(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(voice_id="unknown_voice"),
    )

    issue = next(i for i in result.fulfillment_issues if i.code == "invalid_voice_id")
    assert issue.path == "voice_id"
    assert issue.mappable is True


@pytest.mark.asyncio
async def test_strict_mode_has_unresolved_blocking_issues(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(tts_model="bad-model"),
    )

    blocking = agent_import.unresolved_blocking_issues(result)
    assert any(issue.code == "invalid_tts_model" for issue in blocking)


@pytest.mark.asyncio
async def test_map_defaults_can_resolve_unfulfillable_config(
    tts_catalog: None,
    import_defaults: None,
) -> None:
    original = _minimal_graph(
        tts_provider="missing-provider",
        tts_model="wrong-model",
        voice_id="wrong-voice",
    )
    preview = await agent_import.validate_import_config(db=object(), config=original)

    mapped = agent_import.apply_import_mappings(
        preview.normalized_config,
        preview.suggested_mappings,
    )
    final = await agent_import.validate_import_config(db=object(), config=mapped)

    assert final.fulfillable is True
    assert agent_import.unresolved_blocking_issues(final) == []


@pytest.mark.asyncio
async def test_provider_remap_recomputes_model_for_target_provider(
    monkeypatch: pytest.MonkeyPatch,
    tts_catalog: None,
) -> None:
    async def _fake_get_tts_config(_db: object) -> TTSConfig:
        return TTSConfig(
            provider="deepgram",
            model="aura-2-thalia-en",
            voice_id="deep_voice_1",
            base_url="",
            api_key="",
        )

    async def _fake_rows(_db: object) -> dict[str, ProviderConfig]:
        return {
            "cartesia-ws": ProviderConfig(
                provider_type="tts",
                provider_name="cartesia-ws",
                display_name="Cartesia",
                config_json={},  # not ready (no api key)
                is_default=False,
            ),
            "deepgram": ProviderConfig(
                provider_type="tts",
                provider_name="deepgram",
                display_name="Deepgram",
                config_json={"api_key": "dg-key"},
                is_default=False,
            ),
        }

    monkeypatch.setattr(agent_import, "get_tts_config", _fake_get_tts_config)
    monkeypatch.setattr(agent_import, "_get_tts_provider_rows", _fake_rows)
    monkeypatch.setattr(agent_import, "validate_javascript", lambda _script: [])
    monkeypatch.setattr(
        agent_import,
        "check_tts_model_language",
        lambda _provider, _model, _language: True,
    )

    # Provider is not ready (cartesia-ws), model already belongs to deepgram.
    # Suggested model must stay deepgram-compatible after provider remap.
    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(
            tts_provider="cartesia-ws",
            tts_model="aura-2-thalia-en",
            voice_id="deep_voice_1",
        ),
    )

    assert result.suggested_mappings["tts_provider"] == "deepgram"
    assert result.suggested_mappings["tts_model"] == "aura-2-thalia-en"


@pytest.mark.asyncio
async def test_invalid_voice_suggests_provider_compatible_voice_not_default(
    monkeypatch: pytest.MonkeyPatch,
    tts_catalog: None,
) -> None:
    async def _fake_get_tts_config(_db: object) -> TTSConfig:
        return TTSConfig(
            provider="deepgram",
            model="aura-2-thalia-en",
            voice_id="default",  # invalid for this provider/model in test catalog
            base_url="",
            api_key="",
        )

    async def _fake_rows(_db: object) -> dict[str, ProviderConfig]:
        return {
            "deepgram": ProviderConfig(
                provider_type="tts",
                provider_name="deepgram",
                display_name="Deepgram",
                config_json={"api_key": "dg-key"},
                is_default=False,
            )
        }

    monkeypatch.setattr(agent_import, "get_tts_config", _fake_get_tts_config)
    monkeypatch.setattr(agent_import, "_get_tts_provider_rows", _fake_rows)
    monkeypatch.setattr(agent_import, "validate_javascript", lambda _script: [])
    monkeypatch.setattr(
        agent_import,
        "check_tts_model_language",
        lambda _provider, _model, _language: True,
    )

    result = await agent_import.validate_import_config(
        db=object(),
        config=_minimal_graph(
            tts_provider="deepgram",
            tts_model="aura-2-thalia-en",
            voice_id="bad-voice",
        ),
    )

    issue = next(i for i in result.fulfillment_issues if i.code == "invalid_voice_id")
    assert issue.suggested_value == "deep_voice_1"
    assert result.suggested_mappings.get("voice_id") == "deep_voice_1"


def test_apply_import_mappings_only_applies_supported_paths() -> None:
    mapped = agent_import.apply_import_mappings(
        config={"entry": "main"},
        mappings={
            "tts_provider": "cartesia-ws",
            "tts_model": "sonic-2",
            "voice_id": "en_voice_1",
            "not_supported": "ignored",
        },
    )

    assert mapped["tts_provider"] == "cartesia-ws"
    assert mapped["tts_model"] == "sonic-2"
    assert mapped["voice_id"] == "en_voice_1"
    assert "not_supported" not in mapped
