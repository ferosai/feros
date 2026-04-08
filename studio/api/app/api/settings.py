"""Settings API — configure the builder LLM, voice STT/TTS, and other workspace settings.

The database (provider_configs table) is the single source of truth for
all provider settings. If no DB row exists for a given provider type,
sensible defaults are returned.
"""

from __future__ import annotations

from typing import Any

import httpx
from fastapi import APIRouter, Depends, HTTPException
from loguru import logger
from pydantic import BaseModel, Field, field_validator
from pydantic_ai.exceptions import UserError as PydanticAIUserError
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

import integrations
from app.agent_builder import builder_service
from app.lib import get_settings
from app.lib.config import (
    _LLM_ROLE_PREFIX,
    LLMConfig,
    STTConfig,
    TelephonyConfig,
    TTSConfig,
    _normalize_api_key,
    get_llm_config,
    llm_config_from_row,
    stt_config_from_row,
    telephony_config_from_row,
    tts_config_from_row,
)
from app.lib.database import get_db
from app.models.credential import CURRENT_ENCRYPTION_VERSION
from app.models.provider import ProviderConfig

try:
    from voice_engine import get_stt_model_catalog, get_tts_model_catalog
except Exception:  # Rust extension may not be built in CI / test envs

    def get_tts_model_catalog() -> list[dict[str, Any]]:
        return []

    def get_stt_model_catalog() -> list[dict[str, Any]]:
        return []


router = APIRouter(prefix="/settings", tags=["settings"])


class TtsModelEntry(BaseModel):
    provider: str
    model_id: str
    label: str
    supported_languages: list[str]


class SttModelEntry(BaseModel):
    provider: str
    model_id: str
    label: str
    supported_languages: list[str]


# ══════════════════════════════════════════════════════════════════
# LLM Schemas
# ══════════════════════════════════════════════════════════════════


class LLMSettingsResponse(BaseModel):
    """What the frontend receives — never includes the raw API key."""

    provider: str
    model: str
    base_url: str
    has_api_key: bool  # True if an API key is configured (but we never return it)
    temperature: float
    max_tokens: int

    # Helpful metadata for the frontend dropdown
    supported_providers: list[dict[str, str]] = Field(
        default_factory=lambda: [
            {
                "value": "groq",
                "label": "Groq",
                "description": "Ultra-fast inference — Llama, Qwen, DeepSeek, GPT-OSS",
            },
            {
                "value": "openai",
                "label": "OpenAI",
                "description": "GPT-4o, GPT-4o-mini, and more",
            },
            {
                "value": "anthropic",
                "label": "Anthropic",
                "description": "Claude 3.5 Sonnet, Claude 3 Opus, and more",
            },
            {
                "value": "gemini",
                "label": "Google Gemini",
                "description": "Gemini 2.0 Flash, Gemini 1.5 Pro, and more",
            },
            {
                "value": "deepseek",
                "label": "DeepSeek",
                "description": "DeepSeek-V3, DeepSeek-R1 — reasoning models",
            },
            {
                "value": "openrouter",
                "label": "OpenRouter",
                "description": "Access 100+ models with one API key",
            },
            {
                "value": "ollama",
                "label": "Ollama (Local)",
                "description": "Run models locally — no API key needed",
            },
            {
                "value": "together",
                "label": "Together AI",
                "description": "Llama 3.1, Mixtral, and more — fast inference",
            },
            {
                "value": "fireworks",
                "label": "Fireworks AI",
                "description": "Fast inference for open-source models",
            },
            {
                "value": "vllm",
                "label": "vLLM (Self-hosted)",
                "description": "High-performance self-hosted inference",
            },
            {
                "value": "custom",
                "label": "Custom (OpenAI-compatible)",
                "description": "Any endpoint that speaks the OpenAI API",
            },
        ]
    )
    # Map of provider slug → has_api_key for providers that have saved credentials.
    all_credentials: dict[str, bool] = Field(default_factory=dict)
    # Non-secret config per slug (model, base_url, …) for pre-populating
    # fields when the user switches back to a previously saved provider.
    all_configs: dict[str, dict[str, str]] = Field(default_factory=dict)


class LLMSettingsUpdate(BaseModel):
    """What the frontend sends when updating LLM settings."""

    provider: str = Field(
        ...,
        pattern="^(groq|openai|anthropic|gemini|deepseek|ollama|together|fireworks|openrouter|vllm|custom)$",
    )
    model: str
    base_url: str = ""
    api_key: str = ""
    temperature: float = 0.7
    max_tokens: int = 256


# ══════════════════════════════════════════════════════════════════
# Voice Provider Schemas (generic for STT and TTS)
# ══════════════════════════════════════════════════════════════════


class VoiceProviderField(BaseModel):
    """A single configurable field for a voice provider."""

    key: str
    label: str
    placeholder: str = ""
    is_secret: bool = False  # True → encrypted at rest, shown as password input


class VoiceProviderOption(BaseModel):
    """A selectable voice provider with its configurable fields."""

    value: str
    label: str
    description: str
    default_base_url: str = ""
    docs_url: str = ""
    fields: list[VoiceProviderField] = Field(default_factory=list)


class VoiceProviderSettings(BaseModel):
    """Generic voice provider settings (used for both STT and TTS)."""

    provider: str
    base_url: str
    has_api_key: bool = False  # True when an API key is stored (never returned)
    config: dict[str, str] = Field(default_factory=dict)
    supported_providers: list[VoiceProviderOption] = Field(default_factory=list)
    # Map of provider slug → has_api_key for providers that have saved credentials.
    # Lets the UI show a "Saved" badge for any provider the user has already configured.
    all_credentials: dict[str, bool] = Field(default_factory=dict)
    # Non-secret config per slug (model, voice_id, base_url, …) for pre-populating
    # fields when the user switches back to a previously saved provider.
    all_configs: dict[str, dict[str, str]] = Field(default_factory=dict)


class VoiceProviderUpdate(BaseModel):
    """Update a voice provider (generic for STT/TTS)."""

    provider: str
    base_url: str
    config: dict[str, str] = Field(default_factory=dict)


# ── Provider field definitions ───────────────────────────────────

_API_KEY_FIELD = VoiceProviderField(
    key="api_key",
    label="API Key",
    placeholder="",
    is_secret=True,
)

STT_PROVIDERS: list[VoiceProviderOption] = [
    # ── Self-hosted ────────────────────────────────────────────────
    VoiceProviderOption(
        value="faster-whisper",
        label="Faster Whisper",
        description="Self-hosted STT via faster-whisper-server",
        default_base_url="http://localhost:8100",
        fields=[],
    ),
    # ── Commercial streaming ────────────────────────────────────────
    VoiceProviderOption(
        value="deepgram",
        label="Deepgram",
        description="Ultra-low-latency streaming WS STT — Nova-3",
        docs_url="https://console.deepgram.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="nova-3-general"
            ),
        ],
    ),
    VoiceProviderOption(
        value="cartesia",
        label="Cartesia",
        description="Ink Whisper streaming WS STT",
        docs_url="https://play.cartesia.ai",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="ink-whisper"),
        ],
    ),
    VoiceProviderOption(
        value="openai-realtime",
        label="OpenAI Realtime",
        description="GPT-4o Realtime streaming WS STT",
        docs_url="https://platform.openai.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="gpt-4o-transcribe"
            ),
        ],
    ),
    VoiceProviderOption(
        value="elevenlabs",
        label="ElevenLabs",
        description="Scribe v2 — high-accuracy segmented HTTP STT",
        docs_url="https://elevenlabs.io",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="scribe_v2"),
        ],
    ),
    VoiceProviderOption(
        value="groq",
        label="Groq",
        description="Whisper Large v3 — segmented HTTP transcription",
        docs_url="https://console.groq.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="whisper-large-v3-turbo"
            ),
        ],
    ),
    VoiceProviderOption(
        value="openai-whisper",
        label="OpenAI Whisper",
        description="Whisper via OpenAI API — segmented HTTP",
        docs_url="https://platform.openai.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="gpt-4o-transcribe"
            ),
        ],
    ),
]

