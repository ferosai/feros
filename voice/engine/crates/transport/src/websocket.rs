//! WebSocket transport — wraps an Axum WebSocket into a [`TransportHandle`].
//!
//! The transport splits the WebSocket into
//! send/receive halves, spawns background tasks for each direction, and
//! presents a uniform [`TransportHandle`] to the voice session.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
use voice_trace::{Event, EventCategory, Tracer};

use crate::error::TransportError;
use crate::{TransportAudioSink, TransportEvent, TransportHandle};

// ── WebSocket Transport Factory ─────────────────────────────────

/// Creates a [`TransportHandle`] from an established Axum WebSocket.
pub struct WebSocketTransport;

impl WebSocketTransport {
    /// Accept an Axum WebSocket and produce a [`TransportHandle`].
    ///
    /// This spawns two background tasks:
    /// 1. **Receive loop**: reads WS messages → audio channel + control events
    /// 2. **Forward loop**: EventBus events → WS messages (via the Tracer)
    ///
    /// The returned `TransportHandle` is ready to be passed to `VoiceSession::start()`.
    pub fn accept(socket: WebSocket, tracer: &Tracer, input_sample_rate: u32) -> TransportHandle {
        let (ws_sink, ws_stream) = socket.split();
        let (audio_in_tx, audio_rx) = mpsc::unbounded_channel::<Bytes>();
        let (control_event_tx, control_rx) = mpsc::unbounded_channel();
        let (control_cmd_tx, _control_cmd_rx) = mpsc::unbounded_channel();

        let ws_sink = Arc::new(Mutex::new(ws_sink));

        // ── Task 1: WS incoming → audio + control events ──────────
        let event_tx = control_event_tx.clone();
        let recv_handle = tokio::spawn(ws_recv_loop(ws_stream, audio_in_tx, event_tx));

        // ── Task 2: EventBus → WS outgoing ────────────────────────
        let ws_events = tracer.subscribe_filtered(ws_categories());
        let sink_for_forward = ws_sink.clone();
        let forward_handle = tokio::spawn(ws_forward_loop(ws_events, sink_for_forward));

        // ── Audio sink (sends binary WS frames) ──────────────────
        let audio_sink = WebSocketAudioSink {
            ws_sink: ws_sink.clone(),
        };

        TransportHandle {
            audio_rx,
            audio_tx: Box::new(audio_sink),
            control_rx,
            event_tx: control_event_tx.clone(),
            control_tx: control_cmd_tx,
            input_sample_rate,
            _background_tasks: vec![recv_handle, forward_handle],
            is_telephony: false,
        }
    }
}

// ── Categories the WebSocket client cares about ──────────────────

fn ws_categories() -> HashSet<EventCategory> {
    HashSet::from([
        EventCategory::Session,
        EventCategory::Transcript,
        EventCategory::Tool,
        EventCategory::Agent,
        EventCategory::AgentAudio,
        EventCategory::Error,
    ])
}

// ── WS Receive Loop ─────────────────────────────────────────────

/// Reads from the WebSocket and dispatches to the appropriate channel:
/// - Binary messages → `audio_tx` (raw PCM audio)
/// - Text messages   → parsed for control commands, forwarded as events
/// - Close messages  → `control_tx` with Disconnected event
async fn ws_recv_loop(
    mut ws_stream: futures_util::stream::SplitStream<WebSocket>,
    audio_tx: mpsc::UnboundedSender<Bytes>,
    control_tx: mpsc::UnboundedSender<TransportEvent>,
) {
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Binary(data) => {
                let _ = audio_tx.send(Bytes::from(data.to_vec()));
            }
            Message::Text(text) => {
                // Parse JSON and handle known control messages inline,
                // forward everything else as a generic control event.
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match msg_type {
                        "session.end" => {
                            info!("[ws-transport] Client requested session end");
                            let _ = control_tx.send(TransportEvent::Disconnected {
                                reason: "session.end".to_string(),
                            });
                            break; // Drop audio_tx → Reactor's audio_rx returns None → stops
                        }
                        "config" => {
                            if let Some(sr) = json.get("input_sample_rate").and_then(|v| v.as_u64())
                            {
                                info!("[ws-transport] Client config: input_sample_rate={}", sr);
                            }
                            let _ = control_tx.send(TransportEvent::ControlMessage(json));
                        }
                        _ => {
                            let _ = control_tx.send(TransportEvent::ControlMessage(json));
                        }
                    }
                }
            }
            Message::Close(_) => {
                info!("[ws-transport] WebSocket closed by client");
                let _ = control_tx.send(TransportEvent::Disconnected {
                    reason: "client closed".to_string(),
                });
                break;
            }
            _ => {}
        }
    }
}

// ── WS Forward Loop ─────────────────────────────────────────────

/// Forwards events from the EventBus to the WebSocket.
///
/// Audio events → binary WS messages; all other events → JSON text messages.
async fn ws_forward_loop(
    mut ws_events: voice_trace::FilteredReceiver,
    ws_sink: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
) {
    while let Some(event) = ws_events.recv().await {
        let ws_msg = match &event {
            Event::AgentAudio { pcm: data, .. } => Message::Binary(data.to_vec().into()),
            other => match serde_json::to_string(other) {
                Ok(json) => Message::Text(json.into()),
                Err(e) => {
                    warn!("[ws-transport] Failed to serialize event: {}", e);
                    continue;
                }
            },
        };
        let mut sink = ws_sink.lock().await;
        if sink.send(ws_msg).await.is_err() {
            break;
        }
    }
}

// ── WebSocket Audio Sink ────────────────────────────────────────

/// Sends PCM16 audio as binary WebSocket messages.
struct WebSocketAudioSink {
    ws_sink: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
}

#[async_trait]
impl TransportAudioSink for WebSocketAudioSink {
    async fn send_audio(&self, pcm: Bytes) -> Result<(), TransportError> {
        let mut sink = self.ws_sink.lock().await;
        sink.send(Message::Binary(pcm.to_vec().into()))
            .await
            .map_err(|e| TransportError::SendFailed(e.to_string()))
    }

    async fn interrupt(&self) -> Result<(), TransportError> {
        // For WebSocket, the interrupt event is already sent via EventBus
        // (the forward loop will serialize the Event::Interrupt as JSON).
        // No additional action needed here.
        Ok(())
    }
}
