//! WebRTC transport — produces a [`TransportHandle`] from a WebRTC connection.
//!
//! Bridges the WebRTC connection's channels into the standard transport
//! abstraction, and subscribes to the EventBus to forward audio + interrupt
//! events to the str0m event loop for RTP transmission.
//!
//! In hybrid mode (WebRTC audio + WS UI), all non-audio events (transcripts,
//! state, tools) are forwarded by the WebSocket handler — NOT the data channel.

use std::collections::HashSet;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;
use voice_trace::{EventCategory, Tracer};

use crate::error::TransportError;
use crate::{TransportAudioSink, TransportHandle};

use super::connection::{RtcInternalCmd, WebRtcConnection};

use super::OPUS_SAMPLE_RATE;

// ── WebRTC Transport ────────────────────────────────────────────

/// Creates a [`TransportHandle`] from a WebRTC connection.
///
/// - Audio input: Opus-decoded PCM from the connection's event loop
/// - Audio output: PCM → Opus encoding → RTP via str0m writer
/// - Events: JSON over the WebRTC data channel
pub struct WebRtcTransport;

impl WebRtcTransport {
    /// Create a [`TransportHandle`] from an established [`WebRtcConnection`].
    ///
    /// The connection's audio/control channels are wired directly into the
    /// transport handle. An EventBus subscriber is spawned to forward events
    /// to the client via the data channel.
    pub fn from_connection(mut connection: WebRtcConnection, tracer: &Tracer) -> TransportHandle {
        let audio_rx = connection
            .audio_rx
            .take()
            .expect("audio_rx already consumed");
        let control_rx = connection
            .control_rx
            .take()
            .expect("control_rx already consumed");
        let control_tx = connection
            .control_tx
            .take()
            .expect("control_tx already consumed");
        let event_tx = connection
            .event_tx
            .take()
            .expect("event_tx already consumed");

        let audio_out_tx = connection
            .audio_out_tx
            .take()
            .expect("audio_out_tx already consumed");

        // Audio sink sends PCM to the str0m event loop for Opus encoding + RTP
        let audio_sink = WebRtcAudioSink {
            audio_out_tx: audio_out_tx.clone(),
        };

        // Spawn forward loop: EventBus → str0m event loop (audio + interrupt only).
        // In hybrid mode, UI events (transcripts, state, tools) go through the WS.
        let rtc_events = tracer.subscribe_filtered(rtc_categories());
        let forward_handle = tokio::spawn(rtc_forward_loop(rtc_events, audio_out_tx));

        // Take ownership of the RTC event loop task so it isn't aborted
        // when the connection struct is dropped.
        let rtc_handle = connection.task_handle.take();

        // Collect background tasks — they'll be aborted when TransportHandle drops
        let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        if let Some(h) = rtc_handle {
            tasks.push(h);
        }
        tasks.push(forward_handle);

        TransportHandle {
            audio_rx,
            audio_tx: Box::new(audio_sink),
            control_rx,
            event_tx,
            control_tx,
            input_sample_rate: OPUS_SAMPLE_RATE,
            _background_tasks: tasks,
            is_telephony: false,
        }
    }
}

// ── Categories the RTC forward loop needs ──────────────────────
//
// Only Audio (for RTP) and Session (for Interrupt → ClearAudio).
// All other event categories (Transcript, Tool, Agent, Error) are
// forwarded by the WebSocket handler in hybrid mode.

fn rtc_categories() -> HashSet<EventCategory> {
    HashSet::from([EventCategory::AgentAudio, EventCategory::Session])
}

// ── Forward Loop ────────────────────────────────────────────────

/// Forwards audio and interrupt events from the EventBus to the str0m event loop.
///
/// - Audio events  → `RtcInternalCmd::SendAudio`  → Opus encode → RTP
/// - Interrupt      → `RtcInternalCmd::ClearAudio` → flush pacing buffer
///
/// UI events (transcripts, state, tools) are NOT handled here —
/// they flow through the WebSocket in hybrid mode.
async fn rtc_forward_loop(
    mut events: voice_trace::FilteredReceiver,
    audio_out_tx: mpsc::UnboundedSender<RtcInternalCmd>,
) {
    use voice_trace::Event;
    while let Some(event) = events.recv().await {
        match &event {
            Event::AgentAudio { pcm: data, .. } => {
                let _ = audio_out_tx.send(RtcInternalCmd::SendAudio(data.clone()));
            }
            Event::Interrupt => {
                let _ = audio_out_tx.send(RtcInternalCmd::ClearAudio);
            }
            _ => {} // Other events forwarded by WS handler
        }
    }
}

// ── WebRTC Audio Sink ───────────────────────────────────────────

/// Audio sink for WebRTC — sends PCM16 to the str0m event loop
/// where it is Opus-encoded and transmitted via RTP.
struct WebRtcAudioSink {
    /// Channel to the str0m event loop for outgoing audio.
    audio_out_tx: mpsc::UnboundedSender<RtcInternalCmd>,
}

#[async_trait]
impl TransportAudioSink for WebRtcAudioSink {
    async fn send_audio(&self, pcm: Bytes) -> Result<(), TransportError> {
        self.audio_out_tx
            .send(RtcInternalCmd::SendAudio(pcm))
            .map_err(|_| TransportError::Closed)?;
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), TransportError> {
        // For WebRTC, interruption is handled by the Reactor
        // stopping the TTS audio feed. No special action needed.
        Ok(())
    }
}
