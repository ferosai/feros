"""Application configuration — strongly typed with Pydantic Settings.

Infrastructure settings (database, auth, redis, etc.) are loaded from
environment variables and .env files.

Provider settings (LLM, STT, TTS, Telephony) are stored in the database
and managed via the Settings UI. Their Pydantic models and DB read helpers
live in this module alongside the env-backed settings.
"""

from __future__ import annotations

import os
from enum import StrEnum
from functools import lru_cache
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from pydantic import BaseModel, Field
from pydantic_settings import BaseSettings, SettingsConfigDict
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

# ── Enums ────────────────────────────────────────────────────────


class Environment(StrEnum):
    DEVELOPMENT = "development"
    STAGING = "staging"
    PRODUCTION = "production"
    TESTING = "testing"


# ══════════════════════════════════════════════════════════════════
# Infrastructure Config (env-backed)
# ══════════════════════════════════════════════════════════════════


class DatabaseConfig(BaseModel):
    """PostgreSQL connection settings.

    Env vars: DATABASE_URL, DATABASE_ECHO
    """

    url: str = "postgresql://voice-agent-os:ferosdev@localhost:5432/voice_agent"
    echo: bool = False
    pool_size: int = 5
    max_overflow: int = 10


class RedisConfig(BaseModel):
    """Redis connection settings.

    Env vars: REDIS__URL
    """

    url: str = ""


class StorageConfig(BaseModel):
    """S3/Cloudflare R2 Storage settings for recordings.

    Env vars: STORAGE__AWS_ACCESS_KEY_ID, STORAGE__AWS_SECRET_ACCESS_KEY,
              STORAGE__AWS_REGION, STORAGE__AWS_ENDPOINT_URL_S3
    """

    aws_access_key_id: str = ""
    aws_secret_access_key: str = ""
    aws_region: str = "auto"
    aws_endpoint_url_s3: str = "https://<account_id>.r2.cloudflarestorage.com"
    presigned_url_expiry_seconds: int = 3600


class AuthConfig(BaseModel):
    """Authentication settings.

    Env vars: AUTH__SECRET_KEY, AUTH__API_KEY, AUTH__ACCESS_TOKEN_EXPIRE_MINUTES
    """

    secret_key: str = "dev-secret-change-in-production"
    credential_salt: str = "voice-agent-os-credential-vault-v1"
    api_key: str = ""  # Set via AUTH__API_KEY; empty = auth disabled in dev
    access_token_expire_minutes: int = 60 * 24 * 7  # 7 days
    algorithm: str = "HS256"

class GeminiConfig(BaseModel):
    """Gemini API settings (used for builder web search).

    Env vars: GEMINI__API_KEY
    """

    api_key: str = ""


# ══════════════════════════════════════════════════════════════════
# Provider Config (DB-backed)
#
# These are plain Pydantic models — NOT populated from env vars.
# The DB (provider_configs table) is the single source of truth.
# Defaults represent what the UI shows when no DB row exists yet.
# ══════════════════════════════════════════════════════════════════


class LLMConfig(BaseModel):
    """LLM provider settings.

    Supported providers:
      - groq       → Groq inference (requires api_key)
      - openai     → OpenAI API directly (requires api_key)
      - anthropic  → Anthropic Claude (requires api_key)
      - gemini     → Google Gemini (requires api_key)
      - deepseek   → DeepSeek (requires api_key)
      - ollama     → local Ollama instance (no api_key needed)
      - together   → Together AI (requires api_key)
      - fireworks  → Fireworks AI (requires api_key)
      - openrouter → OpenRouter.ai (requires api_key)
      - vllm       → self-hosted vLLM (OpenAI-compatible, may need api_key)
      - custom     → any OpenAI-compatible endpoint (may need api_key)
    """

    provider: str = Field(
        default="ollama",
        description="LLM provider: groq | openai | anthropic | gemini | deepseek | ollama | openrouter | custom",
    )
    model: str = Field(
        default="llama3.2",
        description="Model name or ID for the chosen provider",
    )
    base_url: str = Field(
        default="http://localhost:11434",
        description="Base URL for the LLM API",
    )
    api_key: str = Field(
        default="",
        description="Required for openrouter / openai / custom providers",
    )
    temperature: float = Field(default=0.7)
    max_tokens: int = Field(default=256)


class STTConfig(BaseModel):
    """Speech-to-Text provider settings."""

    provider: str = "faster-whisper"
    base_url: str = "http://localhost:8100"
    model: str = "large-v3"
    language: str = "en"
    api_key: str = ""  # decrypted at read time; never stored plaintext


class TTSConfig(BaseModel):
    """Text-to-Speech provider settings."""

    provider: str = "fish-speech"
    base_url: str = "http://localhost:8200"
    model: str = ""
    voice_id: str = "default"
    api_key: str = ""  # decrypted at read time; never stored plaintext