TTS_PROVIDERS: list[VoiceProviderOption] = [
    # ── Self-hosted ────────────────────────────────────────────────
    VoiceProviderOption(
        value="fish-speech",
        label="Fish Speech",
        description="High-quality TTS with voice cloning",
        default_base_url="http://localhost:8200",
        fields=[],
    ),
    # ── Commercial WebSocket (lowest latency) ───────────────────────
    VoiceProviderOption(
        value="cartesia-ws",
        label="Cartesia (WS)",
        description="Cartesia streaming WS — lowest latency",
        docs_url="https://play.cartesia.ai",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="sonic-english"),
            VoiceProviderField(
                key="voice_id",
                label="Voice ID",
                placeholder="a0e99841-438c-4a64-b679-ae501e7d6091",
            ),
        ],
    ),
    VoiceProviderOption(
        value="elevenlabs-ws",
        label="ElevenLabs (WS)",
        description="ElevenLabs multi-stream WS — token streaming",
        docs_url="https://elevenlabs.io",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="eleven_turbo_v2_5"
            ),
            VoiceProviderField(
                key="voice_id", label="Voice ID", placeholder="21m00Tcm4TlvDq8ikWAM"
            ),
        ],
    ),
    VoiceProviderOption(
        value="deepgram-ws",
        label="Deepgram (WS)",
        description="Deepgram Aura WS — binary PCM streaming",
        docs_url="https://console.deepgram.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="aura-2-en-us"),
        ],
    ),
    # ── Commercial HTTP ─────────────────────────────────────────────
    VoiceProviderOption(
        value="cartesia",
        label="Cartesia (HTTP)",
        description="Cartesia REST API",
        docs_url="https://play.cartesia.ai",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="sonic-english"),
            VoiceProviderField(
                key="voice_id",
                label="Voice ID",
                placeholder="a0e99841-438c-4a64-b679-ae501e7d6091",
            ),
        ],
    ),
    VoiceProviderOption(
        value="elevenlabs",
        label="ElevenLabs (HTTP)",
        description="ElevenLabs REST API",
        docs_url="https://elevenlabs.io",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(
                key="model", label="Model", placeholder="eleven_turbo_v2_5"
            ),
            VoiceProviderField(
                key="voice_id", label="Voice ID", placeholder="21m00Tcm4TlvDq8ikWAM"
            ),
        ],
    ),
    VoiceProviderOption(
        value="openai",
        label="OpenAI TTS",
        description="OpenAI TTS REST — tts-1, tts-1-hd",
        docs_url="https://platform.openai.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="tts-1"),
            VoiceProviderField(key="voice_id", label="Voice", placeholder="alloy"),
        ],
    ),
    VoiceProviderOption(
        value="deepgram",
        label="Deepgram (HTTP)",
        description="Deepgram Aura REST API",
        docs_url="https://console.deepgram.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="aura-2-en-us"),
        ],
    ),
    VoiceProviderOption(
        value="groq",
        label="Groq TTS",
        description="Groq PlayAI TTS — fast inference",
        docs_url="https://console.groq.com",
        fields=[
            _API_KEY_FIELD,
            VoiceProviderField(key="model", label="Model", placeholder="playai-tts"),
            VoiceProviderField(
                key="voice_id", label="Voice ID", placeholder="Fritz-PlayAI"
            ),
        ],
    ),
]


# ══════════════════════════════════════════════════════════════════
# Helpers
# ══════════════════════════════════════════════════════════════════


async def _get_provider_row(
    db: AsyncSession,
    provider_type: str,
    provider_name: str = "__builder__",
) -> ProviderConfig | None:
    """Get a provider_configs row from the DB, if any."""
    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == provider_type,
            ProviderConfig.provider_name == provider_name,
        )
    )
    return result.scalar_one_or_none()


async def _get_provider_creds_row(
    db: AsyncSession,
    provider_type: str,
    provider_slug: str,
) -> ProviderConfig | None:
    """Get the credential row for a specific provider slug (e.g. ``"elevenlabs-ws"``).

    Credential rows use ``provider_name = provider_slug`` directly (not the
    ``__voice__`` / ``__builder__`` sentinel).
    """
    return await _get_provider_row(db, provider_type, provider_slug)


async def _upsert_provider_creds_row(
    db: AsyncSession,
    provider_type: str,
    provider_slug: str,
    config_json: dict[str, Any],
) -> ProviderConfig:
    """Upsert the per-slug credential row, then return it.

    This stores the API key, model, base_url, voice_id etc. for a specific
    provider. The active-selection pointer (``__voice__`` / ``__builder__``)
    is updated separately via :func:`_set_active_provider`.
    """
    row = await _get_provider_creds_row(db, provider_type, provider_slug)
    if row:
        row.config_json = config_json
        row.display_name = f"{provider_type.upper()} creds: {provider_slug}"
    else:
        row = ProviderConfig(
            provider_type=provider_type,
            provider_name=provider_slug,
            display_name=f"{provider_type.upper()} creds: {provider_slug}",
            config_json=config_json,
            is_default=False,
        )
        db.add(row)
    await db.flush()
    return row


async def _set_active_provider(
    db: AsyncSession,
    provider_type: str,
    role: str,
    active_slug: str,
) -> None:
    """Update the active-provider pointer row for ``role``.

    The pointer row uses ``provider_name = role`` (e.g. ``"__voice__"``) and
    stores only ``{"active": active_slug}`` in ``config_json``.  All actual
    credentials live in the per-slug row.
    """
    row = await _get_provider_row(db, provider_type, role)
    if row:
        row.config_json = {"active": active_slug}
    else:
        row = ProviderConfig(
            provider_type=provider_type,
            provider_name=role,
            display_name=f"{provider_type.upper()} active ({role})",
            config_json={"active": active_slug},
            is_default=True,
        )
        db.add(row)
    await db.flush()


async def _get_all_credentials(
    db: AsyncSession,
    provider_type: str,
    provider_slugs: list[str],
) -> dict[str, bool]:
    """Return a map of provider slug → has_api_key for callers that care.

    Only checks slugs that are known provider values so arbitrary DB rows
    are not surfaced.
    """
    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == provider_type,
            ProviderConfig.provider_name.in_(provider_slugs),
        )
    )
    rows = result.scalars().all()
    out: dict[str, bool] = {}
    for row in rows:
        cfg = row.config_json or {}
        has_key = bool(cfg.get("api_key_encrypted") or cfg.get("api_key"))
        out[row.provider_name] = has_key
    return out


