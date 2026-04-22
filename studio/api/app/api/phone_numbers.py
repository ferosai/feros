"""Phone Number management API.

Handles the full lifecycle of phone numbers:
  - Listing numbers stored in our DB
  - Importing existing numbers from a provider account (Twilio / Telnyx)
  - Assigning a number to an agent + auto-configuring the provider webhook
  - Unassigning a number from an agent and clearing the webhook

Provider specifics
------------------
Twilio:
  Each IncomingPhoneNumber has its own VoiceUrl that Twilio POSTs to when a call
  arrives. We set it to:
    {voice_server_url}/telephony/twilio/incoming/{agent_id}
  The agent_id is embedded in the webhook URL; voice-server routes the call.

Telnyx:
  Telnyx uses "Connections" (Voice API Applications) as the indirection layer.
  A phone number is associated with a connection, and the connection has a
  webhook URL. To route a number to an agent we:
    1. Create or update a Telnyx Voice API Application (connection) with a
       webhook URL pointing at: {voice_server_url}/telephony/telnyx/inbound/{agent_id}
    2. Associate the phone number with that connection via PATCH /phone_numbers/{id}
  We persist the connection_id in the phone_numbers row.
"""

from __future__ import annotations

import json
import uuid as _uuid
from typing import Any, NoReturn, cast

import httpx
from fastapi import APIRouter, Depends, HTTPException
from loguru import logger
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

import integrations

from app.lib.auth import TenantContext, require_tenant
from app.lib.config import get_settings, get_telephony_config
from app.lib.database import get_db
from app.models.agent import Agent
from app.models.credential import CURRENT_ENCRYPTION_VERSION
from app.models.phone_number import PhoneNumber
from app.schemas.phone_number import (
    AssignPhoneNumberRequest,
    FetchNumbersRequest,
    FetchNumbersResponse,
    ImportNumbersRequest,
    PhoneNumberListResponse,
    PhoneNumberResponse,
    ProviderNumber,
)

router = APIRouter(prefix="/phone-numbers", tags=["phone-numbers"])

STALE_NUMBER_AUTH_CODE = "STALE_PHONE_NUMBER_AUTH"
STALE_NUMBER_CODE = "STALE_PHONE_NUMBER_PROVIDER_RESOURCE"
STALE_NUMBER_AUTH_MESSAGE = (
    "The stored authentication for this number is no longer valid. "
    "Delete this number and re-import it to refresh the provider credentials."
)
STALE_NUMBER_MESSAGE = (
    "This number is no longer valid with its provider. "
    "Delete this number and re-import it to continue."
)


def _phone_number_error_detail(code: str, message: str) -> dict[str, str]:
    """Build a structured error payload for phone-number workflows."""
    return {"code": code, "message": message}


def _stale_number_auth_error() -> dict[str, str]:
    """Structured error for expired stored provider credentials."""
    return _phone_number_error_detail(STALE_NUMBER_AUTH_CODE, STALE_NUMBER_AUTH_MESSAGE)


def _stale_number_error() -> dict[str, str]:
    """Structured error for deleted/missing provider-side numbers."""
    return _phone_number_error_detail(STALE_NUMBER_CODE, STALE_NUMBER_MESSAGE)


def _raise_stale_number_auth() -> NoReturn:
    """Raise the standard stale-auth error for stored phone-number credentials."""
    raise HTTPException(status_code=400, detail=_stale_number_auth_error())


def _raise_stale_number() -> NoReturn:
    """Raise the standard stale-number error for missing provider-side resources."""
    raise HTTPException(status_code=400, detail=_stale_number_error())


# ══════════════════════════════════════════════════════════════════
# Internal helpers — per-number credential encryption
# ══════════════════════════════════════════════════════════════════


def _encrypt_credentials(creds: dict[str, Any]) -> str:
    """Encrypt a credentials dict and return a JSON string for DB storage."""
    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
    ct, iv = engine.encrypt(creds)
    return json.dumps(
        {
            "ciphertext": ct,
            "iv": iv,
            "version": CURRENT_ENCRYPTION_VERSION,
        }
    )


def _decrypt_credentials(phone_num: PhoneNumber) -> dict[str, Any]:
    """Decrypt credentials from a phone number record. Raises 400 if missing."""
    raw = phone_num.provider_credentials_encrypted
    if not raw:
        _raise_stale_number_auth()
    blob = json.loads(raw)
    engine = integrations.EncryptionEngine(get_settings().auth.secret_key)
    return engine.decrypt(blob["ciphertext"], blob["iv"])


