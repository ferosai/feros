"""Tests for the OAuth 2.0 flow — KV store, PKCE, client credentials."""

from __future__ import annotations

import asyncio
from unittest.mock import MagicMock, patch

import pytest

from app.lib.kv_store import InMemoryKVStore

# ── KV Store ─────────────────────────────────────────────────────


class TestInMemoryKVStore:
    @pytest.fixture
    def store(self) -> InMemoryKVStore:
        return InMemoryKVStore()

    @pytest.mark.asyncio
    async def test_set_and_get(self, store: InMemoryKVStore) -> None:
        await store.set("key1", {"foo": "bar"}, ttl=60)
        result = await store.get("key1")
        assert result == {"foo": "bar"}

    @pytest.mark.asyncio
    async def test_get_missing_returns_none(self, store: InMemoryKVStore) -> None:
        result = await store.get("nonexistent")
        assert result is None

    @pytest.mark.asyncio
    async def test_pop_returns_and_deletes(self, store: InMemoryKVStore) -> None:
        await store.set("key1", {"a": "b"}, ttl=60)
        result = await store.pop("key1")
        assert result == {"a": "b"}
        assert await store.get("key1") is None

    @pytest.mark.asyncio
    async def test_pop_missing_returns_none(self, store: InMemoryKVStore) -> None:
        result = await store.pop("nonexistent")
        assert result is None

    @pytest.mark.asyncio
    async def test_delete(self, store: InMemoryKVStore) -> None:
        await store.set("key1", {"x": "y"}, ttl=60)
        await store.delete("key1")
        assert await store.get("key1") is None

    @pytest.mark.asyncio
    async def test_expired_key_returns_none(self, store: InMemoryKVStore) -> None:
        # Set with 0-second TTL (already expired)
        await store.set("key1", {"expired": "true"}, ttl=0)
        # monotonic clock may not have advanced, so sleep briefly
        await asyncio.sleep(0.01)
        result = await store.get("key1")
        assert result is None

    @pytest.mark.asyncio
    async def test_expired_pop_returns_none(self, store: InMemoryKVStore) -> None:
        await store.set("key1", {"expired": "true"}, ttl=0)
        await asyncio.sleep(0.01)
        result = await store.pop("key1")
        assert result is None


# ── OAuth client credential lookup ───────────────────────────────


class TestGetOAuthClientCredentials:
    @pytest.mark.asyncio
    async def test_returns_creds_from_db(self) -> None:
        """get_oauth_client_credentials returns client_id and decrypted secret."""
        mock_app = MagicMock()
        mock_app.client_id = "gid-123"

        with (
            patch(
                "app.lib.oauth_apps.get_oauth_app",
                return_value=mock_app,
            ),
            patch(
                "app.lib.oauth_apps.decrypt_client_secret",
                return_value="gsec-456",
            ),
        ):
            from app.lib.oauth_apps import get_oauth_client_credentials

            mock_db = MagicMock()
            client_id, client_secret = await get_oauth_client_credentials(
                "google", mock_db
            )

        assert client_id == "gid-123"
        assert client_secret == "gsec-456"

    @pytest.mark.asyncio
    async def test_returns_empty_when_not_configured(self) -> None:
        """Returns empty strings when no OAuth app is registered."""
        with patch(
            "app.lib.oauth_apps.get_oauth_app",
            return_value=None,
        ):
            from app.lib.oauth_apps import get_oauth_client_credentials

            mock_db = MagicMock()
            client_id, client_secret = await get_oauth_client_credentials(
                "unknown", mock_db
            )

        assert client_id == ""
        assert client_secret == ""
