//! Telephony transport — produces a [`TransportHandle`] from a Twilio/Telnyx
//! WebSocket Media Streams connection.
//!
//! Provider-specific logic (JSON framing, call control) is delegated to
//! the [`TelephonyProviderImpl`] trait — see `twilio.rs` and `telnyx.rs`.
//!
//! # Architecture
//!
//! Phone ──PSTN──▶ Provider ──WS JSON──▶ TelephonyTransport
//!                                           │
//!                               ┌───────────┴───────────┐
//!                         Recv Loop              Forward Loop
//!                     (JSON → decode →       (EventBus audio →
//!                      PCM16 → audio_rx)      encode → JSON WS)
//!
//! Synchronization: Pacing vs Mark Events
//! Unlike some other telephony frameworks, this transport deliberately does NOT
//! use or implement `mark` events to track audio playback. Instead, the `Forward Loop` strictly
//! paces outbound audio, dispatching exactly 20ms chunks every 20ms of real time using a
//! `tokio` interval.
//!
//! This intentional design keeps the provider's (Twilio/Telnyx) remote playback buffer virtually
//! empty. As a result, when an interrupt (barge-in) occurs, we instantly drop our local buffer
//! and issue a single `clear` frame. This yields ultra-low latency barge-in and completely
//! bypasses the need for complex `mark` event queue synchronization.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tracing::{info, warn};
use voice_trace::{Event, EventCategory};

use crate::error::TransportError;
use crate::{TransportAudioSink, TransportCommand, TransportEvent, TransportHandle};

use super::codec;
use super::config::{TelephonyConfig, TelephonyEncoding};
use super::providers::{self, TelephonyProviderImpl};

// ── Telephony Transport ─────────────────────────────────────────

/// Creates a [`TransportHandle`] from a telephony provider's WebSocket.
///
/// Works with any provider that implements [`TelephonyProviderImpl`].
/// The JSON message format differs slightly per provider but follows
/// the same pattern:
/// - `"start"` / `"connected"` → session metadata
/// - `"media"` → base64-encoded G.711 audio
/// - `"stop"` → call ended
pub struct TelephonyTransport;

impl TelephonyTransport {
    /// Accept a telephony WebSocket and produce a [`TransportHandle`].
    ///
    /// Spawns three background tasks:
    /// 1. **Receive loop**: reads provider JSON → decode G.711 → `audio_rx`
    /// 2. **Forward loop**: EventBus audio → encode G.711 → JSON WS
    /// 3. **Command loop**: handles `Close` → auto-hangup via REST API
    /// Accept a telephony WebSocket and produce a [`TransportHandle`].
    ///
    /// `bus_sender_rx` receives the correct `broadcast::Sender<Event>` once
    /// the session has been resolved (which tracer to use is not known at
    /// construction time when the session_id comes from a `customParameters`).
    /// The forward loop waits for this sender (with a short timeout) before
    /// subscribing to ensure it uses the same event bus as the reactor.
    pub fn accept(
        socket: WebSocket,
        config: TelephonyConfig,
        bus_sender_rx: oneshot::Receiver<broadcast::Sender<Event>>,
    ) -> (TransportHandle, oneshot::Receiver<(Option<String>, Option<String>)>) {
        let (ws_sink, ws_stream) = socket.split();
        let (audio_in_tx, audio_rx) = mpsc::unbounded_channel::<Bytes>();
        let (control_event_tx, control_rx) = mpsc::unbounded_channel();
        let (control_cmd_tx, control_cmd_rx) = mpsc::unbounded_channel();

        let ws_sink = Arc::new(Mutex::new(ws_sink));
        let config = Arc::new(config);
        let provider: Arc<Box<dyn TelephonyProviderImpl>> =
            Arc::new(providers::create_provider(&config.credentials));

        // ── Task 1: Provider WS → decode → audio + control events ──
        let recv_config = Arc::clone(&config);
        let recv_provider = Arc::clone(&provider);
        let event_tx = control_event_tx.clone();
        let (session_id_tx, session_id_rx) = oneshot::channel();
        let recv_handle = tokio::spawn(telephony_recv_loop(
            ws_stream,
            audio_in_tx,
            event_tx,
            recv_config,
            recv_provider,
            session_id_tx,
        ));

        // ── Task 2: EventBus audio → encode → Provider WS ──────────
        // The forward loop receives the broadcast::Sender via a oneshot so it
        // can subscribe to the correct event bus after session resolution.
        let fwd_config = Arc::clone(&config);
        let fwd_provider = Arc::clone(&provider);
        let fwd_sink = Arc::clone(&ws_sink);
        let forward_handle = tokio::spawn(telephony_forward_loop(
            bus_sender_rx,
            fwd_sink,
            fwd_config,
            fwd_provider,
        ));

        // ── Task 3: Handle control commands (hangup on close, supervised transfer) ──
        let cmd_config = Arc::clone(&config);
        let cmd_provider = Arc::clone(&provider);
        let cmd_event_tx = control_event_tx.clone();
        let cmd_handle = tokio::spawn(telephony_cmd_loop(control_cmd_rx, cmd_config, cmd_provider, cmd_event_tx));

        // ── Audio sink (sends encoded G.711 as JSON WS frames) ─────
        let audio_sink = TelephonyAudioSink {
            ws_sink: Arc::clone(&ws_sink),
            config: Arc::clone(&config),
            provider: Arc::clone(&provider),
        };

        let handle = TransportHandle {
            audio_rx,
            audio_tx: Box::new(audio_sink),
            control_rx,
            event_tx: control_event_tx.clone(),
            control_tx: control_cmd_tx,
            input_sample_rate: config.sample_rate,
            _background_tasks: vec![recv_handle, forward_handle, cmd_handle],
            is_telephony: true,
        };

        (handle, session_id_rx)
    }
}

