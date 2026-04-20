//! Twilio telephony provider — WebSocket Media Streams protocol + REST call control.
//!
//! Protocol reference: https://www.twilio.com/docs/voice/media-streams

use async_trait::async_trait;
use tracing::{error, info, warn};

use crate::error::TransportError;

use super::TelephonyProviderImpl;

/// Twilio Media Streams provider.
pub struct Twilio {
    /// Shared HTTP client — reused across all REST calls to benefit from connection pooling.
    client: reqwest::Client,
}

impl Twilio {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for Twilio {
    fn default() -> Self {
        Self::new()
    }
}

/// Escape a string for safe embedding in TwiML XML content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[async_trait]
impl TelephonyProviderImpl for Twilio {
    fn name(&self) -> &'static str {
        "twilio"
    }

    fn extract_stream_id(&self, start_json: &serde_json::Value) -> Option<String> {
        start_json
            .get("start")
            .and_then(|s| s.get("streamSid"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn extract_call_id(&self, start_json: &serde_json::Value) -> Option<String> {
        start_json
            .get("start")
            .and_then(|s| s.get("callSid"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn media_frame(&self, payload_b64: &str, stream_id: &str) -> serde_json::Value {
        serde_json::json!({
            "event": "media",
            "streamSid": stream_id,
            "media": { "payload": payload_b64 }
        })
    }

    fn clear_frame(&self, stream_id: &str) -> serde_json::Value {
        serde_json::json!({
            "event": "clear",
            "streamSid": stream_id,
        })
    }

    async fn hangup(
        &self,
        config: &super::super::config::TelephonyConfig,
        call_id: &str,
    ) -> Result<(), TransportError> {
        let (account_sid, auth_token) = match &config.credentials {
            super::super::config::TelephonyCredentials::Twilio {
                account_sid,
                auth_token,
            } => (account_sid.as_str(), auth_token.as_str()),
            #[cfg(feature = "telnyx")]
            super::super::config::TelephonyCredentials::Telnyx { .. } => {
                unreachable!("Telnyx credentials passed to Twilio hangup handler")
            }
        };

        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{}.json",
            account_sid, call_id
        );

        let resp = self.client
            .post(&endpoint)
            .basic_auth(account_sid, Some(auth_token))
            .form(&[("Status", "completed")])
            .send()
            .await
            .map_err(|e| {
                TransportError::SendFailed(format!("Twilio hangup request failed: {}", e))
            })?;

        match resp.status().as_u16() {
            200 => {
                info!("[twilio] Successfully terminated call {}", call_id);
                Ok(())
            }
            404 => {
                warn!("[twilio] Call {} already terminated (404)", call_id);
                Ok(())
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                error!(
                    "[twilio] Failed to terminate call {}: status={}, body={}",
                    call_id, status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Twilio hangup failed: status={}",
                    status
                )))
            }
        }
    }

    /// Place a new outbound call to `destination`.
    ///
    /// When the destination answers, Twilio automatically puts them into
    /// the Conference named `conference_name` (via inline TwiML).
    /// The `transfer_callback_url` receives StatusCallback events so the
    /// webhook handler can react to answered/busy/no-answer/failed outcomes.
    ///
    /// The original caller's WebSocket leg is **not touched** here — the AI
    /// session continues uninterrupted while the outbound leg is ringing.
    async fn blind_transfer(
        &self,
        config: &super::super::config::TelephonyConfig,
        call_id: &str,
        destination: &str,
    ) -> Result<(), TransportError> {
        let (account_sid, auth_token) = match &config.credentials {
            super::super::config::TelephonyCredentials::Twilio {
                account_sid,
                auth_token,
            } => (account_sid.as_str(), auth_token.as_str()),
            #[cfg(feature = "telnyx")]
            super::super::config::TelephonyCredentials::Telnyx { .. } => {
                unreachable!("Telnyx credentials passed to Twilio blind_transfer handler")
            }
        };

        let twiml = format!(
            "<Response><Dial>{}</Dial></Response>",
            xml_escape(destination)
        );

        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{}.json",
            account_sid, call_id
        );

        let resp = self.client
            .post(&endpoint)
            .basic_auth(account_sid, Some(auth_token))
            .form(&[("Twiml", twiml.as_str())])
            .send()
            .await
            .map_err(|e| {
                TransportError::SendFailed(format!("Twilio blind transfer failed: {}", e))
            })?;

        match resp.status().as_u16() {
            200 | 201 => {
                info!(
                    "[twilio] Blind transfer initiated successfully: call={} → destination={}",
                    call_id, destination
                );
                Ok(())
            }
            404 => {
                warn!("[twilio] Call {} not found for blind transfer (404)", call_id);
                Err(TransportError::SendFailed(format!(
                    "Twilio blind transfer failed: Call {} not found (404)",
                    call_id
                )))
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                error!(
                    "[twilio] Failed to initiate blind transfer: status={}, body={}",
                    status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Twilio blind transfer failed: status={}",
                    status
                )))
            }
        }
    }
}