class TelephonyConfig(BaseModel):
    """Telephony provider settings."""

    voice_server_url: str = (
        "http://localhost:8300"  # Public URL of the Rust voice server
    )


class ExperimentalConfig(BaseModel):
    """Experimental feature flags."""

    telnyx: bool = False


# ── Env File Resolution ─────────────────────────────────────────


def _resolve_env_files() -> list[Path]:
    """Resolve .env files in Next.js-style precedence order.

    Loading order (last wins):
        1. .env              — base defaults (committed)
        2. .env.local        — local overrides (gitignored)
        3. .env.{environment}— environment-specific (committed)
        4. .env.{environment}.local — env-specific local (gitignored)
    """
    env = os.getenv("ENVIRONMENT", "development").lower()
    backend_dir = Path(__file__).resolve().parent.parent.parent

    candidates = [
        backend_dir / ".env",
        backend_dir / ".env.local",
        backend_dir / f".env.{env}",
        backend_dir / f".env.{env}.local",
    ]

    return [f for f in candidates if f.is_file()]


# ── Root Settings ────────────────────────────────────────────────


class Settings(BaseSettings):
    """Root application configuration.

    All fields are strongly typed. Nested models use env_prefix
    for flat environment variable naming:

        DATABASE__URL=...
        REDIS__URL=...
        AUTH__SECRET_KEY=...

    .env files are loaded with cascading precedence based on
    the ENVIRONMENT variable.

    NOTE: Provider settings (LLM, STT, TTS, Telephony) are stored
    in the database and managed via the Settings UI. They are NOT
    configured through environment variables.
    """

    # App
    app_name: str = "Voice Agent OS"
    environment: Environment = Environment.DEVELOPMENT
    debug: bool = False
    api_prefix: str = "/api"
    log_level: str = "info"

    # Nested configs — populated from prefixed env vars
    database: DatabaseConfig = Field(default_factory=DatabaseConfig)
    redis: RedisConfig = Field(default_factory=RedisConfig)
    storage: StorageConfig = Field(default_factory=StorageConfig)
    auth: AuthConfig = Field(default_factory=AuthConfig)
    gemini: GeminiConfig = Field(default_factory=GeminiConfig)
    experimental: ExperimentalConfig = Field(default_factory=ExperimentalConfig)

    # OAuth callback URL — per-deployment, not per-integration
    oauth_callback_base_url: str = "http://localhost:3000"

    # CORS
    allowed_origins: list[str] = Field(
        default_factory=lambda: [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://studio-web:3000",
        ]
    )

    model_config = SettingsConfigDict(
        env_file=_resolve_env_files(),
        env_file_encoding="utf-8",
        env_nested_delimiter="__",
        extra="ignore",
    )

    @property
    def is_production(self) -> bool:
        return self.environment == Environment.PRODUCTION

    @property
    def is_development(self) -> bool:
        return self.environment == Environment.DEVELOPMENT

    @property
    def is_testing(self) -> bool:
        return self.environment == Environment.TESTING


@lru_cache
def get_settings() -> Settings:
    return Settings()


# ══════════════════════════════════════════════════════════════════
# Provider Config — DB Read Helpers
# ══════════════════════════════════════════════════════════════════


async def _get_provider_row(
    db: AsyncSession,
    provider_type: str,
    provider_name: str,
) -> Any | None:
    """Get a ProviderConfig row from the DB, if any."""
    from app.models.provider import ProviderConfig

    result = await db.execute(
        select(ProviderConfig).where(
            ProviderConfig.provider_type == provider_type,
            ProviderConfig.provider_name == provider_name,
        )
    )
    return result.scalar_one_or_none()


async def _get_effective_provider_row(
    db: AsyncSession,
    provider_type: str,
    provider_name: str,
) -> Any | None:
    """Resolve pointer rows (e.g. ``__voice__``) to their active provider config.

    Settings endpoints persist provider credentials under provider slugs
    (``openrouter``, ``deepgram``...) and store the current selection in a
    pointer row (``__voice__``, ``__builder__``). Runtime readers need the
    merged view to avoid falling back to hardcoded defaults.
    """
    row = await _get_provider_row(db, provider_type, provider_name)
    if row is None:
        return None

    # Direct provider rows already contain concrete config.
    if not provider_name.startswith("__"):
        return row

    pointer_cfg = dict(row.config_json or {})
    active = pointer_cfg.get("active") or pointer_cfg.get("provider")
    if not isinstance(active, str) or not active:
        return row

    active_row = await _get_provider_row(db, provider_type, active)
    if active_row is None:
        # Keep explicit provider selection even when creds row is missing.
        pointer_cfg.setdefault("provider", active)
        return SimpleNamespace(config_json=pointer_cfg)

    merged_cfg = {
        **dict(active_row.config_json or {}),
        "active": active,
        "provider": active,
    }
    return SimpleNamespace(config_json=merged_cfg)


