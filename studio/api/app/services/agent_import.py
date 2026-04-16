"""Agent import validation and mapping helpers."""

from __future__ import annotations

from typing import Any

from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.agent_builder.edit_ops import canonicalize_config
from app.agent_builder.graph import validate_graph
from app.api.settings import TTS_PROVIDERS
from app.lib.config import TTSConfig, _normalize_api_key, get_tts_config
from app.models.provider import ProviderConfig
from app.schemas.agent import ImportIssue, ImportValidationResponse

try:
    from voice_engine import (
        check_tts_model_language,
        get_tts_model_catalog,
        validate_javascript,
    )

    _TTS_MODEL_CATALOG: list[dict[str, Any]] = list(get_tts_model_catalog())
except Exception:  # pragma: no cover - fallback when native extension unavailable
    _TTS_MODEL_CATALOG = []

    def check_tts_model_language(provider: str, model_id: str, language: str) -> bool:
        del provider, model_id, language
        return True

    def validate_javascript(script: str) -> list[str]:
        del script
        return []


_MAPPABLE_PATHS: set[str] = {"tts_provider", "tts_model", "voice_id"}


async def validate_import_config(
    db: AsyncSession,
    config: dict[str, Any],
) -> ImportValidationResponse:
    """Validate an imported config for schema and runtime fulfillability."""
    normalized = _normalize_config(config)
    schema_issues = _collect_schema_issues(normalized)

    tts_defaults = await get_tts_config(db)
    provider_rows = await _get_tts_provider_rows(db)
    suggested_mappings = _base_suggested_mappings(tts_defaults, normalized)
    fulfillment_issues = _collect_fulfillment_issues(
        normalized,
        provider_rows,
        tts_defaults,
        suggested_mappings,
    )

    return ImportValidationResponse(
        schema_valid=not any(i.blocking and i.severity == "error" for i in schema_issues),
        schema_issues=schema_issues,
        fulfillable=not any(
            i.blocking and i.severity == "error" for i in fulfillment_issues
        ),
        fulfillment_issues=fulfillment_issues,
        suggested_mappings=suggested_mappings,
        normalized_config=normalized,
    )


def apply_import_mappings(
    config: dict[str, Any],
    mappings: dict[str, str],
) -> dict[str, Any]:
    """Apply top-level import mappings and return a normalized config copy."""
    mapped = dict(config)
    for path, value in mappings.items():
        if path in _MAPPABLE_PATHS and value:
            mapped[path] = value
    return _normalize_config(mapped)


def unresolved_blocking_issues(result: ImportValidationResponse) -> list[ImportIssue]:
    """Return unresolved blocking issues across schema and fulfillability checks."""
    combined = [*result.schema_issues, *result.fulfillment_issues]
    return [issue for issue in combined if issue.blocking and issue.severity == "error"]


def _normalize_config(config: dict[str, Any]) -> dict[str, Any]:
    normalized = canonicalize_config(dict(config))
    normalized["config_schema_version"] = "v3_graph"
    normalized.setdefault("language", "en")
    normalized.setdefault("timezone", "")
    return canonicalize_config(normalized)


def _collect_schema_issues(config: dict[str, Any]) -> list[ImportIssue]:
    issues: list[ImportIssue] = []

    for message in validate_graph(config):
        issues.append(
            ImportIssue(
                source="schema",
                code="graph_validation_error",
                path="config",
                message=message,
                blocking=True,
                mappable=False,
            )
        )

    tools = config.get("tools", {})
    if isinstance(tools, dict):
        for tool_id, tool_def in tools.items():
            if not isinstance(tool_def, dict):
                continue
            script = tool_def.get("script", "")
            if not isinstance(script, str) or not script:
                continue
            for err in validate_javascript(script):
                issues.append(
                    ImportIssue(
                        source="schema",
                        code="tool_script_invalid",
                        path=f"tools.{tool_id}.script",
                        message=err,
                        blocking=True,
                        mappable=False,
                    )
                )

    return issues


async def _get_tts_provider_rows(db: AsyncSession) -> dict[str, ProviderConfig]:
    result = await db.execute(
        select(ProviderConfig).where(ProviderConfig.provider_type == "tts")
    )
    rows = result.scalars().all()
    return {row.provider_name: row for row in rows}


