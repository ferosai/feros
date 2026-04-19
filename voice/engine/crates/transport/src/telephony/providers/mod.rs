//! Telephony provider trait and implementations.
//!
//! This module defines [`TelephonyProviderImpl`] — the trait that abstracts
//! the differences between Twilio, Telnyx, and any future telephony providers.
//! Each provider implements this trait in its own submodule.

pub mod telnyx;
pub mod twilio;

use async_trait::async_trait;

use crate::error::TransportError;

use super::config::{TelephonyConfig, TelephonyCredentials};

/// Trait that each telephony provider (Twilio, Telnyx, etc.) implements.
///
/// Covers four concerns:
/// 1. **JSON framing** — how to build outbound media/clear frames
/// 2. **Event parsing** — how to extract stream/call IDs from the `start` event
/// 3. **Unsupervised call control** — hang up
/// 4. **Supervised transfer** — place an outbound call to `destination`;
///    when they answer, put them into a named Conference room.
///    The original caller is then bridged into the same room by the webhook
///    handler (`ServerState::telephony_transfer_status_handler`), and our
///    WebSocket leg is terminated cleanly *after* the bridge is complete.
#[async_trait]
pub trait TelephonyProviderImpl: Send + Sync {
    /// Human-readable provider name (for logging).
    fn name(&self) -> &'static str;

    /// Extract the stream identifier from the JSON `start` event.
    fn extract_stream_id(&self, start_json: &serde_json::Value) -> Option<String>;

    /// Extract the call identifier from the JSON `start` event.
    fn extract_call_id(&self, start_json: &serde_json::Value) -> Option<String>;

    /// Build a JSON media frame for sending audio to the provider.
    fn media_frame(&self, payload_b64: &str, stream_id: &str) -> serde_json::Value;

    /// Build a JSON clear frame for interrupting audio playback (barge-in).
    fn clear_frame(&self, stream_id: &str) -> serde_json::Value;

    /// Extract a named custom parameter from the JSON `start` event.
    /// For Twilio, these come from `start.customParameters`.
    fn extract_custom_param(&self, start_json: &serde_json::Value, name: &str) -> Option<String> {
        // Default: try start.customParameters.<name> (Twilio convention)
        start_json
            .get("start")
            .and_then(|s| s.get("customParameters"))
            .and_then(|p| p.get(name))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Hang up the call unconditionally via the provider's REST API.
    async fn hangup(&self, config: &TelephonyConfig, call_id: &str) -> Result<(), TransportError>;

    /// Perform a blind transfer to another number.
    ///
    /// This immediately instructs the telephony provider to drop the current
    /// AI websocket and connect the caller to the new destination. Return OK
    /// if the transfer command is successfully accepted by the provider.
    async fn blind_transfer(
        &self,
        config: &TelephonyConfig,
        call_id: &str,
        destination: &str,
    ) -> Result<(), TransportError>;
}

/// Create a boxed provider implementation from the enum variant.
pub fn create_provider(credentials: &TelephonyCredentials) -> Box<dyn TelephonyProviderImpl> {
    match credentials {
        TelephonyCredentials::Twilio { .. } => Box::new(twilio::Twilio::new()),
        TelephonyCredentials::Telnyx { .. } => Box::new(telnyx::Telnyx::new()),
    }
}