# Fields that must never be surfaced to the frontend (encrypted secrets)
_SECRET_FIELDS = frozenset({"api_key", "api_key_encrypted", "active", "provider"})


async def _get_all_provider_configs(
    db: AsyncSession,
    provider_type: str,
    provider_slugs: list[str],
) -> dict[str, dict[str, str]]:
    """Return non-secret config (model, voice_id, base_url …) per provider slug.

    Used by the frontend to pre-populate form fields when the user switches
    back to a previously-saved provider without having to save first.
    """
    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == provider_type,
            ProviderConfig.provider_name.in_(provider_slugs),
        )
    )
    rows = result.scalars().all()
    out: dict[str, dict[str, str]] = {}
    for row in rows:
        cfg = row.config_json or {}
        out[row.provider_name] = {
            k: str(v)
            for k, v in cfg.items()
            if k not in _SECRET_FIELDS and v is not None
        }
    return out


def _resolve_active_config(
    pointer_row: ProviderConfig | None,
    creds_row: ProviderConfig | None,
) -> ProviderConfig | None:
    """Merge the active-selection pointer row with the per-slug creds row.

    Returns a synthetic ProviderConfig whose ``config_json`` contains both
    the active provider slug and all credentials, for use with the existing
    ``*_config_from_row()`` helpers which expect a single row.

    Falls back gracefully: if there is no separate creds row the pointer
    row’s own ``config_json`` is used (legacy single-row behaviour).
    """
    if pointer_row is None:
        return None
    if creds_row is None:
        # Legacy: the pointer row itself contains everything
        return pointer_row
    # Merge: creds win for credential fields; pointer supplies the active slug
    merged = {
        **creds_row.config_json,
        "active": pointer_row.config_json.get("active", ""),
    }
    # Create a transient (not DB-tracked) object so the ORM doesn’t try to persist it
    synthetic = ProviderConfig(
        id=pointer_row.id,
        provider_type=pointer_row.provider_type,
        provider_name=pointer_row.provider_name,
        display_name=pointer_row.display_name,
        config_json=merged,
        is_default=pointer_row.is_default,
    )
    return synthetic


# ══════════════════════════════════════════════════════════════════
# LLM role-scoped key helpers
# ══════════════════════════════════════════════════════════════════


def _llm_scoped_name(role: str, provider_slug: str) -> str:
    """Derive a role-scoped provider_name, e.g. ``builder::openai``."""
    return f"{_LLM_ROLE_PREFIX[role]}::{provider_slug}"


async def _get_llm_creds_row(
    db: AsyncSession,
    role: str,
    provider_slug: str,
) -> ProviderConfig | None:
    """Get the scoped LLM credential row for *role* + *provider_slug*."""
    return await _get_provider_row(db, "llm", _llm_scoped_name(role, provider_slug))


async def _upsert_llm_creds_row(
    db: AsyncSession,
    role: str,
    provider_slug: str,
    config_json: dict[str, Any],
) -> ProviderConfig:
    """Upsert a role-scoped LLM credential row, e.g. ``builder::openai``."""
    scoped = _llm_scoped_name(role, provider_slug)
    row = await _get_provider_row(db, "llm", scoped)
    if row:
        row.config_json = config_json
        row.display_name = f"LLM creds: {scoped}"
    else:
        row = ProviderConfig(
            provider_type="llm",
            provider_name=scoped,
            display_name=f"LLM creds: {scoped}",
            config_json=config_json,
            is_default=False,
        )
        db.add(row)
    await db.flush()
    return row


async def _get_all_llm_credentials(
    db: AsyncSession,
    role: str,
    provider_slugs: list[str],
) -> dict[str, bool]:
    """Like ``_get_all_credentials`` but queries role-scoped LLM rows.

    Returns ``{provider_slug: has_api_key}`` with the prefix stripped.
    """
    scoped_names = [_llm_scoped_name(role, s) for s in provider_slugs]
    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == "llm",
            ProviderConfig.provider_name.in_(scoped_names),
        )
    )
    prefix = _LLM_ROLE_PREFIX[role] + "::"
    out: dict[str, bool] = {}
    for row in result.scalars().all():
        cfg = row.config_json or {}
        slug = row.provider_name.removeprefix(prefix)
        out[slug] = bool(cfg.get("api_key_encrypted") or cfg.get("api_key"))
    return out


async def _get_all_llm_provider_configs(
    db: AsyncSession,
    role: str,
    provider_slugs: list[str],
) -> dict[str, dict[str, str]]:
    """Like ``_get_all_provider_configs`` but queries role-scoped LLM rows.

    Returns ``{provider_slug: {model, base_url, …}}`` with the prefix stripped.
    """
    scoped_names = [_llm_scoped_name(role, s) for s in provider_slugs]
    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == "llm",
            ProviderConfig.provider_name.in_(scoped_names),
        )
    )
    prefix = _LLM_ROLE_PREFIX[role] + "::"
    out: dict[str, dict[str, str]] = {}
    for row in result.scalars().all():
        cfg = row.config_json or {}
        slug = row.provider_name.removeprefix(prefix)
        out[slug] = {
            k: str(v)
            for k, v in cfg.items()
            if k not in _SECRET_FIELDS and v is not None
        }
    return out


def _llm_response(
    cfg: LLMConfig,
    all_credentials: dict[str, bool] | None = None,
    all_configs: dict[str, dict[str, str]] | None = None,
) -> LLMSettingsResponse:
    """Build an LLMSettingsResponse from an LLMConfig."""
    return LLMSettingsResponse(
        provider=cfg.provider,
        model=cfg.model,
        base_url=cfg.base_url,
        has_api_key=bool(cfg.api_key),
        temperature=cfg.temperature,
        max_tokens=cfg.max_tokens,
        all_credentials=all_credentials or {},
        all_configs=all_configs or {},
    )


# ══════════════════════════════════════════════════════════════════
# LLM Endpoints
# ══════════════════════════════════════════════════════════════════


_LLM_PROVIDER_SLUGS = [
    "groq",
    "openai",
    "anthropic",
    "gemini",
    "deepseek",
    "openrouter",
    "ollama",
    "together",
    "fireworks",
    "vllm",
    "custom",
]


@router.get("/llm", response_model=LLMSettingsResponse)
async def get_llm_settings(
    db: AsyncSession = Depends(get_db),
) -> LLMSettingsResponse:
    """Return the current builder LLM configuration from the DB."""
    cfg = await get_llm_config(db, "__builder__")
    all_creds = await _get_all_llm_credentials(db, "__builder__", _LLM_PROVIDER_SLUGS)
    all_cfgs = await _get_all_llm_provider_configs(
        db, "__builder__", _LLM_PROVIDER_SLUGS
    )
    return _llm_response(cfg, all_creds, all_cfgs)