def _decrypt_inline(blob: dict[str, str] | str) -> dict[str, str]:
    """Decrypt an inline AES-256-GCM encrypted value stored in config_json."""
    if isinstance(blob, dict):
        try:
            import integrations

            engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
            return engine.decrypt(blob["ciphertext"], blob.get("iv", ""))
        except Exception:
            pass
    return {"value": ""}


def _normalize_api_key(value: str | None) -> str:
    """Trim surrounding whitespace from provider API keys."""
    return value.strip() if isinstance(value, str) else ""


def llm_config_from_row(row: Any) -> LLMConfig:
    """Build an LLMConfig from a provider_configs row."""
    cfg: dict[str, Any] = row.config_json
    api_key = _normalize_api_key(cfg.get("api_key", ""))
    if "api_key_encrypted" in cfg:
        api_key = _normalize_api_key(
            _decrypt_inline(cfg["api_key_encrypted"]).get("value", "")
        )

    return LLMConfig(
        provider=cfg.get("provider", "ollama"),
        model=cfg.get("model", "llama3.2"),
        base_url=cfg.get("base_url", "http://localhost:11434"),
        api_key=api_key,
        temperature=cfg.get("temperature", 0.7),
        max_tokens=cfg.get("max_tokens", 256),
    )


def stt_config_from_row(row: Any) -> STTConfig:
    """Build an STTConfig from a provider_configs row."""
    cfg: dict[str, Any] = row.config_json
    api_key = _normalize_api_key(cfg.get("api_key", ""))
    if "api_key_encrypted" in cfg:
        api_key = _normalize_api_key(
            _decrypt_inline(cfg["api_key_encrypted"]).get("value", "")
        )
    return STTConfig(
        provider=cfg.get("provider", "faster-whisper"),
        base_url=cfg.get("base_url", "http://localhost:8100"),
        model=cfg.get("model", "large-v3"),
        language=cfg.get("language", "en"),
        api_key=api_key,
    )


def tts_config_from_row(row: Any) -> TTSConfig:
    """Build a TTSConfig from a provider_configs row."""
    cfg: dict[str, Any] = row.config_json
    api_key = _normalize_api_key(cfg.get("api_key", ""))
    if "api_key_encrypted" in cfg:
        api_key = _normalize_api_key(
            _decrypt_inline(cfg["api_key_encrypted"]).get("value", "")
        )
    return TTSConfig(
        provider=cfg.get("provider", "fish-speech"),
        base_url=cfg.get("base_url", "http://localhost:8200"),
        model=cfg.get("model", ""),
        voice_id=cfg.get("voice_id", "default"),
        api_key=api_key,
    )


def telephony_config_from_row(row: Any) -> TelephonyConfig:
    """Build a TelephonyConfig from a provider_configs row."""
    cfg: dict[str, Any] = row.config_json

    return TelephonyConfig(
        voice_server_url=cfg.get("voice_server_url")
        or TelephonyConfig().voice_server_url,
    )


_LLM_ROLE_PREFIX: dict[str, str] = {
    "__builder__": "builder",
    "__voice__": "voice",
    "__native_multimodal__": "native_multimodal",
}


async def get_llm_config(
    db: AsyncSession,
    provider_name: str = "__builder__",
) -> LLMConfig:
    """Read the LLM config from DB, or return defaults.

    Resolves the pointer row for *provider_name* (``__builder__`` or
    ``__voice__``), then loads the role-scoped credential row
    (e.g. ``builder::openai``).  Does **not** fall back to legacy
    shared rows.
    """
    pointer_row = await _get_provider_row(db, "llm", provider_name)
    if pointer_row is None:
        return LLMConfig()

    pointer_cfg = dict(pointer_row.config_json or {})
    active = pointer_cfg.get("active") or pointer_cfg.get("provider")
    if not isinstance(active, str) or not active:
        return LLMConfig()

    role_prefix = _LLM_ROLE_PREFIX.get(provider_name)
    if role_prefix is None:
        return LLMConfig()

    scoped_row = await _get_provider_row(db, "llm", f"{role_prefix}::{active}")
    if scoped_row is None:
        return LLMConfig()

    return llm_config_from_row(scoped_row)


async def get_stt_config(db: AsyncSession) -> STTConfig:
    """Read the STT config from DB, or return defaults."""
    row = await _get_effective_provider_row(db, "stt", "__voice__")
    if row:
        return stt_config_from_row(row)
    return STTConfig()


async def get_tts_config(db: AsyncSession) -> TTSConfig:
    """Read the TTS config from DB, or return defaults."""
    row = await _get_effective_provider_row(db, "tts", "__voice__")
    if row:
        return tts_config_from_row(row)
    return TTSConfig()


async def get_telephony_config(db: AsyncSession) -> TelephonyConfig:
    """Read the Telephony config from DB, or return defaults."""
    row = await _get_provider_row(db, "telephony", "__voice__")
    if row:
        return telephony_config_from_row(row)
    return TelephonyConfig()