def _base_suggested_mappings(
    defaults: TTSConfig,
    config: dict[str, Any],
) -> dict[str, str]:
    language = str(config.get("language", "en")).split("-")[0].lower()
    default_provider = defaults.provider or ""
    suggested_model = _pick_model(default_provider, language, defaults.model)
    suggested_voice = defaults.voice_id or _pick_voice(default_provider, suggested_model, language)

    out: dict[str, str] = {}
    if default_provider:
        out["tts_provider"] = default_provider
    if suggested_model:
        out["tts_model"] = suggested_model
    if suggested_voice and (
        not suggested_model
        or suggested_voice in _valid_voice_ids(default_provider, suggested_model, language)
    ):
        out["voice_id"] = suggested_voice
    return out


def _collect_fulfillment_issues(
    config: dict[str, Any],
    provider_rows: dict[str, ProviderConfig],
    defaults: TTSConfig,
    suggested_mappings: dict[str, str],
) -> list[ImportIssue]:
    issues: list[ImportIssue] = []

    provider = str(config.get("tts_provider", "") or "").strip()
    model = str(config.get("tts_model", "") or "").strip()
    voice_id = str(config.get("voice_id", "") or "").strip()
    language = str(config.get("language", "en") or "en").split("-")[0].lower()

    known_providers = {option.value for option in TTS_PROVIDERS}
    provider_has_blocking_issue = False
    effective_provider = provider

    if not provider:
        provider_has_blocking_issue = True
        effective_provider = suggested_mappings.get("tts_provider", "")
        issues.append(
            ImportIssue(
                source="fulfillment",
                code="missing_tts_provider",
                path="tts_provider",
                message="Imported config does not define a TTS provider.",
                blocking=True,
                mappable=True,
                suggested_value=suggested_mappings.get("tts_provider"),
            )
        )
    elif provider not in known_providers:
        provider_has_blocking_issue = True
        effective_provider = suggested_mappings.get("tts_provider", "")
        issues.append(
            ImportIssue(
                source="fulfillment",
                code="unknown_tts_provider",
                path="tts_provider",
                message=f"Unknown TTS provider '{provider}'.",
                blocking=True,
                mappable=True,
                suggested_value=suggested_mappings.get("tts_provider"),
            )
        )
    else:
        if _provider_requires_api_key(provider) and not _provider_has_api_key(
            provider_rows.get(provider)
        ):
            provider_has_blocking_issue = True
            effective_provider = suggested_mappings.get("tts_provider", "")
            issues.append(
                ImportIssue(
                    source="fulfillment",
                    code="tts_provider_not_ready",
                    path="tts_provider",
                    message=(
                        f"Provider '{provider}' is missing credentials in workspace settings."
                    ),
                    blocking=True,
                    mappable=True,
                    suggested_value=suggested_mappings.get("tts_provider"),
                )
            )

    if (
        provider_has_blocking_issue
        and effective_provider in known_providers
        and effective_provider != provider
    ):
        remapped_model = _pick_model(effective_provider, language, defaults.model)
        if remapped_model:
            suggested_mappings["tts_model"] = remapped_model
            remapped_voice = defaults.voice_id or _pick_voice(
                effective_provider,
                remapped_model,
                language,
            )
            if remapped_voice:
                suggested_mappings["voice_id"] = remapped_voice

    provider_for_model = effective_provider if provider_has_blocking_issue else provider
    if provider_for_model and provider_for_model in known_providers:
        if not model:
            suggested_model = _pick_model(provider_for_model, language, defaults.model)
            if suggested_model:
                suggested_mappings["tts_model"] = suggested_model
            issues.append(
                ImportIssue(
                    source="fulfillment",
                    code="missing_tts_model",
                    path="tts_model",
                    message="Imported config does not define a TTS model.",
                    blocking=True,
                    mappable=True,
                    suggested_value=suggested_mappings.get("tts_model"),
                )
            )
        elif _find_model(provider_for_model, model) is None:
            suggested_model = _pick_model(provider_for_model, language, defaults.model)
            if suggested_model:
                suggested_mappings["tts_model"] = suggested_model
            issues.append(
                ImportIssue(
                    source="fulfillment",
                    code="invalid_tts_model",
                    path="tts_model",
                    message=(
                        f"Model '{model}' is not available for provider "
                        f"'{provider_for_model}'."
                    ),
                    blocking=True,
                    mappable=True,
                    suggested_value=suggested_mappings.get("tts_model"),
                )
            )
        elif language and not check_tts_model_language(
            provider_for_model,
            model,
            language,
        ):
            suggested_model = _pick_model(provider_for_model, language, defaults.model)
            if suggested_model:
                suggested_mappings["tts_model"] = suggested_model
            issues.append(
                ImportIssue(
                    source="fulfillment",
                    code="tts_model_language_unsupported",
                    path="tts_model",
                    message=(
                        f"Model '{model}' does not support language '{language}'."
                    ),
                    blocking=True,
                    mappable=True,
                    suggested_value=suggested_mappings.get("tts_model"),
                )
            )

    model_for_voice = model
    if _find_model(provider_for_model, model_for_voice) is None:
        model_for_voice = suggested_mappings.get("tts_model", "")

    # Voice validation relies on catalog language_voices. If unavailable,
    # keep this check non-blocking so import still proceeds.
    if provider_for_model and model_for_voice:
        valid_voices = _valid_voice_ids(provider_for_model, model_for_voice, language)
        if valid_voices:
            suggested_voice = ""
            if defaults.voice_id and defaults.voice_id in valid_voices:
                suggested_voice = defaults.voice_id
            elif valid_voices:
                suggested_voice = sorted(valid_voices)[0]
            if suggested_voice:
                suggested_mappings["voice_id"] = suggested_voice
            if not voice_id:
                issues.append(
                    ImportIssue(
                        source="fulfillment",
                        code="missing_voice_id",
                        path="voice_id",
                        message="Imported config does not define a voice ID.",
                        blocking=True,
                        mappable=True,
                        suggested_value=suggested_mappings.get("voice_id"),
                    )
                )
            elif voice_id not in valid_voices:
                issues.append(
                    ImportIssue(
                        source="fulfillment",
                        code="invalid_voice_id",
                        path="voice_id",
                        message=(
                            f"Voice '{voice_id}' is not available for provider "
                            f"'{provider_for_model}' and model '{model_for_voice}'."
                        ),
                        blocking=True,
                        mappable=True,
                        suggested_value=suggested_mappings.get("voice_id"),
                    )
                )
        elif not voice_id:
            issues.append(
                ImportIssue(
                    source="fulfillment",
                    code="voice_catalog_unavailable",
                    path="voice_id",
                    message=(
                        "Unable to validate voice_id for this provider/model from catalog."
                    ),
                    severity="warning",
                    blocking=False,
                    mappable=False,
                )
            )

    return issues