// ── Event categories the telephony transport cares about ────────

fn telephony_categories() -> HashSet<EventCategory> {
    HashSet::from([EventCategory::AgentAudio, EventCategory::Session])
}

// ── Receive Loop ────────────────────────────────────────────────

async fn telephony_recv_loop(
    mut ws_stream: futures_util::stream::SplitStream<WebSocket>,
    audio_tx: mpsc::UnboundedSender<Bytes>,
    control_tx: mpsc::UnboundedSender<TransportEvent>,
    config: Arc<TelephonyConfig>,
    provider: Arc<Box<dyn TelephonyProviderImpl>>,
    session_id_tx: oneshot::Sender<(Option<String>, Option<String>)>,
) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut session_id_tx = Some(session_id_tx);

    while let Some(Ok(msg)) = ws_stream.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!(
                    "[telephony:{}] WebSocket closed by provider",
                    provider.name()
                );
                let _ = control_tx.send(TransportEvent::Disconnected {
                    reason: "provider closed".to_string(),
                });
                break;
            }
            _ => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!("[telephony:{}] Invalid JSON: {}", provider.name(), e);
                continue;
            }
        };

        let event = json.get("event").and_then(|v| v.as_str()).unwrap_or("");

        match event {
            "connected" => {
                info!("[telephony:{}] Provider connected", provider.name());
                let _ = control_tx.send(TransportEvent::Connected);
            }

            "start" => {
                let stream_id = provider
                    .extract_stream_id(&json)
                    .unwrap_or_else(|| "unknown".to_string());
                let call_id = provider.extract_call_id(&json);
                let custom_session_id = provider.extract_custom_param(&json, "session_id");

                info!(
                    "[telephony:{}] Stream started, stream_id={}, call_id={:?}, custom_session_id={:?}",
                    provider.name(),
                    stream_id,
                    call_id,
                    custom_session_id
                );

                // Send session_id and call_id from start event to the server (once)
                if let Some(tx) = session_id_tx.take() {
                    let _ = tx.send((custom_session_id, call_id.clone()));
                }

                // Store the provider-assigned IDs so outbound frames can use them
                config.set_stream_id(stream_id);
                if let Some(cid) = call_id {
                    config.set_call_id(cid);
                }

                let _ = control_tx.send(TransportEvent::Connected);
            }

            "media" => {
                let payload_b64 = match json
                    .get("media")
                    .and_then(|m| m.get("payload"))
                    .and_then(|v| v.as_str())
                {
                    Some(p) => p,
                    None => continue,
                };

                let raw = match b64.decode(payload_b64) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                let pcm = match config.inbound_encoding {
                    TelephonyEncoding::Pcmu => codec::ulaw_to_pcm16(&raw),
                    TelephonyEncoding::Pcma => codec::alaw_to_pcm16(&raw),
                };

                let _ = audio_tx.send(Bytes::from(pcm));
            }

            "dtmf" => {
                let _ = control_tx.send(TransportEvent::ControlMessage(json));
            }

            "stop" => {
                info!("[telephony:{}] Stream stopped", provider.name());
                let _ = control_tx.send(TransportEvent::Disconnected {
                    reason: "stream stopped".to_string(),
                });
                break;
            }

            other => {
                if !other.is_empty() {
                    warn!("[telephony:{}] Unknown event: {}", provider.name(), other);
                }
            }
        }
    }

    info!("[telephony:{}] Receive loop ended", provider.name());
}

