"""Voice Agent OS — Voice Agent OS.

FastAPI application entrypoint.
"""

from __future__ import annotations

import logging
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager

from fastapi import Depends, FastAPI
from fastapi.middleware.cors import CORSMiddleware
from loguru import logger
from pydantic_ai.exceptions import UserError as PydanticAIUserError

from app.agent_builder import builder_service
from app.api.agents import router as agents_router
from app.api.builder import router as builder_router
from app.api.calls import router as calls_router
from app.api.credentials import router as credentials_router
from app.api.evaluations import router as evaluations_router
from app.api.integrations import router as integrations_router
from app.api.oauth import router as oauth_router
from app.api.oauth_apps import router as oauth_apps_router

from app.api.phone_numbers import router as phone_numbers_router

from app.api.settings import get_builder_llm_config
from app.api.settings import router as settings_router
from app.api.voice_session import router as voice_router
from app.lib import get_settings
from app.lib.auth import require_api_key
from app.lib.database import async_session


class _HealthCheckFilter(logging.Filter):
    """Suppress noisy GET /api/health access log lines."""

    def filter(self, record: logging.LogRecord) -> bool:
        msg = record.getMessage()
        return "GET /api/health" not in msg


logging.getLogger("uvicorn.access").addFilter(_HealthCheckFilter())

VERSION = "0.2.0"


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncGenerator[None, None]:
    """Application lifecycle — startup and shutdown."""
    settings = get_settings()

    # Guard against running production with the default secret key
    if settings.is_production and "dev-secret" in settings.auth.secret_key:
        raise RuntimeError(
            "Refusing to start in production with the default secret key. "
            "Set AUTH__SECRET_KEY in your environment."
        )

    # Load persisted builder LLM config from DB and reconfigure the builder
    # Uses the two-layer pointer→creds resolution so the active provider
    # (e.g. gemini) is correctly loaded instead of falling back to defaults.
    async with async_session() as db:
        builder_llm_cfg = await get_builder_llm_config(db)
        try:
            builder_service.reconfigure(builder_llm_cfg)
        except PydanticAIUserError as exc:
            logger.warning(
                "Builder LLM config invalid during startup; keeping default "
                "builder model. provider={}, model={}, error={}",
                builder_llm_cfg.provider,
                builder_llm_cfg.model,
                exc,
            )

    # NOTE: The voice-server binary runs as a separate process and owns
    # port 8300. Python no longer starts or stops the embedded Rust server.

    logger.info("Voice Agent OS starting in {} mode", settings.environment)
    yield

    logger.info("Voice Agent OS shutting down")


app = FastAPI(
    title="Voice Agent OS",
    description="Open-source Voice Agent OS for business back office",
    version=VERSION,
    lifespan=lifespan,
)

# ── CORS — driven by settings ────────────────────────────────────
settings = get_settings()
app.add_middleware(
    CORSMiddleware,
    allow_origins=settings.allowed_origins,
    allow_credentials=True,
    allow_methods=["GET", "POST", "PATCH", "PUT", "DELETE", "OPTIONS"],
    allow_headers=[
        "Authorization",
        "Content-Type",
        "X-API-Key",
        "Idempotency-Key",
        "X-Workspace-Id",
    ],
)

# ── Authenticated routes (require API key) ───────────────────────
_auth = [Depends(require_api_key)]

app.include_router(agents_router, prefix="/api", dependencies=_auth)
app.include_router(builder_router, prefix="/api", dependencies=_auth)
app.include_router(calls_router, prefix="/api", dependencies=_auth)
app.include_router(credentials_router, prefix="/api", dependencies=_auth)
app.include_router(integrations_router, prefix="/api", dependencies=_auth)
app.include_router(evaluations_router, prefix="/api", dependencies=_auth)
app.include_router(settings_router, prefix="/api", dependencies=_auth)
app.include_router(phone_numbers_router, prefix="/api", dependencies=_auth)
# OAuth routes: authorize needs auth (user-initiated), callback is public
# (browser popup redirect from OAuth provider has no API key header)
app.include_router(oauth_router, prefix="/api")
app.include_router(oauth_apps_router, prefix="/api", dependencies=_auth)

# ── WebSocket routes (auth handled inside the handler via query params,
#    because browsers cannot send custom HTTP headers on WS upgrades) ──
app.include_router(voice_router, prefix="/api")

# NOTE: Twilio/Telnyx webhooks are handled by voice-server directly.


@app.get("/api/health")
async def health_check() -> dict[str, str]:
    """Health check endpoint (unauthenticated)."""
    return {
        "status": "healthy",
        "service": "studio-api",
        "version": VERSION,
    }
