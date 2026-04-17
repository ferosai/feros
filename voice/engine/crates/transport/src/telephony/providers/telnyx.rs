//! Telnyx telephony provider — WebSocket media protocol + REST call control.
//!
//! Protocol reference: https://developers.telnyx.com/docs/voice/media-streaming

use async_trait::async_trait;
use base64::Engine;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use tracing::{error, info, warn};

use crate::error::TransportError;

use super::TelephonyProviderImpl;

/// Telnyx Media Streams provider.
pub struct Telnyx {
    /// Shared HTTP client — reused across all REST calls to benefit from connection pooling.
    client: reqwest::Client,
}

impl Telnyx {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for Telnyx {
    fn default() -> Self {
        Self::new()
    }
}

/// Characters that must be percent-encoded in URL query-parameter values.
/// Encodes everything outside the RFC 3986 unreserved set that would break
/// query string parsing: `&`, `=`, `+`, `#`, `%`, plus space.
/// Hyphens, underscores, tildes and dots are left as-is for readability.
const QUERY_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b']');

#[async_trait]
impl TelephonyProviderImpl for Telnyx {
    fn name(&self) -> &'static str {
        "telnyx"
    }

    fn extract_stream_id(&self, start_json: &serde_json::Value) -> Option<String> {
        start_json
            .get("stream_id")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn extract_call_id(&self, start_json: &serde_json::Value) -> Option<String> {
        start_json
            .get("start")
            .and_then(|s| s.get("call_control_id"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn extract_custom_param(&self, start_json: &serde_json::Value, name: &str) -> Option<String> {
        start_json
            .get("start")
            .and_then(|s| s.get("custom_parameters"))
            .and_then(|p| p.get(name))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn media_frame(&self, payload_b64: &str, stream_id: &str) -> serde_json::Value {
        serde_json::json!({
            "event": "media",
            "stream_id": stream_id,
            "media": { "payload": payload_b64 }
        })
    }

    fn clear_frame(&self, stream_id: &str) -> serde_json::Value {
        serde_json::json!({
            "event": "clear",
            "stream_id": stream_id,
        })
    }

    async fn hangup(
        &self,
        config: &super::super::config::TelephonyConfig,
        call_id: &str,
    ) -> Result<(), TransportError> {
        let api_key = match &config.credentials {
            super::super::config::TelephonyCredentials::Telnyx { api_key, .. } => api_key.as_str(),
            _ => return Err(TransportError::SendFailed("Invalid credentials for Telnyx provider".into())),
        };

        let endpoint = format!("https://api.telnyx.com/v2/calls/{}/actions/hangup", call_id);

        let resp = self.client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .map_err(|e| {
                TransportError::SendFailed(format!("Telnyx hangup request failed: {}", e))
            })?;

        match resp.status().as_u16() {
            200 => {
                info!("[telnyx] Successfully terminated call {}", call_id);
                Ok(())
            }
            422 => {
                warn!("[telnyx] Call {} already terminated (422)", call_id);
                Ok(())
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                error!(
                    "[telnyx] Failed to terminate call {}: status={}, body={}",
                    call_id, status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Telnyx hangup failed: status={}",
                    status
                )))
            }
        }
    }

    /// Conference-based supervised transfer using Telnyx Call Control API.
    ///
    /// Telnyx does not support inline TwiML for outbound calls; instead we:
    /// 1. Create a new outbound call via `POST /v2/calls` with a `webhook_url`
    ///    pointing at `transfer_callback_url`.
    /// 2. Once the outbound call answers (Telnyx sends `call.answered` webhook),
    ///    the webhook handler calls `conference.join` on the Telnyx Call Control API
    ///    for both legs.
    ///
    /// Returns the `call_control_id` of the outbound leg.
    async fn initiate_supervised_transfer(
        &self,
        config: &super::super::config::TelephonyConfig,
        original_call_id: &str,
        destination: &str,
        from_number: &str,
        conference_name: &str,
        transfer_callback_url: &str,
    ) -> Result<String, TransportError> {
        let (api_key, connection_id) = match &config.credentials {
            super::super::config::TelephonyCredentials::Telnyx { api_key, connection_id } => {
                (api_key.as_str(), connection_id.as_str())
            }
            _ => return Err(TransportError::SendFailed("Invalid credentials for Telnyx provider".into())),
        };

        let client_state = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_string(&serde_json::json!({
                "original_call_id": original_call_id,
                "conference_name": conference_name,
            })).map_err(|e| TransportError::SendFailed(e.to_string()))?
        );

        let payload = serde_json::json!({
            "connection_id": connection_id,
            "to": destination,
            "from": from_number,
            "webhook_url": transfer_callback_url,
            "webhook_url_method": "POST",
            "client_state": client_state,
        });

        let resp = self.client
            .post("https://api.telnyx.com/v2/calls")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| {
                TransportError::SendFailed(format!("Telnyx transfer outbound call failed: {}", e))
            })?;

        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();

        match status {
            200 | 201 => {
                let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let new_call_id = json
                    .pointer("/data/call_control_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        warn!(
                            "[telnyx] transfer call response missing /data/call_control_id; raw body: {}",
                            body
                        );
                        String::new()
                    });
                info!(
                    "[telnyx] Supervised transfer initiated: original={} → destination={}, conference={}, new_call_id={}",
                    original_call_id, destination, conference_name, new_call_id
                );
                Ok(new_call_id)
            }
            _ => {
                error!(
                    "[telnyx] Failed to initiate transfer outbound call: status={}, body={}",
                    status, body
                );
                Err(TransportError::SendFailed(format!(
                    "Telnyx transfer call failed: status={}",
                    status
                )))
            }
        }
    }

    /// Join the given call_control_id into a Telnyx Conference room.
    ///
    /// Accepts either a conference UUID (bypasses list lookup, immune to eventual
    /// consistency) or a conference name (falls back to filter[name] search and
    /// then creates the conference if absent).  Returns the authoritative UUID.
    async fn bridge_call_to_conference(
        &self,
        config: &super::super::config::TelephonyConfig,
        call_id: &str,
        conference_id_or_name: &str,
    ) -> Result<String, TransportError> {
        let api_key = match &config.credentials {
            super::super::config::TelephonyCredentials::Telnyx { api_key, .. } => api_key.as_str(),
            _ => return Err(TransportError::SendFailed("Invalid credentials for Telnyx provider".into())),
        };

        let is_uuid = uuid::Uuid::parse_str(conference_id_or_name).is_ok();
        let mut existing_conf_id = None;

        if is_uuid {
            info!(
                "[telnyx] Provided conference identifier {} is a UUID, bypassing list search",
                conference_id_or_name
            );
            existing_conf_id = Some(conference_id_or_name.to_string());
        } else {
            // Step 1: Check if the conference already exists by name.
            let encoded_name = utf8_percent_encode(conference_id_or_name, QUERY_VALUE).to_string();
            let filter_url = format!("https://api.telnyx.com/v2/conferences?filter[name]={}", encoded_name);
            let list_resp = self.client
                .get(&filter_url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| {
                    TransportError::SendFailed(format!("Telnyx list conferences failed: {}", e))
                })?;

            let list_status = list_resp.status().as_u16();
            let list_body = list_resp.text().await.unwrap_or_default();

            if matches!(list_status, 200 | 201) {
                let json: serde_json::Value = serde_json::from_str(&list_body).unwrap_or_default();
                if let Some(data) = json.pointer("/data").and_then(|v| v.as_array()) {
                    if let Some(id) = data.first().and_then(|v| v.pointer("/id")).and_then(|v| v.as_str()) {
                        existing_conf_id = Some(id.to_string());
                    } else if !data.is_empty() {
                        warn!("[telnyx] conference list entry missing /id; raw body: {}", list_body);
                    }
                }
            }
        }

        if let Some(conf_id) = existing_conf_id {
            // Step 2a: Conference exists. Join the call leg into the conference.
            let join_endpoint = format!(
                "https://api.telnyx.com/v2/conferences/{}/actions/join",
                conf_id
            );
            let join_payload = serde_json::json!({"call_control_id": call_id});
            let join_resp = self.client
                .post(&join_endpoint)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .body(join_payload.to_string())
                .send()
                .await
                .map_err(|e| {
                    TransportError::SendFailed(format!("Telnyx conference join failed: {}", e))
                })?;

            match join_resp.status().as_u16() {
                200 | 201 => {
                    info!(
                        "[telnyx] Bridged call {} into existing conference {} (id={})",
                        call_id, conference_id_or_name, conf_id
                    );
                    Ok(conf_id)
                }
                status => {
                    let body = join_resp.text().await.unwrap_or_default();
                    error!(
                        "[telnyx] Failed to bridge call {} into conference {}: status={}, body={}",
                        call_id, conference_id_or_name, status, body
                    );
                    Err(TransportError::SendFailed(format!(
                        "Telnyx conference join failed: status={}",
                        status
                    )))
                }
            }
        } else {
            // Step 2b: Conference doesn't exist. Create it with our call_control_id to automatically join on creation.
            // A Telnyx conference MUST be created with a valid call_control_id.
            let conf_payload = serde_json::json!({
                "name": conference_id_or_name,
                "call_control_id": call_id
            });
            let conf_resp = self.client
                .post("https://api.telnyx.com/v2/conferences")
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .body(conf_payload.to_string())
                .send()
                .await
                .map_err(|e| {
                    TransportError::SendFailed(format!("Telnyx create conference failed: {}", e))
                })?;

            let status = conf_resp.status().as_u16();
            let conf_body = conf_resp.text().await.unwrap_or_default();

            if matches!(status, 200 | 201) {
                let json: serde_json::Value = serde_json::from_str(&conf_body).unwrap_or_default();
                let conf_id = json.pointer("/data/id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        warn!("[telnyx] conference creation response missing /data/id; raw body: {}", conf_body);
                        String::new()
                    });
                info!(
                    "[telnyx] Created conference {} (id={}) and joined initial call {}",
                    conference_id_or_name, conf_id, call_id
                );
                Ok(conf_id)
            } else {
                error!(
                    "[telnyx] Failed to create conference {} with call {}: status={}, body={}",
                    conference_id_or_name, call_id, status, conf_body
                );
                Err(TransportError::SendFailed(format!(
                    "Telnyx conference creation failed: status={}",
                    status
                )))
            }
        }
    }
}