// ── Forward Loop ────────────────────────────────────────────────

async fn telephony_forward_loop(
    bus_sender_rx: oneshot::Receiver<broadcast::Sender<Event>>,
    ws_sink: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    config: Arc<TelephonyConfig>,
    provider: Arc<Box<dyn TelephonyProviderImpl>>,
) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    const PACING_INTERVAL_MS: u64 = 20;

    // (Samples / Second) * (Seconds / Frame) = Samples / Frame
    let frame_samples = (config.sample_rate as u64 * PACING_INTERVAL_MS / 1000) as usize;

    let mut audio_pace_buf = std::collections::VecDeque::<i16>::new();
    let mut pace_interval =
        tokio::time::interval(std::time::Duration::from_millis(PACING_INTERVAL_MS));
    pace_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Wait for the correct event bus sender from session resolution.
    // Session lookup completes within milliseconds; 5s is a very generous bound.
    let bus_sender =
        match tokio::time::timeout(std::time::Duration::from_secs(5), bus_sender_rx).await {
            Ok(Ok(sender)) => sender,
            Ok(Err(_)) => {
                warn!(
                    "[telephony:{}] Bus sender oneshot dropped — forward loop aborting",
                    provider.name()
                );
                return;
            }
            Err(_) => {
                warn!(
                    "[telephony:{}] Timed out waiting for event bus sender — forward loop aborting",
                    provider.name()
                );
                return;
            }
        };

    let mut events =
        voice_trace::FilteredReceiver::new(bus_sender.subscribe(), telephony_categories());

    loop {
        tokio::select! {
            biased;

            event_opt = events.recv() => {
                let Some(event) = event_opt else {
                    info!("[telephony:{}] Event bus closed — forward loop ending",
                        provider.name());
                    break;
                };
                match &event {
                    Event::AgentAudio { pcm: pcm_data, sample_rate, .. } => {
                        let expected_sample_rate = config.sample_rate;
                        debug_assert_eq!(
                            *sample_rate, expected_sample_rate,
                            "telephony forward loop received PCM at {}Hz — expected {}Hz; \
                             check SessionConfig::output_sample_rate for telephony sessions",
                            sample_rate, expected_sample_rate,
                        );
                        let mut pcm_slice = &pcm_data[..];
                        while pcm_slice.len() >= 2 {
                            let sample = i16::from_le_bytes([pcm_slice[0], pcm_slice[1]]);
                            audio_pace_buf.push_back(sample);
                            pcm_slice = &pcm_slice[2..];
                        }
                    }

                    Event::Interrupt => {
                        audio_pace_buf.clear();
                        let stream_id = config.get_stream_id();
                        let json = provider.clear_frame(&stream_id);
                        if let Ok(s) = serde_json::to_string(&json) {
                            let mut sink = ws_sink.lock().await;
                            if let Err(e) = sink.send(Message::Text(s.into())).await {
                                warn!("[telephony:{}] WS send error (clear): {}", provider.name(), e);
                                break;
                            }
                        }
                    }

                    _ => continue,
                }
            }

            _ = pace_interval.tick() => {
                if audio_pace_buf.len() >= frame_samples {
                    let frame: Vec<i16> = audio_pace_buf.drain(..frame_samples).collect();
                    let pcm_bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();

                    let encoded = match config.outbound_encoding {
                        TelephonyEncoding::Pcmu => codec::pcm16_to_ulaw(&pcm_bytes),
                        TelephonyEncoding::Pcma => codec::pcm16_to_alaw(&pcm_bytes),
                    };
                    let payload = b64.encode(&encoded);
                    let stream_id = config.get_stream_id();
                    let json = provider.media_frame(&payload, &stream_id);

                    if let Ok(s) = serde_json::to_string(&json) {
                        let mut sink = ws_sink.lock().await;
                        if let Err(e) = sink.send(Message::Text(s.into())).await {
                            warn!("[telephony:{}] WS send error: {}", provider.name(), e);
                            break;
                        }
                    }
                }
            }
        }
    }

    info!("[telephony:{}] Forward loop ended", provider.name());
}

