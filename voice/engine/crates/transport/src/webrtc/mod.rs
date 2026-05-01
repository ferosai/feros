//! WebRTC transport — lightweight voice-oriented WebRTC using `str0m` (Sans I/O).
//!
//! This module provides a lightweight WebRTC transport for voice sessions.
//! It uses `str0m` as the Sans I/O WebRTC engine and `opus` for audio
//! codec support.
//!
//! # Architecture
//!
//! ```text
//!    Browser                          Server
//!  ┌──────────┐  POST /rtc/offer   ┌──────────────────┐
//!  │  Client  │ ─────SDP Offer───→ │  HTTP Signaling  │
//!  │   (JS)   │ ←────SDP Answer─── │   (axum route)   │
//!  │          │                    │                  │
//!  │  Audio   │ ═══UDP/SRTP═══════ │  str0m Rtc Loop  │
//!  │  Track   │   (Opus encoded)   │  (spawned task)  │
//!  │          │                    │                  │
//!  │  Data    │ ═══SCTP/DTLS══════ │ Control Channel  │
//!  │ Channel  │   (JSON msgs)      │                  │
//!  └──────────┘                    └──────────────────┘
//! ```
//!
//! # Signaling
//!
//! Single HTTP POST exchange (no WebSocket handshake needed):
//! 1. Client sends SDP Offer → `POST /rtc/offer`
//! 2. Server creates `Rtc`, creates Answer, returns it
//! 3. ICE candidates are gathered implicitly (str0m adds host candidates)
//! 4. Media flows over UDP once ICE completes
//!
//! # Audio Flow
//!
//! - **Incoming**: str0m delivers Opus frames via `Event::MediaData` →
//!   opus decode → PCM16 → `audio_tx` channel → Reactor
//! - **Outgoing**: Reactor emits `Event::Audio(pcm)` via EventBus →
//!   opus encode → `rtc.writer(mid).write()` → str0m → UDP → Browser

pub mod multiplexer;
mod connection;
pub mod ice;
pub mod stun;
mod transport;

pub use connection::WebRtcConnection;
pub use ice::{ice_provider_from_config, IceConfig, IceProvider, IceProviderError, IceServer};
pub use multiplexer::UdpMux;
pub use transport::WebRtcTransport;

/// Opus sample rate is always 48kHz per spec.
pub const OPUS_SAMPLE_RATE: u32 = 48000;
