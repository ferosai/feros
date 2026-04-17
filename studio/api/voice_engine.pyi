"""Type stubs for the ``voice_engine`` Rust/PyO3 native module.

Classes:
    SessionConfig  — per-session voice pipeline configuration
    ServerConfig   — server-level settings (bind address, providers, telephony)
    VoiceServer    — WebSocket voice server running in a background Rust thread
    AgentRunner    — headless agent backend for testing

Functions:
    validate_javascript — syntax-check a QuickJS tool script (feature: quickjs)
"""

from collections.abc import Callable
from typing import Any

class SessionConfig:
    """Per-session voice pipeline configuration."""

    agent_id: str

    def __init__(
        self,
        agent_id: str = "",
        temperature: float = 0.7,
        max_tokens: int = 32768,
        input_sample_rate: int = 48000,
        output_sample_rate: int = 24000,
        models_dir: str = "./dsp_models",
        smart_turn_threshold: float = 0.5,
        denoise_enabled: bool = True,
        denoise_backend: str = "rnnoise",
        smart_turn_enabled: bool = True,
        turn_completion_enabled: bool = True,
        idle_timeout_secs: int = 5,
        idle_max_retries: int = 2,
        min_barge_in_words: int = 2,
        barge_in_timeout_ms: int = 800,
        graph_json: str | None = None,
    ) -> None: ...

class ServerConfig:
    """Server-level settings for starting the voice server."""

    def __init__(
        self,
        host: str = "0.0.0.0",
        port: int = 8300,
        default_stt_url: str = "http://localhost:8100",
        default_stt_provider: str = "",
        default_stt_model: str = "",
        default_stt_api_key: str = "",
        default_llm_url: str = "http://localhost:11434/v1",
        default_llm_api_key: str = "",
        default_llm_model: str = "llama3.2",
        default_llm_provider: str = "",
        default_tts_url: str = "http://localhost:8200",
        default_tts_provider: str = "",
        default_tts_model: str = "",
        default_tts_api_key: str = "",
        default_twilio_account_sid: str = "",
        default_twilio_auth_token: str = "",
        default_telnyx_api_key: str = "",
        auth_secret_key: str = "",
    ) -> None: ...

class VoiceServer:
    """WebSocket voice server running in a background Rust thread."""

    @property
    def port(self) -> int: ...
    @staticmethod
    def start(config: ServerConfig) -> VoiceServer: ...
    def register_session(
        self,
        session_id: str,
        config: SessionConfig,
        stt_url: str,
        llm_url: str,
        llm_api_key: str,
        llm_model: str,
        tts_url: str,
        llm_provider: str = "",
        stt_provider: str = "",
        tts_provider: str = "",
        stt_model: str = "",
        tts_model: str = "",
        stt_api_key: str = "",
        tts_api_key: str = "",
    ) -> None: ...
    def pending_sessions(self) -> int: ...
    def stop(self) -> None: ...

class AgentRunner:
    """Headless agent backend for testing."""

    def __init__(
        self,
        llm_url: str,
        llm_api_key: str,
        llm_model: str,
        system_prompt: str,
        llm_provider: str = "",
        graph_json: str | None = None,
        before_tool_call: Callable[[str, str], str | None] | None = None,
        after_tool_call: Callable[[str, str, str], str | None] | None = None,
        temperature: float = 0.7,
        max_tokens: int = 32768,
        greeting: str | None = None,
        secrets: dict[str, str] | None = None,
        escalation_destinations_json: str | None = None,
    ) -> None: ...
    def send(self, text: str) -> list[Any]: ...
    def start_turn(self, text: str) -> None: ...
    # tool_call_completed events include `success` and `error_message`.
    # hang_up events include `reason` and optional `content`.
    def recv_event(self) -> dict[str, Any] | None: ...
    def cancel_handle(self) -> CancelHandle: ...

class CancelHandle:
    """Lightweight handle to cancel an in-flight AgentRunner turn from another thread."""

    def cancel(self) -> None: ...

def validate_javascript(script: str) -> list[str]:
    """Validate a JavaScript tool script using QuickJS (syntax-only, no execution).

    Returns an empty list if valid, or a list of error strings.
    """
    ...

def get_supported_languages() -> list[dict[str, str]]: ...
def get_elevenlabs_multilingual_models() -> list[str]: ...
def get_tts_model_catalog() -> list[dict[str, Any]]: ...
def check_tts_model_language(provider: str, model_id: str, language: str) -> bool: ...
def get_stt_model_catalog() -> list[dict[str, Any]]: ...
def check_stt_model_language(provider: str, model_id: str, language: str) -> bool: ...
def validate_rhai(script: str) -> list[str]: ...