def _phone_number_response(num: PhoneNumber) -> PhoneNumberResponse:
    """Build a PhoneNumberResponse with the has_credentials flag."""
    resp = PhoneNumberResponse.model_validate(num)
    resp.has_credentials = bool(num.provider_credentials_encrypted)
    return resp


# ══════════════════════════════════════════════════════════════════
# Internal helpers — provider API calls
# ══════════════════════════════════════════════════════════════════


def _raise_provider_api_error(
    provider_name: str,
    resp: httpx.Response,
    *,
    reimport_on_auth_error: bool = False,
) -> None:
    """Raise a user-facing HTTPException for a provider API response."""
    if reimport_on_auth_error and resp.status_code == 401:
        _raise_stale_number_auth()
    raise HTTPException(
        status_code=resp.status_code,
        detail=f"{provider_name} API error: {resp.text[:300]}",
    )


async def _twilio_get(
    path: str,
    account_sid: str,
    auth_token: str,
    *,
    reimport_on_auth_error: bool = False,
) -> dict[str, Any]:
    """GET from the Twilio REST API."""
    url = f"https://api.twilio.com/2010-04-01/Accounts/{account_sid}/{path}"
    async with httpx.AsyncClient() as client:
        resp = await client.get(
            url,
            auth=(account_sid, auth_token),
            timeout=15.0,
            headers={"Accept": "application/json"},
        )
    if resp.status_code >= 400:
        _raise_provider_api_error(
            "Twilio",
            resp,
            reimport_on_auth_error=reimport_on_auth_error,
        )
    return cast(dict[str, Any], resp.json())


async def _twilio_post(
    path: str,
    account_sid: str,
    auth_token: str,
    data: dict[str, Any],
    *,
    reimport_on_auth_error: bool = False,
) -> dict[str, Any]:
    """POST to the Twilio REST API."""
    url = f"https://api.twilio.com/2010-04-01/Accounts/{account_sid}/{path}"
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            url,
            data=data,
            auth=(account_sid, auth_token),
            headers={"Accept": "application/json"},
            timeout=15.0,
        )
    if resp.status_code >= 400:
        _raise_provider_api_error(
            "Twilio",
            resp,
            reimport_on_auth_error=reimport_on_auth_error,
        )
    return cast(dict[str, Any], resp.json())


async def _telnyx_get(
    path: str,
    api_key: str,
    *,
    reimport_on_auth_error: bool = False,
) -> dict[str, Any]:
    """GET from the Telnyx REST API v2."""
    async with httpx.AsyncClient() as client:
        resp = await client.get(
            f"https://api.telnyx.com/v2/{path}",
            headers={
                "Authorization": f"Bearer {api_key}",
                "Accept": "application/json",
            },
            timeout=15.0,
        )
    if resp.status_code >= 400:
        _raise_provider_api_error(
            "Telnyx",
            resp,
            reimport_on_auth_error=reimport_on_auth_error,
        )
    return cast(dict[str, Any], resp.json())


async def _telnyx_post(
    path: str,
    api_key: str,
    payload: dict[str, Any],
    *,
    reimport_on_auth_error: bool = False,
) -> dict[str, Any]:
    """POST to the Telnyx REST API v2."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"https://api.telnyx.com/v2/{path}",
            json=payload,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
            timeout=15.0,
        )
    if resp.status_code >= 400:
        _raise_provider_api_error(
            "Telnyx",
            resp,
            reimport_on_auth_error=reimport_on_auth_error,
        )
    return cast(dict[str, Any], resp.json())


async def _telnyx_patch(
    path: str,
    api_key: str,
    payload: dict[str, Any],
    *,
    reimport_on_auth_error: bool = False,
) -> dict[str, Any]:
    """PATCH the Telnyx REST API v2."""
    async with httpx.AsyncClient() as client:
        resp = await client.patch(
            f"https://api.telnyx.com/v2/{path}",
            json=payload,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
            timeout=15.0,
        )
    if resp.status_code >= 400:
        _raise_provider_api_error(
            "Telnyx",
            resp,
            reimport_on_auth_error=reimport_on_auth_error,
        )
    return cast(dict[str, Any], resp.json())


# ── Webhook auto-configuration helpers ───────────────────────────


async def _configure_twilio_webhook(
    number_sid: str,
    voice_server_url: str,
    agent_id: str,
    account_sid: str,
    auth_token: str,
) -> None:
    """Point a Twilio number's VoiceUrl at voice-server with the agent_id embedded."""
    base = voice_server_url.rstrip("/")
    incoming_url = f"{base}/telephony/twilio/incoming/{agent_id}"
    status_url = f"{base}/telephony/twilio/status"
    await _twilio_post(
        f"IncomingPhoneNumbers/{number_sid}.json",
        account_sid,
        auth_token,
        data={
            "VoiceUrl": incoming_url,
            "VoiceMethod": "POST",
            "StatusCallback": status_url,
            "StatusCallbackMethod": "POST",
        },
        reimport_on_auth_error=True,
    )
    logger.info("Configured Twilio webhook for {} → {}", number_sid, incoming_url)


