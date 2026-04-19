//! Shared utilities for voice-server.

/// Convert an HTTP(S) URL to a WebSocket URL.
///
/// `PUBLIC_URL` is naturally expressed as `https://` (how users think about
/// their domain, what ngrok/reverse-proxies give you). But Twilio `<Stream>`,
/// Telnyx `<Stream>`, and browser WebSocket connections all need `wss://`.
///
/// Rules:
///   https://… → wss://…
///   http://…  → ws://…
///   wss://…   → wss://…  (already correct — pass through)
///   ws://…    → ws://…   (already correct — pass through)
pub fn to_ws_url(url: &str) -> String {
    let cleaned = url.trim_end_matches('/');
    if let Some(rest) = cleaned.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = cleaned.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        cleaned.to_string() // already ws:// or wss://
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_ws_url() {
        assert_eq!(
            to_ws_url("https://voice.myapp.com"),
            "wss://voice.myapp.com"
        );
        assert_eq!(to_ws_url("http://localhost:8300"), "ws://localhost:8300");
        assert_eq!(to_ws_url("wss://voice.myapp.com"), "wss://voice.myapp.com");
        assert_eq!(to_ws_url("ws://localhost:8300"), "ws://localhost:8300");
        assert_eq!(
            to_ws_url("https://abc123.ngrok.io"),
            "wss://abc123.ngrok.io"
        );
    }
}
