# syntax=docker/dockerfile:1
# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — builder
#   Installs Rust + build tools, compiles PyO3 extensions, syncs Python deps.
#   Nothing from this stage's toolchain ships in the final image.
# ─────────────────────────────────────────────────────────────────────────────
FROM python:3.12-slim AS builder

WORKDIR /workspace

# Build-time system dependencies (NOT copied to the final stage)
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    clang \
    cmake \
    curl \
    libclang-dev \
    patch \
    pkg-config \
    libssl-dev \
    libopus-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Install Rust (needed to compile voice-engine PyO3 extensions)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"
# sccache is not present here — ensure cargo uses rustc directly.
ENV RUSTC_WRAPPER=""

# Install uv
COPY --from=ghcr.io/astral-sh/uv:0.10.9 /uv /usr/local/bin/uv

# Keep the venv in a fixed path so the final stage can copy it cleanly.
ENV UV_PROJECT_ENVIRONMENT=/opt/venv

# ── Dependency layer (cache-friendly) ────────────────────────────────────────
# Copy only the metadata files first so this layer is only invalidated when
# pyproject.toml / uv.lock change — not when application code changes.
COPY studio/api/pyproject.toml studio/api/uv.lock* /workspace/studio/api/
COPY voice/engine/pyproject.toml /workspace/voice/engine/pyproject.toml
COPY integrations/pyproject.toml /workspace/integrations/pyproject.toml

WORKDIR /workspace/studio/api

RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-install-package voice-engine --no-install-package integrations

# ── Compile PyO3 extensions ───────────────────────────────────────────────────
COPY voice/engine /workspace/voice/engine
COPY integrations /workspace/integrations
COPY proto /workspace/proto

RUN --mount=type=cache,target=/root/.cache/uv \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/workspace/voice/engine/target \
    --mount=type=cache,target=/workspace/integrations/target \
    uv sync --frozen --no-dev

# ── Copy application source ───────────────────────────────────────────────────
COPY studio/api/ /workspace/studio/api/

# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 — final runtime image
#   Only the compiled venv + application code. No Rust, no clang, no cmake.
# ─────────────────────────────────────────────────────────────────────────────
FROM python:3.12-slim

WORKDIR /app

# Runtime-only libraries our compiled extensions actually link against
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 \
    libopus0 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy compiled venv and application from builder
COPY --from=builder /opt/venv /opt/venv
COPY --from=builder /workspace/studio/api /app

ENV PATH="/opt/venv/bin:${PATH}"

# Run as a non-root user
RUN useradd --create-home --shell /bin/bash appuser \
    && chown -R appuser:appuser /app /opt/venv
USER appuser

EXPOSE 8000

CMD ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