def _provider_requires_api_key(provider: str) -> bool:
    for option in TTS_PROVIDERS:
        if option.value == provider:
            return any(field.key == "api_key" for field in option.fields)
    return False


def _provider_has_api_key(row: ProviderConfig | None) -> bool:
    if row is None:
        return False
    cfg = row.config_json or {}
    if cfg.get("api_key_encrypted"):
        return True
    return bool(_normalize_api_key(cfg.get("api_key", "")))


def _find_model(provider: str, model_id: str) -> dict[str, Any] | None:
    for entry in _TTS_MODEL_CATALOG:
        if entry.get("provider") == provider and entry.get("model_id") == model_id:
            return entry
    return None


def _pick_model(provider: str, language: str, preferred_model: str = "") -> str:
    if not provider:
        return ""

    preferred = _find_model(provider, preferred_model) if preferred_model else None
    if preferred is not None and language in preferred.get("supported_languages", []):
        return preferred_model

    candidates = [e for e in _TTS_MODEL_CATALOG if e.get("provider") == provider]
    if not candidates:
        return ""

    for entry in candidates:
        if language in entry.get("supported_languages", []):
            value = str(entry.get("model_id", ""))
            if value:
                return value

    fallback = str(candidates[0].get("model_id", ""))
    return fallback


def _pick_voice(provider: str, model: str, language: str) -> str:
    model_entry = _find_model(provider, model)
    if model_entry is None:
        return ""

    lang_voices = model_entry.get("language_voices", [])
    if isinstance(lang_voices, list):
        for entry in lang_voices:
            if not isinstance(entry, dict):
                continue
            if entry.get("language_code") == language and entry.get("voice_id"):
                return str(entry["voice_id"])
        for entry in lang_voices:
            if not isinstance(entry, dict):
                continue
            if entry.get("voice_id"):
                return str(entry["voice_id"])
    return ""


def _valid_voice_ids(provider: str, model: str, language: str) -> set[str]:
    model_entry = _find_model(provider, model)
    if model_entry is None:
        return set()

    out: set[str] = set()
    lang_voices = model_entry.get("language_voices", [])
    if not isinstance(lang_voices, list):
        return out

    for entry in lang_voices:
        if not isinstance(entry, dict):
            continue
        voice = entry.get("voice_id")
        if not isinstance(voice, str) or not voice:
            continue
        entry_lang = str(entry.get("language_code", "")).lower()
        if not entry_lang or entry_lang == language:
            out.add(voice)

    if out:
        return out

    # If language-filtered result is empty, allow any voice from this model.
    for entry in lang_voices:
        if isinstance(entry, dict):
            voice = entry.get("voice_id")
            if isinstance(voice, str) and voice:
                out.add(voice)
    return out
