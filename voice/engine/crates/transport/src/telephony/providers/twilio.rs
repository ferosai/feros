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
            _ => return Err(TransportError::SendFailed("Invalid credentials for Twilio provider".into())),
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
    async fn initiate_supervised_transfer(
        &self,
        config: &super::super::config::TelephonyConfig,
        original_call_id: &str,
        destination: &str,
        from_number: &str,
        conference_name: &str,
        transfer_callback_url: &str,
    ) -> Result<String, TransportError> {
        let (account_sid, auth_token) = match &config.credentials {
            super::super::config::TelephonyCredentials::Twilio {
                account_sid,
                auth_token,
            } => (account_sid.as_str(), auth_token.as_str()),
            _ => return Err(TransportError::SendFailed("Invalid credentials for Twilio provider".into())),
        };

        let mut callback_url = transfer_callback_url.to_string();
        if !callback_url.contains("original_call_id=") {
            let sep = if callback_url.contains('?') { "&" } else { "?" };
            callback_url.push_str(&format!("{}original_call_id={}", sep, original_call_id));
        }

        let twiml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
    <Dial>
        <Conference endConferenceOnExit="true">{}</Conference>
    </Dial>
</Response>"#,
            xml_escape(conference_name)
        );

        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json",
            account_sid
        );

        let mut form_data = vec![
            ("To", destination),
            ("From", from_number),
            ("Twiml", twiml.as_str()),
            ("Timeout", "30"),
            ("StatusCallback", transfer_callback_url),
            ("StatusCallbackMethod", "POST"),
        ];
        form_data.push(("StatusCallbackEvent", "initiated"));
        form_data.push(("StatusCallbackEvent", "ringing"));
        form_data.push(("StatusCallbackEvent", "answered"));
        form_data.push(("StatusCallbackEvent", "completed"));

        let resp = self.client
            .post(&endpoint)
            .basic_auth(account_sid, Some(auth_token))
            .form(&form_data)
            .send()
            .await
            .map_err(|e| {
                TransportError::SendFailed(format!("Twilio transfer outbound call failed: {}", e))
            })?;

        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();

        match status {
            200 | 201 => {
                // Extract the new call SID from the response JSON.
                let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let new_call_sid = json
                    .get("sid")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        warn!("[twilio] transfer call response missing 'sid'; raw body: {}", body);
                        String::new()
                    });
                info!(
                    "[twilio] Supervised transfer initiated: original={} → destination={}, conference={}, new_call_sid={}",
                    original_call_id, destination, conference_name, new_call_sid
                );
                Ok(new_call_sid)
            }
            _ => {
                error!(
                    "[twilio] Failed to initiate transfer outbound call: status={}, body={}",
                    status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Twilio transfer call failed: status={}",
                    status
                )))
            }
        }
    }

    /// Move the original caller's active call into the named Conference room.
    ///
    /// Called by the webhook handler once the destination has answered and
    /// is already waiting in the Conference. We update the original call's
    /// TwiML to `<Dial><Conference>` — this drops the caller into the room
    /// without any interruption on their end (no ring, no hold music gap).
    async fn bridge_call_to_conference(
        &self,
        config: &super::super::config::TelephonyConfig,
        call_id: &str,
        conference_id_or_name: &str,
    ) -> Result<String, TransportError> {
        let (account_sid, auth_token) = match &config.credentials {
            super::super::config::TelephonyCredentials::Twilio {
                account_sid,
                auth_token,
            } => (account_sid.as_str(), auth_token.as_str()),
            _ => {
                return Err(TransportError::SendFailed(
                    "Invalid credentials for Twilio provider".into(),
                ))
            }
        };

        // Twilio bridge is simple: we tell one leg to immediately execute new TwiML
        // which drops them into the named conference.
        let twiml = format!(
            "<Response><Dial><Conference>{}</Conference></Dial></Response>",
            xml_escape(conference_id_or_name)
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
                TransportError::SendFailed(format!("Twilio conference bridge failed: {}", e))
            })?;

        match resp.status().as_u16() {
            200 | 201 => {
                info!(
                    "[twilio] Bridged call {} into conference {}",
                    call_id, conference_id_or_name
                );
                Ok(conference_id_or_name.to_string())
            }
            404 => {
                warn!("[twilio] Call {} not found for conference bridge (404)", call_id);
                Err(TransportError::SendFailed(format!(
                    "Twilio conference bridge failed: Call {} not found (404)",
                    call_id
                )))
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                error!(
                    "[twilio] Failed to bridge call {} into conference {}: status={}, body={}",
                    call_id, conference_id_or_name, status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Twilio conference bridge failed: status={}",
                    status
                )))
            }
        }
    }
}