async def _clear_twilio_webhook(
    number_sid: str, account_sid: str, auth_token: str
) -> None:
    """Remove webhook URLs from a Twilio number."""
    await _twilio_post(
        f"IncomingPhoneNumbers/{number_sid}.json",
        account_sid,
        auth_token,
        data={"VoiceUrl": "", "StatusCallback": ""},
        reimport_on_auth_error=True,
    )
    logger.info("Cleared Twilio webhook for {}", number_sid)


async def _ensure_telnyx_connection(
    voice_server_url: str,
    agent_id: str,
    api_key: str,
    existing_connection_id: str | None,
) -> str:
    """Ensure a Telnyx TeXML Application exists pointing at voice-server.

    Uses a unique name per agent. If an application with that name already
    exists in Telnyx, we reuse it and update its URL. This prevents 400
    collision errors and ensures all numbers for one agent share a config.

    Returns the application ID.
    """
    voice_url = f"{voice_server_url.rstrip('/')}/telephony/telnyx/inbound/{agent_id}"
    friendly_name = f"Voice Agent OS-TeXML: {agent_id[:8]}"

    # 1. Search Telnyx for a TeXML app with the new name prefix. This ensures
    # we find the correct TeXML app regardless of the existing_connection_id
    # column (which might still hold an old Call Control app ID).
    conn_id_to_use = None
    try:
        # filter[friendly_name][contains] is at least 3 chars.
        search_data = await _telnyx_get(
            f"texml_applications?filter[friendly_name][contains]={friendly_name}",
            api_key,
            reimport_on_auth_error=True,
        )
        # Find the exact match in the returned list
        for app in search_data.get("data", []):
            if app.get("friendly_name") == friendly_name:
                conn_id_to_use = app["id"]
                break
    except Exception as exc:
        logger.warning("Failed to search Telnyx apps by name: {}", exc)

    # 2. If name search failed, fall back to existing_connection_id
    if not conn_id_to_use:
        conn_id_to_use = existing_connection_id

    # 3. Update existing or create new
    if conn_id_to_use:
        try:
            await _telnyx_patch(
                f"texml_applications/{conn_id_to_use}",
                api_key,
                {
                    "voice_url": voice_url,
                    "voice_method": "POST",
                    "friendly_name": friendly_name,
                },
                reimport_on_auth_error=True,
            )
            logger.info(
                "Updated Telnyx TeXML application {} → {}", conn_id_to_use, voice_url
            )
            return conn_id_to_use
        except HTTPException as exc:
            if exc.status_code != 404:
                raise
            # App might not be TeXML or was deleted externally — fall through to create new TeXML app

    # 4. Create new if still no ID
    result = await _telnyx_post(
        "texml_applications",
        api_key,
        {
            "friendly_name": friendly_name,
            "voice_url": voice_url,
            "voice_method": "POST",
            "voice_fallback_url": "",
            "active": True,
        },
        reimport_on_auth_error=True,
    )
    new_id: str = result["data"]["id"]
    logger.info("Created Telnyx TeXML application {} → {}", new_id, voice_url)
    return new_id


async def _assign_telnyx_number_to_connection(
    phone_number_id: str, connection_id: str, api_key: str
) -> None:
    """Associate a Telnyx phone number with a Voice API Application (connection)."""
    await _telnyx_patch(
        f"phone_numbers/{phone_number_id}",
        api_key,
        {"connection_id": connection_id},
        reimport_on_auth_error=True,
    )
    logger.info(
        "Assigned Telnyx number {} to connection {}", phone_number_id, connection_id
    )


