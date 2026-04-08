"""Recording URI resolution utilities.

The ``calls.recording_url`` column stores a **canonical storage URI** written
by voice-server:

| URI scheme         | What it means                                 |
|--------------------|-----------------------------------------------|
| ``file:///abs``    | Local filesystem path (bare metal or volume). |
| ``s3://bucket/key``| Object in an S3-compatible store.             |

This module provides ``resolve_recording_http_url`` — a pure function that
translates a stored URI into an HTTP URL the frontend can consume.
The actual file-serving endpoint lives in ``calls.py`` (``GET /calls/{id}/recording``)
to keep it co-located with the rest of the call resources.
"""

from __future__ import annotations

import logging
import threading
import uuid
from typing import Any
from urllib.parse import urlparse

import boto3
from botocore.config import Config

from app.lib.config import get_settings

logger = logging.getLogger(__name__)

# S3 client — created once and reused across requests.
# A lock guards initialization so that concurrent first-requests under a
# multi-threaded ASGI server (e.g. Uvicorn with multiple threads) don't race
# to build two separate clients.
_s3_client: Any = None
_s3_client_lock = threading.Lock()


def get_s3_client() -> Any:
    global _s3_client
    if _s3_client is None:
        with _s3_client_lock:
            # Double-checked locking: re-test inside the lock.
            if _s3_client is None:
                storage = get_settings().storage
                try:
                    _s3_client = boto3.client(
                        "s3",
                        endpoint_url=storage.aws_endpoint_url_s3 or None,
                        region_name=storage.aws_region,
                        aws_access_key_id=storage.aws_access_key_id or None,
                        aws_secret_access_key=storage.aws_secret_access_key or None,
                        config=Config(signature_version="s3v4"),
                    )
                except Exception as e:
                    logger.error(f"Failed to initialize boto3 S3 client: {e}")
                    raise
    return _s3_client


def resolve_recording_http_url(call_id: uuid.UUID, uri: str | None) -> str | None:
    """Translate a canonical storage URI into an HTTP URL for the frontend.

    Called at query time when building ``CallResponse`` objects — the raw URI
    is never sent to the frontend.

    Scheme mapping:

    - ``file://…``   → ``/api/calls/{call_id}/recording``  (proxied by the API)
    - ``s3://…``     → pre-signed URL (1-hour expiry via SigV4)
    - ``http(s)://`` → returned as-is  (legacy absolute URLs from older releases)
    - ``None``       → ``None``
    """
    if not uri:
        return None

    parsed = urlparse(uri)

    if parsed.scheme == "file":
        # Route through the proxy endpoint — browsers cannot access local paths.
        return f"/api/calls/{call_id}/recording"

    if parsed.scheme == "s3":
        try:
            bucket = parsed.netloc
            key = parsed.path.lstrip("/")
            client = get_s3_client()
            expiry = get_settings().storage.presigned_url_expiry_seconds
            url = client.generate_presigned_url(
                ClientMethod="get_object",
                Params={"Bucket": bucket, "Key": key},
                ExpiresIn=expiry,
            )
            return str(url)
        except Exception as e:
            logger.error(f"Failed to generate presigned S3 url for {uri}: {e}")
            return None

    if parsed.scheme in ("http", "https"):
        # Legacy: absolute HTTP URL stored by an older voice-server release.
        return uri

    return None