@router.put("/llm", response_model=LLMSettingsResponse)
async def update_llm_settings(
    body: LLMSettingsUpdate,
    db: AsyncSession = Depends(get_db),
) -> LLMSettingsResponse:
    """Update the LLM provider used by the builder.

    Credentials are stored in role-scoped rows (``builder::<slug>``).
    Switching providers preserves previously saved API keys.
    Hot-swaps the builder's model in memory.
    """
    config_json: dict[str, Any] = {
        "provider": body.provider,
        "model": body.model,
        "base_url": body.base_url,
        "temperature": body.temperature,
        "max_tokens": body.max_tokens,
    }
    api_key = _normalize_api_key(body.api_key)
    existing_creds = await _get_llm_creds_row(db, "__builder__", body.provider)
    if api_key:
        engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
        _ct, _iv = engine.encrypt({"value": api_key})
        config_json["api_key_encrypted"] = {
            "ciphertext": _ct,
            "iv": _iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
    elif existing_creds:
        for key in ("api_key_encrypted", "api_key"):
            if key in (existing_creds.config_json or {}):
                config_json[key] = existing_creds.config_json[key]

    creds_row = await _upsert_llm_creds_row(
        db, "__builder__", body.provider, config_json
    )
    await _set_active_provider(db, "llm", "__builder__", body.provider)

    llm_config = llm_config_from_row(creds_row)
    try:
        builder_service.reconfigure(llm_config)
    except PydanticAIUserError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    all_creds = await _get_all_llm_credentials(db, "__builder__", _LLM_PROVIDER_SLUGS)
    all_cfgs = await _get_all_llm_provider_configs(
        db, "__builder__", _LLM_PROVIDER_SLUGS
    )
    return _llm_response(llm_config, all_creds, all_cfgs)


async def get_builder_llm_config(db: AsyncSession) -> LLMConfig:
    """Load the configured builder LLM from the DB.

    Exported so other modules (e.g. agents.py) can resolve the correct LLM
    without importing private helper functions from this module.
    """
    return await get_llm_config(db, "__builder__")


# ══════════════════════════════════════════════════════════════════
# Voice Agent LLM Endpoints
# ══════════════════════════════════════════════════════════════════


@router.get("/voice-llm", response_model=LLMSettingsResponse)
async def get_voice_llm_settings(
    db: AsyncSession = Depends(get_db),
) -> LLMSettingsResponse:
    """Return the current voice agent LLM configuration from the DB."""
    cfg = await get_llm_config(db, "__voice__")
    all_creds = await _get_all_llm_credentials(db, "__voice__", _LLM_PROVIDER_SLUGS)
    all_cfgs = await _get_all_llm_provider_configs(db, "__voice__", _LLM_PROVIDER_SLUGS)
    return _llm_response(cfg, all_creds, all_cfgs)


@router.put("/voice-llm", response_model=LLMSettingsResponse)
async def update_voice_llm_settings(
    body: LLMSettingsUpdate,
    db: AsyncSession = Depends(get_db),
) -> LLMSettingsResponse:
    """Update the LLM provider used by the voice pipeline.

    Credentials are stored in role-scoped rows (``voice::<slug>``).
    Switching providers preserves previously saved API keys.
    """
    config_json: dict[str, Any] = {
        "provider": body.provider,
        "model": body.model,
        "base_url": body.base_url,
        "temperature": body.temperature,
        "max_tokens": body.max_tokens,
    }
    api_key = _normalize_api_key(body.api_key)
    existing_creds = await _get_llm_creds_row(db, "__voice__", body.provider)
    if api_key:
        engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
        _ct, _iv = engine.encrypt({"value": api_key})
        config_json["api_key_encrypted"] = {
            "ciphertext": _ct,
            "iv": _iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
    elif existing_creds:
        for key in ("api_key_encrypted", "api_key"):
            if key in (existing_creds.config_json or {}):
                config_json[key] = existing_creds.config_json[key]

    creds_row = await _upsert_llm_creds_row(db, "__voice__", body.provider, config_json)
    await _set_active_provider(db, "llm", "__voice__", body.provider)

    llm_config = llm_config_from_row(creds_row)
    logger.info(
        "Voice LLM settings updated: provider={}, model={}", body.provider, body.model
    )

    all_creds = await _get_all_llm_credentials(db, "__voice__", _LLM_PROVIDER_SLUGS)
    all_cfgs = await _get_all_llm_provider_configs(db, "__voice__", _LLM_PROVIDER_SLUGS)
    return _llm_response(llm_config, all_creds, all_cfgs)


# ══════════════════════════════════════════════════════════════════
# STT Endpoints
# ══════════════════════════════════════════════════════════════════


def _stt_to_voice_settings(
    cfg: STTConfig,
    row: Any | None = None,
    all_credentials: dict[str, bool] | None = None,
    all_configs: dict[str, dict[str, str]] | None = None,
) -> VoiceProviderSettings:
    """Convert an STTConfig to the generic VoiceProviderSettings."""
    visible_config: dict[str, str] = {}
    if row:
        raw: dict[str, Any] = row.config_json or {}
        for k, v in raw.items():
            if k not in (
                "provider",
                "base_url",
                "api_key",
                "api_key_encrypted",
                "active",
            ) and isinstance(v, str):
                visible_config[k] = v
    has_api_key = bool(
        row
        and (row.config_json.get("api_key_encrypted") or row.config_json.get("api_key"))
    )
    return VoiceProviderSettings(
        provider=cfg.provider,
        base_url=cfg.base_url,
        has_api_key=has_api_key,
        config=visible_config,
        supported_providers=STT_PROVIDERS,
        all_credentials=all_credentials or {},
        all_configs=all_configs or {},
    )


@router.get("/stt", response_model=VoiceProviderSettings)
async def get_stt_settings(
    db: AsyncSession = Depends(get_db),
) -> VoiceProviderSettings:
    """Return the current STT configuration from the DB."""
    pointer_row = await _get_provider_row(db, "stt", "__voice__")
    # Resolve the active provider slug
    if pointer_row:
        active_slug = pointer_row.config_json.get(
            "active"
        ) or pointer_row.config_json.get("provider", "")
    else:
        active_slug = STTConfig().provider
    creds_row = (
        await _get_provider_creds_row(db, "stt", active_slug) if active_slug else None
    )
    merged_row = _resolve_active_config(pointer_row, creds_row)
    cfg = stt_config_from_row(merged_row) if merged_row else STTConfig()

    known_slugs = [p.value for p in STT_PROVIDERS]
    all_creds = await _get_all_credentials(db, "stt", known_slugs)
    all_cfgs = await _get_all_provider_configs(db, "stt", known_slugs)
    return _stt_to_voice_settings(cfg, merged_row, all_creds, all_cfgs)


@router.put("/stt", response_model=VoiceProviderSettings)
async def update_stt_settings(
    body: VoiceProviderUpdate,
    db: AsyncSession = Depends(get_db),
) -> VoiceProviderSettings:
    """Update the STT provider for the voice pipeline.

    Credentials are stored per-provider-slug so switching providers preserves
    previously saved keys.  Only the active-selection pointer is changed when
    the user switches between already-configured providers.
    """
    api_key = (
        _normalize_api_key(body.config.pop("api_key", ""))
        if "api_key" in body.config
        else ""
    )

    config_json: dict[str, Any] = {
        "provider": body.provider,
        "base_url": body.base_url,
        **body.config,
    }

    # Preserve existing API key if none was supplied
    existing_creds = await _get_provider_creds_row(db, "stt", body.provider)
    if api_key:
        engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
        _ct, _iv = engine.encrypt({"value": api_key})
        config_json["api_key_encrypted"] = {
            "ciphertext": _ct,
            "iv": _iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
    elif existing_creds:
        for key in ("api_key_encrypted", "api_key"):
            if key in (existing_creds.config_json or {}):
                config_json[key] = existing_creds.config_json[key]

    creds_row = await _upsert_provider_creds_row(db, "stt", body.provider, config_json)
    await _set_active_provider(db, "stt", "__voice__", body.provider)

    stt_config = stt_config_from_row(creds_row)
    logger.info(
        "STT settings updated: provider={}, base_url={}", body.provider, body.base_url
    )

    known_slugs = [p.value for p in STT_PROVIDERS]
    all_creds = await _get_all_credentials(db, "stt", known_slugs)
    all_cfgs = await _get_all_provider_configs(db, "stt", known_slugs)
    return _stt_to_voice_settings(stt_config, creds_row, all_creds, all_cfgs)


# ══════════════════════════════════════════════════════════════════
# TTS Endpoints
# ══════════════════════════════════════════════════════════════════


def _tts_to_voice_settings(
    cfg: TTSConfig,
    row: Any | None = None,
    all_credentials: dict[str, bool] | None = None,
    all_configs: dict[str, dict[str, str]] | None = None,
) -> VoiceProviderSettings:
    """Convert a TTSConfig to the generic VoiceProviderSettings."""
    visible_config: dict[str, str] = {}
    if row:
        raw: dict[str, Any] = row.config_json or {}
        for k, v in raw.items():
            if k not in (
                "provider",
                "base_url",
                "api_key",
                "api_key_encrypted",
                "active",
            ) and isinstance(v, str):
                visible_config[k] = v
    else:
        if cfg.voice_id:
            visible_config["voice_id"] = cfg.voice_id
    has_api_key = bool(
        row
        and (row.config_json.get("api_key_encrypted") or row.config_json.get("api_key"))
    )
    return VoiceProviderSettings(
        provider=cfg.provider,
        base_url=cfg.base_url,
        has_api_key=has_api_key,
        config=visible_config,
        supported_providers=TTS_PROVIDERS,
        all_credentials=all_credentials or {},
        all_configs=all_configs or {},
    )


@router.get("/tts", response_model=VoiceProviderSettings)
async def get_tts_settings(
    db: AsyncSession = Depends(get_db),
) -> VoiceProviderSettings:
    """Return the current TTS configuration from the DB."""
    pointer_row = await _get_provider_row(db, "tts", "__voice__")
    if pointer_row:
        active_slug = pointer_row.config_json.get(
            "active"
        ) or pointer_row.config_json.get("provider", "")
    else:
        active_slug = TTSConfig().provider
    creds_row = (
        await _get_provider_creds_row(db, "tts", active_slug) if active_slug else None
    )
    merged_row = _resolve_active_config(pointer_row, creds_row)
    cfg = tts_config_from_row(merged_row) if merged_row else TTSConfig()

    known_slugs = [p.value for p in TTS_PROVIDERS]
    all_creds = await _get_all_credentials(db, "tts", known_slugs)
    all_cfgs = await _get_all_provider_configs(db, "tts", known_slugs)
    return _tts_to_voice_settings(cfg, merged_row, all_creds, all_cfgs)


@router.put("/tts", response_model=VoiceProviderSettings)
async def update_tts_settings(
    body: VoiceProviderUpdate,
    db: AsyncSession = Depends(get_db),
) -> VoiceProviderSettings:
    """Update the TTS provider for the voice pipeline.

    Credentials are stored per-provider-slug so switching providers preserves
    previously saved keys.  Only the active-selection pointer is changed when
    the user switches between already-configured providers.
    """
    api_key = (
        _normalize_api_key(body.config.pop("api_key", ""))
        if "api_key" in body.config
        else ""
    )

    config_json: dict[str, Any] = {
        "provider": body.provider,
        "base_url": body.base_url,
        **body.config,
    }

    # Preserve the existing key for this specific provider slug
    existing_creds = await _get_provider_creds_row(db, "tts", body.provider)
    if api_key:
        engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
        _ct, _iv = engine.encrypt({"value": api_key})
        config_json["api_key_encrypted"] = {
            "ciphertext": _ct,
            "iv": _iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
    elif existing_creds:
        for key in ("api_key_encrypted", "api_key"):
            if key in (existing_creds.config_json or {}):
                config_json[key] = existing_creds.config_json[key]

    creds_row = await _upsert_provider_creds_row(db, "tts", body.provider, config_json)
    await _set_active_provider(db, "tts", "__voice__", body.provider)

    tts_config = tts_config_from_row(creds_row)
    logger.info(
        "TTS settings updated: provider={}, base_url={}", body.provider, body.base_url
    )

    known_slugs = [p.value for p in TTS_PROVIDERS]
    all_creds = await _get_all_credentials(db, "tts", known_slugs)
    return _tts_to_voice_settings(tts_config, creds_row, all_creds)


# ── Voice catalog proxy ────────────────────────────────────────────
#
# Fetches the live voice list from the configured TTS provider using the
# stored (decrypted) API key and returns a normalized list that the
# frontend voice-picker combobox consumes.
#
# Provider notes:
#   ElevenLabs — GET /v1/voices returns {voices: [{voice_id, name, labels, preview_url}]}
#   Cartesia    — GET /voices returns [{id, name, language, description}]
#   Deepgram    — Aura voices are static; we return a curated list from TTS_MODEL_CATALOG


@router.get("/tts-catalog", response_model=list[TtsModelEntry])
async def get_tts_catalog(provider: str | None = None) -> list[TtsModelEntry]:
    """Return TTS model catalog, optionally filtered by provider slug.

    Rust TTS_MODEL_CATALOG (language_config.rs) is the canonical source of
    truth. Returns an empty list only when the Rust extension is not built
    (e.g. CI environments without a compiled voice_engine wheel).
    """
    raw: list[dict[str, Any]] = list(get_tts_model_catalog())
    entries = [
        TtsModelEntry(
            provider=m["provider"],
            model_id=m["model_id"],
            label=m["label"],
            supported_languages=list(m["supported_languages"]),
        )
        for m in raw
    ]
    if provider:
        entries = [e for e in entries if e.provider == provider]
    return entries


@router.get("/stt-catalog", response_model=list[SttModelEntry])
async def get_stt_catalog(provider: str | None = None) -> list[SttModelEntry]:
    """Return STT model catalog, optionally filtered by provider slug.

    Rust STT_MODEL_CATALOG (language_config.rs) is the canonical source of
    truth. Returns an empty list only when the Rust extension is not built
    (e.g. CI environments without a compiled voice_engine wheel).
    """
    raw: list[dict[str, Any]] = list(get_stt_model_catalog())
    entries = [
        SttModelEntry(
            provider=m["provider"],
            model_id=m["model_id"],
            label=m["label"],
            supported_languages=list(m["supported_languages"]),
        )
        for m in raw
    ]
    if provider:
        entries = [e for e in entries if e.provider == provider]
    return entries


class VoiceOption(BaseModel):
    """A single voice entry returned by GET /settings/tts/voices."""

    voice_id: str
    name: str
    description: str = ""
    gender: str = ""  # "male" | "female" | "" if unknown
    language: str = ""  # ISO 639-1 hint (not always available)
    preview_url: str = ""


async def _fetch_elevenlabs_voices(api_key: str) -> list[VoiceOption]:
    """Call the ElevenLabs personal voices endpoint and normalise the response."""
    if not api_key:
        logger.info("_fetch_elevenlabs_voices: no API key — skipping")
        return []
    try:
        async with httpx.AsyncClient(timeout=10) as client:
            r = await client.get(
                "https://api.elevenlabs.io/v1/voices",
                headers={"xi-api-key": api_key},
            )
            if not r.is_success:
                logger.warning(
                    "_fetch_elevenlabs_voices: HTTP {} — {}",
                    r.status_code,
                    r.text[:200],
                )
                return []
            data = r.json()
        voices = []
        for v in data.get("voices", []):
            labels: dict[str, Any] = v.get("labels", {}) or {}
            voices.append(
                VoiceOption(
                    voice_id=v.get("voice_id", ""),
                    name=v.get("name", ""),
                    gender=labels.get("gender", ""),
                    language=labels.get("language", ""),
                    preview_url=v.get("preview_url") or "",
                    description=labels.get("description") or labels.get("use_case", ""),
                )
            )
        voices.sort(key=lambda v: v.name.lower())
        return voices
    except Exception as exc:
        logger.warning("_fetch_elevenlabs_voices: exception — {}", exc)
        return []


async def _fetch_cartesia_voices(
    api_key: str, language: str | None = None
) -> list[VoiceOption]:
    """Call the Cartesia voices endpoint and normalise the response.

    Cartesia voices have an ISO 639-1 ``language`` field (e.g. ``en``, ``fr``,
    ``pt``).  When ``language`` is provided the list is filtered to matching
    voices before being returned.
    """
    if not api_key:
        return []
    try:
        async with httpx.AsyncClient(timeout=10) as client:
            r = await client.get(
                "https://api.cartesia.ai/voices",
                headers={
                    "X-API-Key": api_key,
                    "Cartesia-Version": "2024-06-10",
                },
            )
            r.raise_for_status()
            data = r.json()
        voices = []
        for v in data:
            lang = v.get("language", "")
            # Server-side filter: skip voices that don't match the requested language
            if language and lang and lang.lower() != language.lower():
                continue
            voices.append(
                VoiceOption(
                    voice_id=v.get("id", ""),
                    name=v.get("name", ""),
                    language=lang,
                    description=v.get("description", ""),
                )
            )
        voices.sort(key=lambda v: v.name.lower())
        return voices
    except Exception as exc:
        logger.warning("Failed to fetch Cartesia voices: {}", exc)
        return []


def _deepgram_static_voices(language: str | None = None) -> list[VoiceOption]:
    """Return Deepgram Aura voices from TTS_MODEL_CATALOG, optionally filtered by language."""
    return _catalog_static_voices("deepgram", language)


def _catalog_static_voices(
    provider_prefix: str, language: str | None = None
) -> list[VoiceOption]:
    """Return voices from the Rust TTS_MODEL_CATALOG for providers with static voice lists.

    Used by OpenAI, Groq, and Deepgram which have no /voices API endpoint.
    Deduplicates by voice_id and optionally filters by ISO 639-1 language code.
    """
    try:
        catalog = get_tts_model_catalog()
    except Exception:
        catalog = []

    seen: set[str] = set()
    voices: list[VoiceOption] = []
    for spec in catalog:
        if not spec["provider"].startswith(provider_prefix):
            continue
        for lv in spec.get("language_voices", []):
            vid = lv["voice_id"]
            if vid in seen:
                continue
            lang_hint = lv.get("language_code", "en")
            # Apply language filter when provided (base code comparison)
            if language and lang_hint != language.split("-")[0].lower():
                continue
            seen.add(vid)
            voices.append(
                VoiceOption(
                    voice_id=vid,
                    name=lv.get("voice_label", vid),
                    language=lang_hint,
                )
            )
    return voices


async def _fetch_builtin_voices(
    base_url: str,
    config_voices: list[dict[str, Any]] | None,
) -> list[VoiceOption]:
    """Resolve voice catalog for a self-hosted / builtin TTS server.

    Two-tier resolution, in priority order:

    1. **Static config** — if the operator stored a ``voice_catalog`` list in
       the provider's ``config_json``, use that directly.  No network call.
       Each entry is: ``{"voice_id": str, "name": str, "language": str?,
       "gender": str?, "description": str?, "preview_url": str?}``.

    2. **Discovery probe** — try ``GET {base_url}/v1/voices``.  If the server
       responds with a JSON array of voice objects (same schema as above, or
       compatible with ElevenLabs / OpenAI voice listings), parse and return
       them.  This follows the voice-agent-os builtin TTS protocol convention so that
       any conforming self-hosted server gains automatic catalog discovery.

    If neither tier succeeds the endpoint returns ``[]`` and the frontend
    shows the raw voice-ID text input.
    """
    # ── Tier 1: static config ───────────────────────────────────────
    if config_voices:
        voices = []
        for v in config_voices:
            voices.append(
                VoiceOption(
                    voice_id=v.get("voice_id") or v.get("id", ""),
                    name=v.get("name", ""),
                    language=v.get("language", ""),
                    gender=v.get("gender", ""),
                    description=v.get("description", ""),
                    preview_url=v.get("preview_url", ""),
                )
            )
        if voices:
            logger.info(
                "_fetch_builtin_voices: returning {} static voices from config",
                len(voices),
            )
            return voices

    # ── Tier 2: discovery probe ─────────────────────────────────────
    if not base_url:
        return []
    probe_url = base_url.rstrip("/") + "/v1/voices"
    try:
        async with httpx.AsyncClient(timeout=5) as client:
            r = await client.get(probe_url)
        if not r.is_success:
            logger.debug(
                "_fetch_builtin_voices: probe {} returned HTTP {} — no catalog",
                probe_url,
                r.status_code,
            )
            return []
        data = r.json()
        # Accept both a top-level list or {"voices": [...]}
        if isinstance(data, dict):
            data = data.get("voices", [])
        voices = []
        for v in data or []:
            voices.append(
                VoiceOption(
                    voice_id=v.get("voice_id") or v.get("id", ""),
                    name=v.get("name", ""),
                    language=v.get("language", ""),
                    gender=v.get("gender", ""),
                    description=v.get("description", ""),
                    preview_url=v.get("preview_url", ""),
                )
            )
        if voices:
            logger.info(
                "_fetch_builtin_voices: discovered {} voices from {}",
                len(voices),
                probe_url,
            )
        return voices
    except Exception as exc:
        logger.debug("_fetch_builtin_voices: probe failed ({})", exc)
        return []


@router.get("/tts/voices", response_model=list[VoiceOption])
async def get_tts_voices(
    db: AsyncSession = Depends(get_db),
    language: str | None = None,
    provider: str | None = None,
) -> list[VoiceOption]:
    """Return the live voice catalog for the configured TTS provider.

    Query params
    ------------
    language : str, optional
        ISO 639-1 code (e.g. ``pt``, ``ru``, ``ar``).
    provider : str, optional
        Override which provider's voice library to fetch (e.g. ``elevenlabs-ws``).
        When omitted the global TTS provider setting is used.
    """
    # Resolve the TTS config: explicit provider override first, then global.
    pointer_row = await _get_provider_row(db, "tts", "__voice__")
    if provider:
        active_slug = provider
    elif pointer_row:
        active_slug = pointer_row.config_json.get(
            "active"
        ) or pointer_row.config_json.get("provider", "")
    else:
        active_slug = TTSConfig().provider
    creds_row = (
        await _get_provider_creds_row(db, "tts", active_slug) if active_slug else None
    )
    row = _resolve_active_config(pointer_row, creds_row) if not provider else creds_row
    cfg = tts_config_from_row(row) if row else TTSConfig()
    if provider:
        cfg.provider = provider
    provider = cfg.provider

    api_key = cfg.api_key
    raw_config: dict[str, Any] = (row.config_json or {}) if row else {}

    # Fallback: try reading plaintext api_key directly from the row
    # (handles legacy unencrypted rows).
    if not api_key and row:
        api_key = _normalize_api_key(raw_config.get("api_key", ""))

    if not api_key and provider not in ("fish-speech", "builtin"):
        logger.info(
            "GET /settings/tts/voices: no API key for provider='{}' — returning empty list.",
            provider,
        )

    if provider in ("elevenlabs", "elevenlabs-ws"):
        # Always use the personal voice library (/v1/voices).
        # Shared marketplace voices (/v1/shared-voices) require the user to first
        # "add to Voice Lab" before they can be used via the API — using a raw
        # shared voice_id yields "voice_id_does_not_exist" on the WS TTS endpoint.
        # Language selection is handled by the model (e.g. eleven_turbo_v2_5 is
        # multilingual), not by the voice choice.
        return await _fetch_elevenlabs_voices(api_key)
    if provider in ("cartesia", "cartesia-ws"):
        # Cartesia voices carry an ISO language field — filter post-fetch
        return await _fetch_cartesia_voices(api_key, language=language)
    if provider in ("deepgram", "deepgram-ws"):
        return _deepgram_static_voices(language=language)
    if provider == "openai":
        # OpenAI has no /voices API — voices are a fixed list per model.
        return _catalog_static_voices("openai", language=language)
    if provider == "groq":
        # Groq has no /voices API — voices are a fixed list per model.
        return _catalog_static_voices("groq", language=language)

    # Self-hosted / builtin / unknown provider
    # Try static config first, then probe the server's /v1/voices endpoint.
    config_voices: list[dict[str, Any]] | None = raw_config.get("voice_catalog") or None
    return await _fetch_builtin_voices(cfg.base_url, config_voices)


# ══════════════════════════════════════════════════════════════════
# Telephony Endpoints
# ══════════════════════════════════════════════════════════════════


class TelephonySettingsResponse(BaseModel):
    """Telephony config returned to the frontend — only voice_server_url."""

    voice_server_url: str = ""


class TelephonySettingsUpdate(BaseModel):
    """Update telephony settings — only voice_server_url."""

    voice_server_url: str = ""

    @field_validator("voice_server_url")
    @classmethod
    def validate_voice_server_url(cls, value: str) -> str:
        if not value:
            raise ValueError("voice_server_url is required")
        if not (value.startswith("http://") or value.startswith("https://")):
            raise ValueError("voice_server_url must start with http:// or https://")
        return value


# ── Observability defaults (single source of truth) ────────────────
_OBS_DEFAULT_QUEUE_SIZE: int = 2048
_OBS_DEFAULT_BATCH_SIZE: int = 128
_OBS_DEFAULT_FLUSH_INTERVAL_MS: int = 1000
_OBS_DEFAULT_SHUTDOWN_FLUSH_TIMEOUT_MS: int = 1500
_OBS_DEFAULT_DROP_POLICY: str = "drop_oldest"
_OBS_DEFAULT_DB_CATEGORIES: list[str] = [
    "session",
    "metrics",
    "observability",
    "tool",
    "error",
]
_OBS_DEFAULT_DB_EVENT_TYPES: list[str] = []  # empty = capture all event types


class ObservabilitySettingsResponse(BaseModel):
    db_events_enabled: bool = True
    langfuse_enabled: bool = False
    langfuse_base_url: str = "https://cloud.langfuse.com"
    langfuse_has_public_key: bool = False
    langfuse_has_secret_key: bool = False
    langfuse_trace_public: bool = False
    queue_size: int = _OBS_DEFAULT_QUEUE_SIZE
    batch_size: int = _OBS_DEFAULT_BATCH_SIZE
    flush_interval_ms: int = _OBS_DEFAULT_FLUSH_INTERVAL_MS
    shutdown_flush_timeout_ms: int = _OBS_DEFAULT_SHUTDOWN_FLUSH_TIMEOUT_MS
    drop_policy: str = _OBS_DEFAULT_DROP_POLICY
    db_categories: list[str] = Field(
        default_factory=lambda: list(_OBS_DEFAULT_DB_CATEGORIES)
    )
    db_event_types: list[str] = Field(
        default_factory=lambda: list(_OBS_DEFAULT_DB_EVENT_TYPES)
    )


class ObservabilitySettingsUpdate(BaseModel):
    db_events_enabled: bool = True
    langfuse_enabled: bool = False
    langfuse_base_url: str = "https://cloud.langfuse.com"
    langfuse_public_key: str = ""
    langfuse_secret_key: str = ""
    langfuse_trace_public: bool = False
    queue_size: int = Field(default=_OBS_DEFAULT_QUEUE_SIZE, ge=1, le=1_000_000)
    batch_size: int = Field(default=_OBS_DEFAULT_BATCH_SIZE, ge=1, le=10_000)
    flush_interval_ms: int = Field(
        default=_OBS_DEFAULT_FLUSH_INTERVAL_MS, ge=50, le=60_000
    )
    shutdown_flush_timeout_ms: int = Field(
        default=_OBS_DEFAULT_SHUTDOWN_FLUSH_TIMEOUT_MS, ge=50, le=60_000
    )
    drop_policy: str = _OBS_DEFAULT_DROP_POLICY
    db_categories: list[str] = Field(
        default_factory=lambda: list(_OBS_DEFAULT_DB_CATEGORIES)
    )
    db_event_types: list[str] = Field(
        default_factory=lambda: list(_OBS_DEFAULT_DB_EVENT_TYPES)
    )

    @field_validator("langfuse_base_url")
    @classmethod
    def validate_langfuse_base_url(cls, value: str) -> str:
        if not value:
            raise ValueError("langfuse_base_url is required")
        if not (value.startswith("http://") or value.startswith("https://")):
            raise ValueError("langfuse_base_url must start with http:// or https://")
        return value

    @field_validator("drop_policy")
    @classmethod
    def validate_drop_policy(cls, value: str) -> str:
        allowed = {"drop_oldest", "drop_newest", "block", "ignore"}
        if value not in allowed:
            raise ValueError(f"drop_policy must be one of {sorted(allowed)}")
        return value


def _telephony_to_response(cfg: TelephonyConfig) -> TelephonySettingsResponse:
    return TelephonySettingsResponse(
        voice_server_url=cfg.voice_server_url,
    )


@router.get("/telephony", response_model=TelephonySettingsResponse)
async def get_telephony_settings(
    db: AsyncSession = Depends(get_db),
) -> TelephonySettingsResponse:
    """Return the current telephony configuration from the DB."""
    row = await _get_provider_row(db, "telephony", "__voice__")
    cfg = telephony_config_from_row(row) if row else TelephonyConfig()
    return _telephony_to_response(cfg)


@router.put("/telephony", response_model=TelephonySettingsResponse)
async def update_telephony_settings(
    body: TelephonySettingsUpdate,
    db: AsyncSession = Depends(get_db),
) -> TelephonySettingsResponse:
    """Update telephony settings — only voice_server_url.

    Provider credentials are now stored per-number at import time.
    """
    row = await _get_provider_row(db, "telephony", "__voice__")

    config_json: dict[str, Any] = {}
    if row:
        # Preserve any existing fields (legacy credentials, etc.)
        config_json = dict(row.config_json)

    # Update voice_server_url exactly as submitted
    config_json["voice_server_url"] = body.voice_server_url

    if row:
        row.config_json = config_json
    else:
        row = ProviderConfig(
            provider_type="telephony",
            provider_name="__voice__",
            display_name="Voice Server",
            config_json=config_json,
            is_default=True,
        )
        db.add(row)

    await db.flush()

    telephony_config = telephony_config_from_row(row)
    logger.info(
        "Telephony settings updated: voice_server_url={}", body.voice_server_url
    )

    return _telephony_to_response(telephony_config)


@router.get("/observability", response_model=ObservabilitySettingsResponse)
async def get_observability_settings(
    db: AsyncSession = Depends(get_db),
) -> ObservabilitySettingsResponse:
    row = await _get_provider_row(db, "observability", "__voice__")
    cfg = row.config_json if row else {}
    has_public = bool(
        cfg.get("langfuse_public_key_encrypted") or cfg.get("langfuse_public_key")
    )
    has_secret = bool(
        cfg.get("langfuse_secret_key_encrypted") or cfg.get("langfuse_secret_key")
    )
    return ObservabilitySettingsResponse(
        db_events_enabled=bool(cfg.get("db_events_enabled", True)),
        langfuse_enabled=bool(cfg.get("langfuse_enabled", False)),
        langfuse_base_url=str(
            cfg.get("langfuse_base_url", "https://cloud.langfuse.com")
        ),
        langfuse_has_public_key=has_public,
        langfuse_has_secret_key=has_secret,
        langfuse_trace_public=bool(cfg.get("langfuse_trace_public", False)),
        queue_size=int(cfg.get("queue_size", _OBS_DEFAULT_QUEUE_SIZE)),
        batch_size=int(cfg.get("batch_size", _OBS_DEFAULT_BATCH_SIZE)),
        flush_interval_ms=int(
            cfg.get("flush_interval_ms", _OBS_DEFAULT_FLUSH_INTERVAL_MS)
        ),
        shutdown_flush_timeout_ms=int(
            cfg.get("shutdown_flush_timeout_ms", _OBS_DEFAULT_SHUTDOWN_FLUSH_TIMEOUT_MS)
        ),
        drop_policy=str(cfg.get("drop_policy", _OBS_DEFAULT_DROP_POLICY)),
        db_categories=list(cfg.get("db_categories", _OBS_DEFAULT_DB_CATEGORIES)),
        db_event_types=list(cfg.get("db_event_types", _OBS_DEFAULT_DB_EVENT_TYPES)),
    )


@router.put("/observability", response_model=ObservabilitySettingsResponse)
async def update_observability_settings(
    body: ObservabilitySettingsUpdate,
    db: AsyncSession = Depends(get_db),
) -> ObservabilitySettingsResponse:
    row = await _get_provider_row(db, "observability", "__voice__")
    existing_cfg: dict[str, Any] = dict(row.config_json) if row else {}

    config_json: dict[str, Any] = {
        **existing_cfg,
        "db_events_enabled": body.db_events_enabled,
        "langfuse_enabled": body.langfuse_enabled,
        "langfuse_base_url": body.langfuse_base_url,
        "langfuse_trace_public": body.langfuse_trace_public,
        "queue_size": body.queue_size,
        "batch_size": body.batch_size,
        "flush_interval_ms": body.flush_interval_ms,
        "shutdown_flush_timeout_ms": body.shutdown_flush_timeout_ms,
        "drop_policy": body.drop_policy,
        "db_categories": body.db_categories,
        "db_event_types": body.db_event_types,
    }

    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)

    if body.langfuse_public_key:
        ct, iv = engine.encrypt({"value": body.langfuse_public_key})
        config_json["langfuse_public_key_encrypted"] = {
            "ciphertext": ct,
            "iv": iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
        config_json.pop("langfuse_public_key", None)
    elif "langfuse_public_key_encrypted" in existing_cfg:
        config_json["langfuse_public_key_encrypted"] = existing_cfg[
            "langfuse_public_key_encrypted"
        ]

    if body.langfuse_secret_key:
        ct, iv = engine.encrypt({"value": body.langfuse_secret_key})
        config_json["langfuse_secret_key_encrypted"] = {
            "ciphertext": ct,
            "iv": iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
        config_json.pop("langfuse_secret_key", None)
    elif "langfuse_secret_key_encrypted" in existing_cfg:
        config_json["langfuse_secret_key_encrypted"] = existing_cfg[
            "langfuse_secret_key_encrypted"
        ]

    has_public = bool(
        config_json.get("langfuse_public_key_encrypted")
        or config_json.get("langfuse_public_key")
    )
    has_secret = bool(
        config_json.get("langfuse_secret_key_encrypted")
        or config_json.get("langfuse_secret_key")
    )

    if body.langfuse_enabled and (not has_public or not has_secret):
        raise HTTPException(
            status_code=400,
            detail="Langfuse is enabled but API keys are missing. Provide both public and secret keys.",
        )

    if row:
        row.config_json = config_json
        row.display_name = "Observability"
    else:
        row = ProviderConfig(
            provider_type="observability",
            provider_name="__voice__",
            display_name="Observability",
            config_json=config_json,
            is_default=True,
        )
        db.add(row)

    await db.flush()

    return ObservabilitySettingsResponse(
        db_events_enabled=bool(config_json.get("db_events_enabled", True)),
        langfuse_enabled=bool(config_json.get("langfuse_enabled", False)),
        langfuse_base_url=str(
            config_json.get("langfuse_base_url", "https://cloud.langfuse.com")
        ),
        langfuse_has_public_key=has_public,
        langfuse_has_secret_key=has_secret,
        langfuse_trace_public=bool(config_json.get("langfuse_trace_public", False)),
        queue_size=int(config_json.get("queue_size", _OBS_DEFAULT_QUEUE_SIZE)),
        batch_size=int(config_json.get("batch_size", _OBS_DEFAULT_BATCH_SIZE)),
        flush_interval_ms=int(
            config_json.get("flush_interval_ms", _OBS_DEFAULT_FLUSH_INTERVAL_MS)
        ),
        shutdown_flush_timeout_ms=int(
            config_json.get(
                "shutdown_flush_timeout_ms", _OBS_DEFAULT_SHUTDOWN_FLUSH_TIMEOUT_MS
            )
        ),
        drop_policy=str(config_json.get("drop_policy", _OBS_DEFAULT_DROP_POLICY)),
        db_categories=list(
            config_json.get("db_categories", _OBS_DEFAULT_DB_CATEGORIES)
        ),
        db_event_types=list(
            config_json.get("db_event_types", _OBS_DEFAULT_DB_EVENT_TYPES)
        ),
    )
