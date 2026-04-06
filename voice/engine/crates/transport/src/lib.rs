//! Transport abstraction layer for voice-rust.
//!
//! Defines the [`TransportHandle`] that decouples the voice pipeline from any
//! specific I/O transport. Currently provides:
//!
//! - [`WebSocketTransport`] — the existing WebSocket-based transport (always available).
//! - [`WebRtcTransport`] — lightweight WebRTC transport (behind `webrtc` feature flag).
//!
//! Future transports (telephony) will live behind additional feature flags.

mod error;
pub mod websocket;

#[cfg(feature = "webrtc")]
pub mod webrtc;

#[cfg(feature = "telephony")]
pub mod telephony;

pub use error::TransportError;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

// ── Core Types ───────────────────────────────────────────────────

/// A handle to an active transport connection.
///
/// Created by a transport factory (e.g. [`WebSocketTransport::accept`]).
/// The [`VoiceSession`] consumes this handle:
/// - `audio_rx` feeds the Reactor
/// - `audio_tx` bridges EventBus audio events to the remote client
/// - `control_rx` receives lifecycle events from the transport
/// - `control_tx` sends control messages back to the client
pub struct TransportHandle {
    /// Incoming audio from the remote end (user's microphone).
    /// PCM16 mono at the transport's input sample rate.
    pub audio_rx: mpsc::UnboundedReceiver<Bytes>,

    /// Outgoing audio sink — send TTS audio to the remote end.
    pub audio_tx: Box<dyn TransportAudioSink>,

    /// Transport lifecycle events (connected, disconnected, control messages).
    pub control_rx: mpsc::UnboundedReceiver<TransportEvent>,

    /// Send control messages back to the client.
    pub control_tx: mpsc::UnboundedSender<TransportCommand>,

    /// The actual sample rate of incoming audio.
    pub input_sample_rate: u32,

    /// Background tasks owned by this transport (event loop, forward loop, etc.).
    /// Aborted on drop so they don't leak.
    pub(crate) _background_tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl TransportHandle {
    /// Take the audio receiver out of the handle.
    ///
    /// Replaces the internal receiver with a dummy closed channel.
    /// Used by `VoiceSession` to hand the receiver to the Reactor.
    pub fn take_audio_rx(&mut self) -> mpsc::UnboundedReceiver<Bytes> {
        let (_, dummy_rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.audio_rx, dummy_rx)
    }

    /// Create a no-op transport handle that owns no resources.
    ///
    /// Used when a session path takes ownership of the real transport
    /// but the `VoiceSession` struct still needs a placeholder for `_transport`.
    pub fn dummy() -> Self {
        struct NullSink;
        #[async_trait::async_trait]
        impl TransportAudioSink for NullSink {
            async fn send_audio(&self, _: Bytes) -> Result<(), TransportError> {
                Ok(())
            }
            async fn interrupt(&self) -> Result<(), TransportError> {
                Ok(())
            }
        }
        let (_, audio_rx) = mpsc::unbounded_channel();
        let (control_tx, _) = mpsc::unbounded_channel();
        let (_, control_rx) = mpsc::unbounded_channel();
        Self {
            audio_rx,
            audio_tx: Box::new(NullSink),
            control_rx,
            control_tx,
            input_sample_rate: 48_000,
            _background_tasks: vec![],
        }
    }
}

impl Drop for TransportHandle {
    fn drop(&mut self) {
        for handle in self._background_tasks.drain(..) {
            handle.abort();
        }
    }
}

/// Events from the transport to the session/reactor.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// Client connected and ready.
    Connected,
    /// Client disconnected (clean close or error).
    Disconnected { reason: String },
    /// Client sent a JSON control message (e.g. session config).
    ControlMessage(serde_json::Value),
}

/// Commands from the session to the transport.
#[derive(Debug, Clone)]
pub enum TransportCommand {
    /// Send a JSON message to the client.
    SendMessage(serde_json::Value),
    /// Signal the transport to close.
    Close,
    /// Add a remote ICE candidate.
    #[cfg(feature = "webrtc")]
    AddIceCandidate(String),
}

/// Trait for sending audio to the remote end.
///
/// Different transports implement this differently:
/// - WebSocket: serialize as binary WS message
/// - WebRTC:    encode to Opus, write as RTP sample
#[async_trait]
pub trait TransportAudioSink: Send + Sync {
    /// Send a chunk of PCM16 audio to the remote end.
    async fn send_audio(&self, pcm: Bytes) -> Result<(), TransportError>;

    /// Signal interruption (e.g. barge-in — flush/stop audio buffers).
    async fn interrupt(&self) -> Result<(), TransportError>;
}