// ── Command Loop ────────────────────────────────────────────────

async fn telephony_cmd_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<TransportCommand>,
    config: Arc<TelephonyConfig>,
    provider: Arc<Box<dyn TelephonyProviderImpl>>,
    control_tx: mpsc::UnboundedSender<crate::TransportEvent>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            TransportCommand::Close => {
                info!("[telephony:{}] Close command received", provider.name());
                if config.auto_hang_up {
                    if let Some(call_id) = config.get_call_id() {
                        // Retry up to 3 times with 500ms delay on failure
                        const MAX_ATTEMPTS: u32 = 3;
                        for attempt in 1..=MAX_ATTEMPTS {
                            match provider.hangup(&config, &call_id).await {
                                Ok(()) => break,
                                Err(e) => {
                                    if attempt < MAX_ATTEMPTS {
                                        warn!(
                                            "[telephony:{}] Hangup attempt {}/{} failed: {} — retrying",
                                            provider.name(),
                                            attempt,
                                            MAX_ATTEMPTS,
                                            e
                                        );
                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                    } else {
                                        warn!(
                                            "[telephony:{}] Hangup failed after {} attempts: {}",
                                            provider.name(),
                                            attempt,
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(
                            "[telephony:{}] auto_hang_up enabled but no call_id",
                            provider.name()
                        );
                    }
                }
                break;
            }
            TransportCommand::TransferCompletedEndSession => {
                info!("[telephony:{}] Transfer succeeded, gracefully ending session loop without forcing hangup.", provider.name());
                break;
            }
            TransportCommand::SendMessage(msg) => {
                info!("[telephony:{}] Control message: {:?}", provider.name(), msg);
            }
            TransportCommand::Transfer { destination } => {
                info!("[telephony:{}] Supervised transfer command received → {}", provider.name(), destination);

                let call_id = config.get_call_id();
                let from_number = config.from_number.clone();
                let public_url = config.public_url.clone();

                match (call_id, from_number, public_url) {
                    (Some(call_id), Some(from_number), Some(public_url)) => {
                        // Build a unique conference name from the original call SID.
                        let conference_name = format!("feros-transfer-{}", call_id);
                        // Transfer ID is the original call SID — used as the lookup key in transfer_waiters.
                        // The original call_id is used as the transfer_waiters lookup key.
                        let callback_url = format!(
                            "{}/telephony/{}/transfer-status?original_call_id={}",
                            public_url.trim_end_matches('/'),
                            provider.name().to_lowercase(),
                            call_id
                        );

                        info!(
                            "[telephony:{}] Initiating supervised transfer: call_id={}, from={}, conference={}, callback={}",
                            provider.name(), call_id, from_number, conference_name, callback_url
                        );

                        const MAX_ATTEMPTS: u32 = 3;
                        let mut transfer_initiated = false;
                        for attempt in 1..=MAX_ATTEMPTS {
                            match provider
                                .initiate_supervised_transfer(
                                    &config,
                                    &call_id,
                                    &destination,
                                    &from_number,
                                    &conference_name,
                                    &callback_url,
                                )
                                .await
                            {
                                Ok(new_call_sid) => {
                                    info!(
                                        "[telephony:{}] Outbound transfer leg created: {}",
                                        provider.name(),
                                        new_call_sid
                                    );
                                    transfer_initiated = true;
                                    break;
                                }
                                Err(e) => {
                                    if attempt < MAX_ATTEMPTS {
                                        warn!(
                                            "[telephony:{}] Transfer attempt {}/{} failed: {} — retrying",
                                            provider.name(), attempt, MAX_ATTEMPTS, e
                                        );
                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                    } else {
                                        warn!(
                                            "[telephony:{}] Transfer failed after {} attempts: {}",
                                            provider.name(), attempt, e
                                        );
                                    }
                                }
                            }
                        }

                        if !transfer_initiated {
                            // Emit a failure event so the AI can tell the user.
                            let _ = control_tx.send(crate::TransportEvent::TransferResult {
                                succeeded: false,
                                destination: destination.clone(),
                                reason: Some("Failed to place outbound transfer call".into()),
                            });
                        }
                        // Do NOT break — we wait for TransferResult from the webhook or a Close command.
                    }
                    (None, _, _) => {
                        warn!("[telephony:{}] Transfer requested but no call_id available", provider.name());
                        let _ = control_tx.send(crate::TransportEvent::TransferResult {
                            succeeded: false,
                            destination,
                            reason: Some("No call_id available for transfer".into()),
                        });
                    }
                    (_, None, _) => {
                        warn!("[telephony:{}] Transfer requested but no from_number configured — cannot place outbound call", provider.name());
                        let _ = control_tx.send(crate::TransportEvent::TransferResult {
                            succeeded: false,
                            destination,
                            reason: Some("No from_number configured".into()),
                        });
                    }
                    (_, _, None) => {
                        warn!("[telephony:{}] Transfer requested but no public_url configured — cannot build callback URL", provider.name());
                        let _ = control_tx.send(crate::TransportEvent::TransferResult {
                            succeeded: false,
                            destination,
                            reason: Some("No public_url configured".into()),
                        });
                    }
                }
            }
            #[cfg(feature = "webrtc")]
            TransportCommand::AddIceCandidate(_) => {
                // No-op for telephony.
            }
        }
    }
}

// ── Audio Sink ──────────────────────────────────────────────────

struct TelephonyAudioSink {
    ws_sink: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    config: Arc<TelephonyConfig>,
    provider: Arc<Box<dyn TelephonyProviderImpl>>,
}

#[async_trait]
impl TransportAudioSink for TelephonyAudioSink {
    async fn send_audio(&self, pcm: Bytes) -> Result<(), TransportError> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        let encoded = match self.config.outbound_encoding {
            TelephonyEncoding::Pcmu => codec::pcm16_to_ulaw(&pcm),
            TelephonyEncoding::Pcma => codec::pcm16_to_alaw(&pcm),
        };
        let payload = b64.encode(&encoded);
        let stream_id = self.config.get_stream_id();
        let json = self.provider.media_frame(&payload, &stream_id);

        let msg =
            serde_json::to_string(&json).map_err(|e| TransportError::SendFailed(e.to_string()))?;

        let mut sink = self.ws_sink.lock().await;
        sink.send(Message::Text(msg.into()))
            .await
            .map_err(|e| TransportError::SendFailed(e.to_string()))
    }

    async fn interrupt(&self) -> Result<(), TransportError> {
        let stream_id = self.config.get_stream_id();
        let json = self.provider.clear_frame(&stream_id);

        let msg =
            serde_json::to_string(&json).map_err(|e| TransportError::SendFailed(e.to_string()))?;

        let mut sink = self.ws_sink.lock().await;
        sink.send(Message::Text(msg.into()))
            .await
            .map_err(|e| TransportError::SendFailed(e.to_string()))
    }
}
