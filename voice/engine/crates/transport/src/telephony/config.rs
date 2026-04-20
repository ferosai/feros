//! Telephony configuration types — provider credentials and session parameters.
//!
//! Users provide their own Twilio/Telnyx API keys and phone numbers.
//! Credentials can be passed as query parameters on the webhook URL or
//! stored in the pre-registered session config.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// PSTN sample rate is always 8 kHz by default.
pub const TELEPHONY_SAMPLE_RATE: u32 = 8000;

/// Credentials and provider selection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum TelephonyCredentials {
    Twilio {
        #[serde(default)]
        account_sid: String,
        #[serde(default)]
        auth_token: String,
    },
    #[cfg(feature = "telnyx")]
    Telnyx {
        #[serde(default)]
        api_key: String,
        #[serde(default)]
        connection_id: String,
    },
}

impl Default for TelephonyCredentials {
    fn default() -> Self {
        Self::Twilio {
            account_sid: String::new(),
            auth_token: String::new(),
        }
    }
}

impl TelephonyCredentials {
    /// Return the discriminant as a [`TelephonyProviderKind`].
    pub fn kind(&self) -> TelephonyProviderKind {
        match self {
            Self::Twilio { .. } => TelephonyProviderKind::Twilio,
            #[cfg(feature = "telnyx")]
            Self::Telnyx { .. } => TelephonyProviderKind::Telnyx,
        }
    }
}

/// Lightweight provider discriminant — identifies *which* telephony provider
/// an incoming WebSocket came from without carrying credential fields.
///
/// Use this when you only need to branch on Twilio vs Telnyx, and credentials
/// will be resolved separately (e.g. from query params, registered session,
/// or [`ServerState`]).  The full [`TelephonyCredentials`] enum is used once
/// credentials are actually known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelephonyProviderKind {
    Twilio,
    #[cfg(feature = "telnyx")]
    Telnyx,
}

/// Audio encoding used by the telephony provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TelephonyEncoding {
    /// μ-law (G.711μ) — Twilio default, Telnyx option.
    #[serde(rename = "PCMU")]
    Pcmu,
    /// A-law (G.711A) — Telnyx option.
    #[serde(rename = "PCMA")]
    Pcma,
}

impl Default for TelephonyEncoding {
    fn default() -> Self {
        Self::Pcmu
    }
}

/// Credentials and identifiers for a telephony session.
///
/// Users provide these when configuring their telephony webhook —
/// we do not store credentials beyond the session lifetime.
///
/// `stream_id` and `call_id` use interior mutability (`Mutex`) because
/// they are unknown at construction time and populated from the provider's
/// `start` WebSocket event while the transport is already running.
#[derive(Debug, Serialize, Deserialize)]
pub struct TelephonyConfig {
    /// Which telephony provider is sending the WebSocket connection and its credentials.
    #[serde(flatten)]
    pub credentials: TelephonyCredentials,

    /// Provider-assigned stream identifier.
    ///
    /// - Twilio: `streamSid` (received in the `start` event)
    /// - Telnyx: `stream_id` (received in the `start` event)
    ///
    /// Populated by the recv loop when the `start` event arrives.
    /// Wrapped in `Mutex` for interior mutability (set-once in practice, read-many).
    #[serde(default)]
    pub stream_id: Mutex<String>,

    /// Provider-assigned call identifier (for call control: hangup, transfer).
    ///
    /// - Twilio: `callSid`
    /// - Telnyx: `call_control_id`
    ///
    /// Populated by the recv loop when the `start` event arrives.
    #[serde(default)]
    pub call_id: Mutex<Option<String>>,

    /// Audio encoding for inbound audio (provider → us).
    #[serde(default)]
    pub inbound_encoding: TelephonyEncoding,

    /// Audio encoding for outbound audio (us → provider).
    #[serde(default)]
    pub outbound_encoding: TelephonyEncoding,

    /// Whether to automatically hang up the call when the session ends.
    #[serde(default = "default_auto_hang_up")]
    pub auto_hang_up: bool,

    /// Telephony sample rate. Always 8000 Hz for PSTN.
    #[serde(default = "default_telephony_sample_rate")]
    pub sample_rate: u32,

    /// Publicly reachable URL for incoming webhooks (e.g. transfer status callbacks).
    #[serde(default)]
    pub public_url: Option<String>,

    /// The provider phone number that placed / answered this call.
    ///
    /// Required for supervised transfer: when the AI initiates a transfer, we place
    /// a *new* outbound call from this number to the destination. The destination
    /// is then put into a Twilio/Telnyx Conference room; once they answer, we move
    /// the original caller into the same room and terminate the AI leg.
    #[serde(default)]
    pub from_number: Option<String>,
}

fn default_auto_hang_up() -> bool {
    true
}

fn default_telephony_sample_rate() -> u32 {
    TELEPHONY_SAMPLE_RATE
}

impl Default for TelephonyConfig {
    fn default() -> Self {
        Self {
            credentials: TelephonyCredentials::default(),
            stream_id: Mutex::new(String::new()),
            call_id: Mutex::new(None),
            inbound_encoding: TelephonyEncoding::default(),
            outbound_encoding: TelephonyEncoding::default(),
            auto_hang_up: true,
            sample_rate: TELEPHONY_SAMPLE_RATE,
            public_url: None,
            from_number: None,
        }
    }
}

impl TelephonyConfig {
    /// Get the current stream ID (locks briefly).
    pub fn get_stream_id(&self) -> String {
        self.stream_id.lock().unwrap().clone()
    }

    /// Set the stream ID (from the provider's `start` event).
    pub fn set_stream_id(&self, id: String) {
        *self.stream_id.lock().unwrap() = id;
    }

    /// Get the current call ID (locks briefly).
    pub fn get_call_id(&self) -> Option<String> {
        self.call_id.lock().unwrap().clone()
    }

    /// Set the call ID (from the provider's `start` event).
    pub fn set_call_id(&self, id: String) {
        *self.call_id.lock().unwrap() = Some(id);
    }
}