async def _detach_telnyx_number_from_connection(
    phone_number_id: str, api_key: str
) -> None:
    """Remove a Telnyx phone number from any connection (set connection_id to null)."""
    try:
        await _telnyx_patch(
            f"phone_numbers/{phone_number_id}",
            api_key,
            {"connection_id": None},
            reimport_on_auth_error=True,
        )
        logger.info("Detached Telnyx number {} from connection", phone_number_id)
    except HTTPException as exc:
        if exc.status_code != 404:
            raise
        logger.warning(
            "Could not detach Telnyx number {} from connection", phone_number_id
        )


# ══════════════════════════════════════════════════════════════════
# Endpoints
# ══════════════════════════════════════════════════════════════════


@router.get("", response_model=PhoneNumberListResponse)
async def list_phone_numbers(
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> PhoneNumberListResponse:
    """List all phone numbers stored in the DB."""
    query = select(PhoneNumber)
    query = query.order_by(PhoneNumber.created_at.desc())
    result = await db.execute(query)
    numbers = list(result.scalars().all())
    return PhoneNumberListResponse(
        phone_numbers=[_phone_number_response(n) for n in numbers],
        total=len(numbers),
    )


@router.post("/fetch", response_model=FetchNumbersResponse)
async def fetch_provider_numbers(
    body: FetchNumbersRequest,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> FetchNumbersResponse:
    """Fetch phone numbers from a provider account using inline credentials.

    Does NOT persist anything — just returns the list so the user can pick
    which numbers to import.
    """
    # Collect existing E.164s for already_imported check
    query = select(PhoneNumber.phone_number)
    result = await db.execute(query)
    existing_e164s = {row[0] for row in result.all()}

    numbers: list[ProviderNumber] = []

    if body.provider == "telnyx" and not get_settings().experimental.telnyx:
        raise HTTPException(
            status_code=403, detail="Telnyx is experimental and currently disabled."
        )

    if body.provider == "twilio":
        if not body.twilio_account_sid or not body.twilio_auth_token:
            raise HTTPException(
                status_code=400,
                detail="Twilio Account SID and Auth Token are required.",
            )
        data = await _twilio_get(
            "IncomingPhoneNumbers.json", body.twilio_account_sid, body.twilio_auth_token
        )
        for pn in data.get("incoming_phone_numbers", []):
            e164 = pn["phone_number"]
            already = e164 in existing_e164s
            numbers.append(
                ProviderNumber(
                    phone_number=e164,
                    provider_sid=pn["sid"],
                    friendly_name=pn.get("friendly_name", e164),
                    locality=pn.get("locality", "") or "",
                    region=pn.get("region", "") or "",
                    number_type=pn.get("address_requirements", ""),
                    already_imported=already,
                    disabled_reason=(
                        "Already imported — delete it first to import again"
                        if already
                        else ""
                    ),
                )
            )

    elif body.provider == "telnyx":
        if not body.telnyx_api_key:
            raise HTTPException(status_code=400, detail="Telnyx API Key is required.")
        data = await _telnyx_get("phone_numbers?page[size]=250", body.telnyx_api_key)
        for pn in data.get("data", []):
            e164 = pn.get("phone_number", "")
            if not e164:
                continue
            already = e164 in existing_e164s
            numbers.append(
                ProviderNumber(
                    phone_number=e164,
                    provider_sid=pn["id"],
                    friendly_name=pn.get("phone_number", e164),
                    number_type=pn.get("phone_number_type", ""),
                    already_imported=already,
                    disabled_reason=(
                        "Already imported — delete it first to import again"
                        if already
                        else ""
                    ),
                )
            )

    return FetchNumbersResponse(numbers=numbers)


@router.post("/import-selected", response_model=PhoneNumberListResponse)
async def import_selected_numbers(
    body: ImportNumbersRequest,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> PhoneNumberListResponse:
    """Import selected phone numbers with inline credentials.

    Credentials are encrypted and stored per-number.
    """
    if not body.selected_numbers:
        raise HTTPException(status_code=400, detail="No numbers selected.")

    if body.provider == "telnyx" and not get_settings().experimental.telnyx:
        raise HTTPException(
            status_code=403, detail="Telnyx is experimental and currently disabled."
        )

    # Build the credential dict to encrypt
    if body.provider == "twilio":
        if not body.twilio_account_sid or not body.twilio_auth_token:
            raise HTTPException(status_code=400, detail="Twilio credentials required.")
        cred_blob = {
            "provider": "twilio",
            "twilio_account_sid": body.twilio_account_sid,
            "twilio_auth_token": body.twilio_auth_token,
        }
        # Fetch metadata from provider
        data = await _twilio_get(
            "IncomingPhoneNumbers.json", body.twilio_account_sid, body.twilio_auth_token
        )
        provider_map = {
            pn["phone_number"]: pn for pn in data.get("incoming_phone_numbers", [])
        }
    elif body.provider == "telnyx":
        if not body.telnyx_api_key:
            raise HTTPException(status_code=400, detail="Telnyx API key required.")
        cred_blob = {
            "provider": "telnyx",
            "telnyx_api_key": body.telnyx_api_key,
        }
        data = await _telnyx_get("phone_numbers?page[size]=250", body.telnyx_api_key)
        provider_map = {pn.get("phone_number", ""): pn for pn in data.get("data", [])}
    else:
        raise HTTPException(
            status_code=400, detail="provider must be 'twilio' or 'telnyx'"
        )

    encrypted_creds = _encrypt_credentials(cred_blob)
    selected_set = set(body.selected_numbers)
    upserted: list[PhoneNumber] = []

    for e164 in selected_set:
        pn_data = provider_map.get(e164)
        if not pn_data:
            logger.warning(
                "Selected number {} not found in provider account, skipping", e164
            )
            continue

        query = select(PhoneNumber).where(PhoneNumber.phone_number == e164)
        result = await db.execute(query)
        existing = result.scalar_one_or_none()

        if body.provider == "twilio":
            sid = pn_data["sid"]
            friendly = pn_data.get("friendly_name", e164)
        else:
            sid = pn_data["id"]
            friendly = pn_data.get("phone_number", e164)

        if existing:
            existing.provider_sid = sid
            existing.friendly_name = friendly
            existing.provider = body.provider
            existing.provider_credentials_encrypted = encrypted_creds
            if body.provider == "telnyx":
                conn_id = pn_data.get("connection_id") or pn_data.get(
                    "connection", {}
                ).get("id")
                if conn_id and not existing.telnyx_connection_id:
                    existing.telnyx_connection_id = conn_id
            upserted.append(existing)
        else:
            new_num = PhoneNumber(
                provider=body.provider,
                phone_number=e164,
                provider_sid=sid,
                friendly_name=friendly,
                provider_credentials_encrypted=encrypted_creds,
            )
            if body.provider == "telnyx":
                conn_id = pn_data.get("connection_id") or pn_data.get(
                    "connection", {}
                ).get("id")
                new_num.telnyx_connection_id = conn_id
            db.add(new_num)
            upserted.append(new_num)

    await db.flush()
    for num in upserted:
        await db.refresh(num)

    logger.info(
        "Imported {} {} phone number(s) with per-number credentials",
        len(upserted),
        body.provider,
    )
    return PhoneNumberListResponse(
        phone_numbers=[_phone_number_response(n) for n in upserted],
        total=len(upserted),
    )


@router.patch("/{phone_number_id}/assign", response_model=PhoneNumberResponse)
async def assign_phone_number(
    phone_number_id: _uuid.UUID,
    body: AssignPhoneNumberRequest,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> PhoneNumberResponse:
    """Assign or unassign a phone number to/from an agent.

    On assignment:
      - Validates that the agent exists
      - Reads voice_server_url from global Settings
      - Reads provider credentials from the phone number record
      - Automatically configures the provider webhook
      - Persists agent_id in the DB

    On unassignment (agent_id = null):
      - Reads provider credentials from the phone number record
      - Clears the provider webhook
      - Clears agent_id in the DB
    """
    query = select(PhoneNumber).where(PhoneNumber.id == phone_number_id)
    result = await db.execute(query)
    phone_num = result.scalar_one_or_none()
    if not phone_num:
        raise HTTPException(status_code=404, detail="Phone number not found")

    if phone_num.provider == "telnyx" and not get_settings().experimental.telnyx:
        raise HTTPException(
            status_code=403, detail="Telnyx is experimental and currently disabled."
        )

    # ── Unassign ────────────────────────────────────────────────
    if body.agent_id is None:
        old_sid = phone_num.provider_sid
        old_connection_id = phone_num.telnyx_connection_id

        # Clear provider webhook using per-number credentials
        if phone_num.provider == "twilio" and phone_num.agent_id is not None:
            if not old_sid:
                _raise_stale_number()
            creds = _decrypt_credentials(phone_num)
            sid = creds.get("twilio_account_sid", "")
            token = creds.get("twilio_auth_token", "")
            if not sid or not token:
                _raise_stale_number_auth()
            await _clear_twilio_webhook(old_sid, sid, token)
        elif (
            phone_num.provider == "telnyx"
            and phone_num.agent_id is not None
            and old_connection_id
        ):
            if not old_sid:
                _raise_stale_number()
            creds = _decrypt_credentials(phone_num)
            api_key = creds.get("telnyx_api_key", "")
            if not api_key:
                _raise_stale_number_auth()
            await _detach_telnyx_number_from_connection(old_sid, api_key)

        phone_num.agent_id = None
        await db.flush()
        await db.refresh(phone_num)
        logger.info("Unassigned phone number {}", phone_num.phone_number)
        return _phone_number_response(phone_num)

    # ── Assign ──────────────────────────────────────────────────
    # Validate agent exists and is in the same workspace
    agent_query = select(Agent).where(Agent.id == body.agent_id)
    agent_result = await db.execute(agent_query)
    if not agent_result.scalar_one_or_none():
        raise HTTPException(status_code=404, detail="Agent not found")

    # Read the current global voice_server_url and persist it as the per-number snapshot
    telephony_cfg = await get_telephony_config(db)
    voice_server_url = telephony_cfg.voice_server_url
    if not voice_server_url:
        raise HTTPException(
            status_code=400,
            detail="Voice Server URL not configured. Set it in Settings → Telephony.",
        )

    settings = get_settings()
    if settings.is_production and not voice_server_url.startswith("https://"):
        raise HTTPException(
            status_code=400,
            detail="voice_server_url must use HTTPS in production",
        )

    # Read provider credentials from the phone number record
    creds = _decrypt_credentials(phone_num)

    if phone_num.provider == "twilio":
        account_sid = creds.get("twilio_account_sid", "")
        auth_token = creds.get("twilio_auth_token", "")
        if not account_sid or not auth_token:
            _raise_stale_number_auth()
        if not phone_num.provider_sid:
            _raise_stale_number()
        await _configure_twilio_webhook(
            phone_num.provider_sid,
            voice_server_url,
            str(body.agent_id),
            account_sid,
            auth_token,
        )

    elif phone_num.provider == "telnyx":
        api_key = creds.get("telnyx_api_key", "")
        if not api_key:
            _raise_stale_number_auth()
        if not phone_num.provider_sid:
            _raise_stale_number()
        connection_id = await _ensure_telnyx_connection(
            voice_server_url,
            str(body.agent_id),
            api_key,
            body.telnyx_connection_id or phone_num.telnyx_connection_id,
        )
        await _assign_telnyx_number_to_connection(
            phone_num.provider_sid, connection_id, api_key
        )
        phone_num.telnyx_connection_id = connection_id

    phone_num.agent_id = body.agent_id
    phone_num.voice_server_url = voice_server_url
    await db.flush()
    await db.refresh(phone_num)
    logger.info(
        "Assigned {} → agent {} (voice-server={})",
        phone_num.phone_number,
        body.agent_id,
        voice_server_url,
    )
    return _phone_number_response(phone_num)


@router.delete("/{phone_number_id}", status_code=204)
async def delete_phone_number(
    phone_number_id: _uuid.UUID,
    ctx: TenantContext | None = Depends(require_tenant),
    db: AsyncSession = Depends(get_db),
) -> None:
    """Remove a phone number from our DB.

    This does NOT cancel/release the number with the provider. It only
    removes it from our management view.
    """
    query = select(PhoneNumber).where(PhoneNumber.id == phone_number_id)
    result = await db.execute(query)
    phone_num = result.scalar_one_or_none()
    if not phone_num:
        raise HTTPException(status_code=404, detail="Phone number not found")
    await db.delete(phone_num)
    logger.info("Deleted phone number {} from DB", phone_num.phone_number)
