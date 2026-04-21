"""KV Store — ephemeral key-value storage with TTL.

Provides a simple abstraction over in-memory and Redis backends.
Used for short-lived data like OAuth state tokens.

Usage::

    store = create_kv_store()  # auto-detects Redis, falls back to in-memory

    await store.set("key", {"data": "value"}, ttl=600)
    data = await store.get("key")        # → {"data": "value"} or None
    data = await store.pop("key")        # atomic get-and-delete
"""

from __future__ import annotations

import json
import time
from typing import Any, Protocol

from loguru import logger


class KVStore(Protocol):
    """Protocol for ephemeral key-value stores with TTL."""

    async def set(self, key: str, value: dict[str, Any], ttl: int) -> None:
        """Store a value with a time-to-live in seconds."""
        ...

    async def get(self, key: str) -> dict[str, Any] | None:
        """Retrieve a value, or None if expired/missing."""
        ...

    async def pop(self, key: str) -> dict[str, Any] | None:
        """Atomically retrieve and delete a value."""
        ...

    async def delete(self, key: str) -> None:
        """Delete a key."""
        ...


class InMemoryKVStore:
    """In-memory KV store with TTL — suitable for single-process deployments.

    Not shared across workers. Fine for development and small deployments.
    For multi-process production, use Redis.
    """

    def __init__(self) -> None:
        self._store: dict[
            str, tuple[dict[str, Any], float]
        ] = {}  # key → (value, expires_at)

    def _evict_expired(self) -> None:
        """Lazy eviction of expired keys."""
        now = time.monotonic()
        expired = [k for k, (_, exp) in self._store.items() if exp <= now]
        for k in expired:
            del self._store[k]

    async def set(self, key: str, value: dict[str, Any], ttl: int) -> None:
        self._evict_expired()
        self._store[key] = (value, time.monotonic() + ttl)

    async def get(self, key: str) -> dict[str, Any] | None:
        entry = self._store.get(key)
        if entry is None:
            return None
        value, expires_at = entry
        if time.monotonic() >= expires_at:
            del self._store[key]
            return None
        return value

    async def pop(self, key: str) -> dict[str, Any] | None:
        entry = self._store.pop(key, None)
        if entry is None:
            return None
        value, expires_at = entry
        if time.monotonic() >= expires_at:
            return None
        return value

    async def delete(self, key: str) -> None:
        self._store.pop(key, None)


class RedisKVStore:
    """Redis-backed KV store with TTL — for multi-process production."""

    def __init__(self, redis_url: str, prefix: str = "kv:") -> None:
        import redis.asyncio as aioredis

        self._redis = aioredis.from_url(redis_url, decode_responses=True)
        self._prefix = prefix

    def _key(self, key: str) -> str:
        return f"{self._prefix}{key}"

    async def set(self, key: str, value: dict[str, Any], ttl: int) -> None:
        await self._redis.setex(self._key(key), ttl, json.dumps(value))

    async def get(self, key: str) -> dict[str, Any] | None:
        raw = await self._redis.get(self._key(key))
        if raw is None:
            return None
        result: dict[str, Any] = json.loads(raw)
        return result

    async def pop(self, key: str) -> dict[str, Any] | None:
        k = self._key(key)
        raw = await self._redis.getdel(k)
        if raw is None:
            return None
        result: dict[str, Any] = json.loads(raw)
        return result

    async def delete(self, key: str) -> None:
        await self._redis.delete(self._key(key))


def create_kv_store() -> InMemoryKVStore | RedisKVStore:
    """Auto-detect backend: use Redis if REDIS__URL is set, else in-memory."""
    from app.lib.config import get_settings

    redis_url = get_settings().redis.url
    if redis_url:
        logger.info("KV store: Redis ({})", redis_url[:30])
        return RedisKVStore(redis_url, prefix="oauth_state:")

    logger.info("KV store: in-memory (set REDIS__URL for multi-process)")
    return InMemoryKVStore()


# Module-level singleton
kv_store = create_kv_store()
