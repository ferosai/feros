//! Voice Engine — standalone WebSocket server mode.
//!
//! Architecture: Event-Sourced Hub-and-Spoke (Actor Model)
//! Delegates to `server::run_server` for the actual WebSocket handling.

use std::net::SocketAddr;

use tracing::info;

use voice_engine::server::{run_server, ProviderConfig, ServerState, TelephonyCredentials};
use voice_engine::settings::Settings;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "voice_engine=debug,tower_http=debug".into()),
        )
        .init();

    // Parse settings from env vars (+ .env file if present)
    let settings = Settings::from_env().expect("Failed to parse settings from env");
    info!(?settings, "Loaded settings");

    // Build server state (handles rustls and ICE init internally)
    let providers = ProviderConfig {
        stt_url: settings.stt_base_url,
        stt_provider: settings.stt_provider,
        stt_model: settings.stt_model,
        stt_api_key: settings.stt_api_key,
        llm_url: settings.llm_base_url,
        llm_api_key: settings.llm_api_key,
        llm_model: settings.llm_model,
        llm_provider: settings.llm_provider,
        tts_url: settings.tts_base_url,
        tts_provider: settings.tts_provider,
        tts_model: settings.tts_model,
        tts_api_key: settings.tts_api_key,
        tts_voice_id: String::new(), // populated per-session from SessionConfig.voice_id
    };
    let telephony = TelephonyCredentials {
        twilio_account_sid: settings.twilio_account_sid,
        twilio_auth_token: settings.twilio_auth_token,
        telnyx_api_key: settings.telnyx_api_key,
        telnyx_connection_id: settings.telnyx_connection_id,
    };
    let auth_secret_key = std::env::var("AUTH__SECRET_KEY").unwrap_or_default();
    let state = ServerState::new(
        providers,
        telephony,
        settings.telnyx_public_key,
        auth_secret_key,
    );

    let addr: SocketAddr = format!("{}:{}", settings.listen_host, settings.listen_port)
        .parse()
        .expect("Invalid LISTEN_HOST/LISTEN_PORT");
    info!("Starting voice-engine in standalone mode");
    run_server(addr, state).await;
}
