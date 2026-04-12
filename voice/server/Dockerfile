# syntax=docker/dockerfile:1
# ── Voice Server — Rust binary ────────────────────────────────────
# Multi-stage build: compile in Ubuntu 24.04 (glibc 2.39), copy binary into
# a slim Ubuntu 24.04 runtime image.
#
# WHY Ubuntu 24.04 (not Debian Bookworm)?
# ort-sys 2.0.0-rc.11 bundles prebuilt ONNX Runtime static libraries that
# reference __isoc23_strtoll / __isoc23_strtoull (added in glibc 2.38).
# Debian Bookworm ships glibc 2.36 → linker fails with "undefined reference".
# Ubuntu 24.04 ships glibc 2.39 → symbols present in both builder and runtime.

# ── Stage 1: Build ────────────────────────────────────────────────
FROM ubuntu:24.04 AS builder

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    build-essential \
    cmake \
    clang \
    libclang-dev \
    libssl-dev \
    libopus-dev \
    pkg-config \
    protobuf-compiler \
    patch \
    && rm -rf /var/lib/apt/lists/*

# Install Rust via rustup (no official rust:*-ubuntu24.04 image exists)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# Neutralise any rustc-wrapper=sccache set in .cargo/config.toml — sccache
# is not installed here and would cause the build to fail immediately.
ENV RUSTC_WRAPPER=""

# Without a workspace Cargo.toml at /build/, `cargo build` inside
# voice/server/ would put the target at /build/voice/server/target/.
# Pin it to /build/target/ so the cache mounts and the cp below agree.
ENV CARGO_TARGET_DIR=/build/target

WORKDIR /build

# Copy workspace members needed for compilation
COPY voice/server/ voice/server/
COPY voice/engine/ voice/engine/
COPY integrations/ integrations/
COPY proto/ proto/

# Build using the dev profile: skips LTO + codegen-units=1 (7+ min → ~2 min).
# All deps are still opt-level=3 (see [profile.dev.package."*"] in Cargo.toml)
# so runtime perf of libraries is equivalent to release.
RUN --mount=type=cache,target=/build/target,sharing=locked \
    --mount=type=cache,target=/root/.cargo/registry,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,sharing=locked \
    cd voice/server && cargo build \
    && cp /build/target/debug/voice-server /voice-server-bin

# ── Stage 2: Runtime ─────────────────────────────────────────────
# Must also be Ubuntu 24.04 — the binary was linked against glibc 2.39 and
# will SIGILL / fail to load on older glibc at runtime.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    libopus0 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /voice-server-bin /usr/local/bin/voice-server
COPY --from=builder /build/voice/server/dsp_models /dsp_models

EXPOSE 8300

CMD ["voice-server"]
