//! WebRTC connection — wraps a `str0m::Rtc` instance in a tokio task.
//!
//! Manages the Sans I/O event loop: polls str0m for outputs (transmit, events),
//! feeds it UDP input, and bridges audio/data channel events to channels.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{TransportCommand, TransportEvent};

// ── Constants ────────────────────────────────────────────────────

use super::OPUS_SAMPLE_RATE;

/// Opus frame size: 20ms at 48kHz = 960 samples.
const OPUS_FRAME_SAMPLES: usize = 960;
/// Max Opus packet size (recommended by opus spec).
const MAX_OPUS_PACKET_SIZE: usize = 4000;

// ── Internal command for the event loop ─────────────────────────

/// Commands sent from the audio sink to the str0m event loop.
///
/// Because `Rtc` is `!Sync` and lives inside the event loop task,
/// all interactions with str0m media writers must be proxied.
pub(crate) enum RtcInternalCmd {
    /// PCM16 mono audio at 48kHz to encode and send via RTP.
    SendAudio(Bytes),
    /// Flush the pacing buffer (barge-in / interrupt).
    ClearAudio,
}

// ── WebRtcConnection ────────────────────────────────────────────

/// An active WebRTC connection backed by str0m.
///
/// Created by the signaling endpoint, produces channels that feed
/// into a [`TransportHandle`].
pub struct WebRtcConnection {
    /// Unique connection ID for logging.
    pub id: String,
    /// Channel sending decoded PCM16 audio to the Reactor.
    pub(crate) audio_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    /// Channel for transport lifecycle events.
    pub(crate) control_rx: Option<mpsc::UnboundedReceiver<TransportEvent>>,
    /// Channel for sending events back to the session.
    pub(crate) event_tx: Option<mpsc::UnboundedSender<TransportEvent>>,
    /// Channel for sending commands to the connection loop.
    pub(crate) control_tx: Option<mpsc::UnboundedSender<TransportCommand>>,
    /// Channel for sending PCM audio to the str0m event loop for Opus encoding.
    pub(crate) audio_out_tx: Option<mpsc::UnboundedSender<RtcInternalCmd>>,
    /// Join handle for the connection task.
    pub(crate) task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl WebRtcConnection {
    /// Create a WebRTC connection from an SDP offer.
    ///
    /// Returns `(connection, sdp_answer_json)`.
    ///
    /// # Arguments
    ///
    /// - `offer_json` — the SDP offer from the browser
    /// - `stun_server` — STUN server address for public IP discovery
    ///   (e.g. `"stun.cloudflare.com:3478"`)
    ///
    /// The connection spawns a tokio task that:
    /// 1. Obtains the shared UDP socket from the UdpMux
    /// 2. Runs the str0m event loop
    /// 3. Decodes incoming Opus audio → PCM16 → `audio_tx`
    /// 4. Encodes outgoing PCM16 → Opus → RTP via str0m writer
    /// 5. Forwards data channel messages bidirectionally
    pub async fn from_offer(
        offer_json: serde_json::Value,
        stun_server: &str,
        mux: Arc<super::multiplexer::UdpMux>,
    ) -> Result<(Self, serde_json::Value), Box<dyn std::error::Error + Send + Sync>> {
        let id = uuid::Uuid::new_v4().to_string();
        let offer: SdpOffer = serde_json::from_value(offer_json)?;

        let ice_lite = std::env::var("WEBRTC__ICE_LITE")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        // Create str0m Rtc instance
        let mut rtc = RtcConfig::new().set_ice_lite(ice_lite).build(Instant::now());

        let socket = mux.socket();
        let bound_addr = socket.local_addr()?;
        info!("[webrtc:{}] Using shared UDP socket on {}", &id[..8], bound_addr);

        let configured_public_ips: Vec<std::net::IpAddr> = std::env::var("WEBRTC__PUBLIC_IP")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|p| p.trim().parse::<std::net::IpAddr>().ok())
                    .collect()
            })
            .unwrap_or_default();

        let local_ips = if !configured_public_ips.is_empty() {
            info!("[webrtc:{}] Using WEBRTC__PUBLIC_IP(s)={:?}", &id[..8], configured_public_ips);
            configured_public_ips.clone()
        } else if !bound_addr.ip().is_unspecified() && !bound_addr.ip().is_loopback() {
            info!("[webrtc:{}] Bound to specific IP {}; using as public IP", &id[..8], bound_addr.ip());
            vec![bound_addr.ip()]
        } else {
            let probe = UdpSocket::bind("0.0.0.0:0").await?;
            probe.connect("8.8.8.8:80").await?;
            vec![probe.local_addr()?.ip()]
        };

        let mut primary_host_addr = None;

        for ip in &local_ips {
            let host_addr = std::net::SocketAddr::new(*ip, bound_addr.port());
            if primary_host_addr.is_none() {
                primary_host_addr = Some(host_addr);
            }
            // Add host candidate (local network address)
            let host_candidate = Candidate::host(host_addr, "udp")
                .map_err(|e| format!("Failed to create host ICE candidate: {}", e))?;
            rtc.add_local_candidate(host_candidate);
            info!("[webrtc:{}] Host candidate: {}", &id[..8], host_addr);
        }

        // Resolve public IP via STUN Binding Request.
        // Skip srflx in explicit local override mode to avoid advertising
        // unreachable candidates that can win pair selection.
        if configured_public_ips.is_empty() {
            if let Ok(stun_addr) = tokio::net::lookup_host(stun_server)
                .await
                .map(|mut addrs| addrs.next())
            {
                if let Some(stun_addr) = stun_addr {
                    match super::stun::stun_binding(stun_addr).await {
                        Some(public_addr) => {
                            if let Some(host_addr) = primary_host_addr {
                                let srflx = Candidate::server_reflexive(public_addr, host_addr, "udp")
                                    .map_err(|e| format!("Failed to create srflx candidate: {}", e))?;
                                rtc.add_local_candidate(srflx);
                                info!(
                                    "[webrtc:{}] Server-reflexive candidate: {} (via STUN {})",
                                    &id[..8],
                                    public_addr,
                                    stun_server
                                );
                            }
                        }
                        None => {
                            warn!(
                                "[webrtc:{}] STUN binding failed ({}) — using host candidate only",
                                &id[..8],
                                stun_server
                            );
                        }
                    }
                }
            }
        } else {
            info!(
                "[webrtc:{}] Skipping STUN srflx candidate because WEBRTC__PUBLIC_IP is set",
                &id[..8]
            );
        }

        // Accept the offer and get the answer
        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| format!("Failed to accept SDP offer: {}", e))?;

        let answer_json = serde_json::to_value(&answer)?;

        let sdp_str = answer_json.get("sdp").and_then(|v| v.as_str()).unwrap_or("");
        let ufrag = sdp_str
            .lines()
            .find_map(|l| l.strip_prefix("a=ice-ufrag:"))
            .unwrap_or("")
            .to_string();
        if ufrag.is_empty() {
            return Err("Failed to extract ice-ufrag from SDP answer — cannot register with UdpMux".into());
        }

        // Create channels
        let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Bytes>();
        let (control_event_tx, control_rx) = mpsc::unbounded_channel();
        let (control_cmd_tx, control_cmd_rx) = mpsc::unbounded_channel();
        let (audio_out_tx, audio_out_rx) = mpsc::unbounded_channel();
        let (mux_tx, mux_rx) = mpsc::unbounded_channel();

        mux.register(ufrag.clone(), mux_tx).await;

        let id_for_task = id.clone();
        let control_event_tx_task = control_event_tx.clone();
        let mux_for_task = mux.clone();
        let ufrag_for_task = ufrag.clone();

        // Spawn the str0m event loop as a tokio task
        let task_handle = tokio::spawn(async move {
            if let Err(e) = run_rtc_loop(
                id_for_task.clone(),
                rtc,
                socket,
                local_ips,
                bound_addr.port(),
                primary_host_addr.expect("primary_host_addr should always be populated by local_ips"),
                audio_tx,
                control_event_tx_task,
                control_cmd_rx,
                audio_out_rx,
                mux_rx,
                mux_for_task,
                ufrag_for_task,
            )
            .await
            {
                error!("[webrtc:{}] Event loop error: {}", &id_for_task[..8], e);
            }
        });

        let conn = Self {
            id,
            audio_rx: Some(audio_rx),
            control_rx: Some(control_rx),
            event_tx: Some(control_event_tx.clone()),
            control_tx: Some(control_cmd_tx),
            audio_out_tx: Some(audio_out_tx),
            task_handle: Some(task_handle),
        };

        Ok((conn, answer_json))
    }
}

impl Drop for WebRtcConnection {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}

// ── str0m Event Loop ────────────────────────────────────────────

struct UdpMuxGuard {
    mux: Arc<super::multiplexer::UdpMux>,
    ufrag: String,
}

impl Drop for UdpMuxGuard {
    fn drop(&mut self) {
        let mux = self.mux.clone();
        let ufrag = self.ufrag.clone();
        tokio::spawn(async move {
            mux.unregister(&ufrag).await;
        });
    }
}

/// The main Sans I/O event loop for a single WebRTC connection.
///
/// Runs until the ICE connection disconnects or an error occurs.
/// Uses tokio's `UdpSocket` for async I/O instead of blocking sockets.
async fn run_rtc_loop(
    id: String,
    mut rtc: Rtc,
    socket: Arc<UdpSocket>,
    local_ips: Vec<std::net::IpAddr>,
    bound_port: u16,
    primary_host_addr: std::net::SocketAddr,
    audio_tx: mpsc::UnboundedSender<Bytes>,
    control_tx: mpsc::UnboundedSender<TransportEvent>,
    mut control_rx: mpsc::UnboundedReceiver<TransportCommand>,
    mut audio_out_rx: mpsc::UnboundedReceiver<RtcInternalCmd>,
    mut mux_rx: mpsc::UnboundedReceiver<(Bytes, std::net::SocketAddr)>,
    mux: Arc<super::multiplexer::UdpMux>,
    ufrag: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tag = &id[..8];
    // socket is already Arc<UdpSocket> (shared from UdpMux); no re-wrapping needed.

    let _mux_guard = UdpMuxGuard {
        mux: mux.clone(),
        ufrag: ufrag.clone(),
    };

    // Opus decoder for incoming audio (browser → server)
    let mut opus_decoder: Option<opus::Decoder> = None;
    let mut decode_buf = vec![0i16; OPUS_FRAME_SAMPLES * 2]; // stereo max

    // Opus encoder for outgoing audio (server → browser)
    let mut opus_encoder: Option<opus::Encoder> = None;
    let mut encode_buf = vec![0u8; MAX_OPUS_PACKET_SIZE];

    // Track the audio Mids
    let mut recv_mid: Option<Mid> = None;
    let mut send_mid: Option<Mid> = None;

    // Track the data channel ID once it opens
    let mut data_channel_id: Option<ChannelId> = None;

    // Media time counter for outgoing audio (in RTP clock ticks at 48kHz)
    let mut media_time: u64 = 0;

    // ── Pacing buffer for outgoing audio ──────────────────────────
    // Instead of writing all frames at once (which str0m can't pace),
    // we queue PCM samples and pop one 20ms frame every tick.
    let mut audio_pace_buf: std::collections::VecDeque<i16> = std::collections::VecDeque::new();
    // 20ms pacing interval — one Opus frame per tick
    let mut pace_interval = tokio::time::interval(std::time::Duration::from_millis(20));
    pace_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!("[webrtc:{}] Event loop started", tag);

    loop {
        // 1. Drive str0m's internal timers (ICE keepalives, RTCP, etc.)
        //    This must happen every iteration — the pacing tick fires every
        //    20ms, which would starve the old `tokio::sleep(duration)` branch.
        rtc.handle_input(Input::Timeout(Instant::now()))?;

        // 2. Poll str0m for outputs (transmit queued packets, handle events)
        let timeout = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,

                Ok(Output::Transmit(t)) => {
                    if let Err(e) = socket.send_to(&t.contents, t.destination).await {
                        warn!("[webrtc:{}] UDP send error: {}", tag, e);
                    }
                    continue;
                }

                Ok(Output::Event(event)) => {
                    handle_str0m_event(
                        tag,
                        &event,
                        &audio_tx,
                        &control_tx,
                        &mut opus_decoder,
                        &mut opus_encoder,
                        &mut decode_buf,
                        &mut recv_mid,
                        &mut send_mid,
                        &mut data_channel_id,
                    );

                    if event == Event::IceConnectionStateChange(IceConnectionState::Disconnected) {
                        info!("[webrtc:{}] ICE disconnected — stopping", tag);
                        let _ = control_tx.send(TransportEvent::Disconnected {
                            reason: "ICE disconnected".to_string(),
                        });
                        return Ok(());
                    }
                    continue;
                }

                Err(e) => {
                    error!("[webrtc:{}] str0m error: {}", tag, e);
                    return Err(e.into());
                }
            }
        };

        // 3. Calculate how long until str0m needs attention again
        let now = Instant::now();
        let duration = timeout.saturating_duration_since(now);

        // 4. Wait for UDP input, control commands, audio output, pacing tick,
        //    or str0m timeout — whichever comes first.
        tokio::select! {
            biased;

            // Incoming UDP packet from multiplexer
            result = mux_rx.recv() => {
                match result {
                    Some((packet, source)) => {
                        let is_stun = packet.len() >= 20 && (packet[0] == 0x00 || packet[0] == 0x01);
                        if !is_stun {
                            tracing::trace!("[webrtc:{}] Feeding RTP/other packet (len={}) from {} to str0m", tag, packet.len(), source);
                        } else {
                            tracing::debug!("[webrtc:{}] Feeding STUN packet (len={}, type={:02x}{:02x}) from {} to str0m", tag, packet.len(), packet[0], packet[1], source);
                        }
                        let dest_ip = if local_ips.contains(&source.ip()) {
                            source.ip()
                        } else {
                            primary_host_addr.ip()
                        };
                        let destination = std::net::SocketAddr::new(dest_ip, bound_port);

                        let input = Input::Receive(
                            Instant::now(),
                            Receive {
                                proto: Protocol::Udp,
                                source,
                                destination,
                                contents: (&packet[..]).try_into()?,
                            },
                        );
                        rtc.handle_input(input)?;
                    }
                    None => {
                        warn!("[webrtc:{}] mux_rx closed", tag);
                        return Ok(());
                    }
                }
            }

            // 20ms pacing tick — send exactly one Opus frame.
            // Matches Pion's pattern: one frame per tick, consistent spacing.
            // The VecDeque absorbs timing wobble from TTS chunk arrivals.
            _ = pace_interval.tick() => {
                if audio_pace_buf.len() >= OPUS_FRAME_SAMPLES {
                    if let Some(mid) = send_mid {
                        let frame: Vec<i16> = audio_pace_buf.drain(..OPUS_FRAME_SAMPLES).collect();
                        encode_and_send_one_frame(
                            tag,
                            &mut rtc,
                            mid,
                            &frame,
                            &mut opus_encoder,
                            &mut encode_buf,
                            &mut media_time,
                        );
                    } else {
                        // Drain even if send_mid is not set, to avoid unbounded growth
                        let _ = audio_pace_buf.drain(..OPUS_FRAME_SAMPLES);
                    }
                }
            }

            // Control commands from the session
            cmd = control_rx.recv() => {
                match cmd {
                    Some(TransportCommand::Close) => {
                        info!("[webrtc:{}] Close command received", tag);
                        rtc.disconnect();
                        return Ok(());
                    }
                    Some(TransportCommand::TransferCompletedEndSession) => {
                        info!("[webrtc:{}] Transfer completed. Disconnecting.", tag);
                        rtc.disconnect();
                        return Ok(());
                    }
                    Some(TransportCommand::SendMessage(json)) => {
                        if let Some(cid) = data_channel_id {
                            let msg = serde_json::to_string(&json).unwrap_or_default();
                            if let Some(mut ch) = rtc.channel(cid) {
                                if let Err(e) = ch.write(true, msg.as_bytes()) {
                                    warn!("[webrtc:{}] Data channel write error: {}", tag, e);
                                }
                            }
                        } else {
                            warn!("[webrtc:{}] SendMessage dropped: no data channel yet", tag);
                        }
                    }
                    Some(TransportCommand::AddIceCandidate(candidate)) => {
                        info!("[webrtc:{tag}] Adding remote ICE candidate: {}", candidate);

                        match str0m::Candidate::from_sdp_string(&candidate) {
                            Ok(c) => rtc.add_remote_candidate(c),
                            Err(e) => warn!("[webrtc:{tag}] Failed to parse ICE candidate {}: {}", candidate, e)
                        }
                    }
                    Some(TransportCommand::Transfer { destination }) => {
                        warn!("[webrtc:{tag}] Transfer command unsupported in WebRTC (destination: {})", destination);
                    }
                    None => {
                        info!("[webrtc:{}] Control channel closed", tag);
                        return Ok(());
                    }
                }
            }

            // Outgoing audio from the voice engine — queue into pacing buffer
            audio_cmd = audio_out_rx.recv() => {
                match audio_cmd {
                    Some(RtcInternalCmd::SendAudio(pcm)) => {
                        let samples = pcm
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]));
                        audio_pace_buf.extend(samples);
                    }
                    Some(RtcInternalCmd::ClearAudio) => {
                        let flushed = audio_pace_buf.len();
                        audio_pace_buf.clear();
                        if flushed > 0 {
                            info!("[webrtc:{}] Cleared pacing buffer ({} samples)", tag, flushed);
                        }
                    }
                    None => {
                        debug!("[webrtc:{}] Audio output channel closed", tag);
                    }
                }
            }

            // Timeout — ensure we loop back to drive str0m
            _ = tokio::time::sleep(duration) => {
                // Timeout handled at the top of the loop
            }
        }
    }
}

// ── Event Handling ──────────────────────────────────────────────

/// Handle a single str0m event.
#[allow(clippy::too_many_arguments)]
fn handle_str0m_event(
    tag: &str,
    event: &Event,
    audio_tx: &mpsc::UnboundedSender<Bytes>,
    control_tx: &mpsc::UnboundedSender<TransportEvent>,
    opus_decoder: &mut Option<opus::Decoder>,
    opus_encoder: &mut Option<opus::Encoder>,
    decode_buf: &mut [i16],
    recv_mid: &mut Option<Mid>,
    send_mid: &mut Option<Mid>,
    data_channel_id: &mut Option<ChannelId>,
) {
    match event {
        Event::IceConnectionStateChange(state) => {
            info!("[webrtc:{}] ICE state: {:?}", tag, state);
            if matches!(
                *state,
                IceConnectionState::Connected | IceConnectionState::Completed
            ) {
                let _ = control_tx.send(TransportEvent::Connected);
            }
        }

        Event::MediaAdded(media) => {
            info!(
                "[webrtc:{}] Media added: mid={}, kind={:?}, dir={:?}",
                tag, media.mid, media.kind, media.direction
            );
            if media.kind == MediaKind::Audio {
                match media.direction {
                    Direction::RecvOnly => {
                        *recv_mid = Some(media.mid);
                        init_opus_decoder(tag, opus_decoder);
                    }
                    Direction::SendOnly => {
                        *send_mid = Some(media.mid);
                        init_opus_encoder(tag, opus_encoder);
                    }
                    Direction::SendRecv => {
                        *recv_mid = Some(media.mid);
                        *send_mid = Some(media.mid);
                        init_opus_decoder(tag, opus_decoder);
                        init_opus_encoder(tag, opus_encoder);
                    }
                    Direction::Inactive => {}
                }
            }
        }

        Event::MediaData(data) => {
            // Decode Opus → PCM16
            if let Some(decoder) = opus_decoder.as_mut() {
                match decoder.decode(&data.data, decode_buf, false) {
                    Ok(samples) => {
                        // Convert i16 samples to little-endian bytes (PCM16)
                        let pcm_bytes: Vec<u8> = decode_buf[..samples]
                            .iter()
                            .flat_map(|s| s.to_le_bytes())
                            .collect();
                        let _ = audio_tx.send(Bytes::from(pcm_bytes));
                    }
                    Err(e) => {
                        warn!("[webrtc:{}] Opus decode error: {}", tag, e);
                    }
                }
            }
        }

        Event::ChannelOpen(cid, name) => {
            info!("[webrtc:{}] Data channel opened: {:?} ({})", tag, cid, name);
            *data_channel_id = Some(*cid);
        }

        Event::ChannelData(data) => {
            // Data channel message — parse as JSON control message
            if let Ok(text) = std::str::from_utf8(&data.data) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match msg_type {
                        "session.end" => {
                            info!(
                                "[webrtc:{}] Client requested session end via data channel",
                                tag
                            );
                            let _ = control_tx.send(TransportEvent::Disconnected {
                                reason: "session.end".to_string(),
                            });
                        }
                        _ => {
                            let _ = control_tx.send(TransportEvent::ControlMessage(json));
                        }
                    }
                }
            }
        }

        Event::ChannelClose(cid) => {
            info!("[webrtc:{}] Data channel closed: {:?}", tag, cid);
            if data_channel_id.as_ref() == Some(cid) {
                *data_channel_id = None;
            }
        }

        _ => {
            debug!("[webrtc:{}] Unhandled str0m event: {:?}", tag, event);
        }
    }
}

// ── Opus Helpers ────────────────────────────────────────────────

fn init_opus_decoder(tag: &str, opus_decoder: &mut Option<opus::Decoder>) {
    if opus_decoder.is_none() {
        match opus::Decoder::new(OPUS_SAMPLE_RATE, opus::Channels::Mono) {
            Ok(dec) => {
                *opus_decoder = Some(dec);
                info!("[webrtc:{}] Opus decoder initialized (48kHz mono)", tag);
            }
            Err(e) => {
                error!("[webrtc:{}] Failed to create Opus decoder: {}", tag, e);
            }
        }
    }
}

fn init_opus_encoder(tag: &str, opus_encoder: &mut Option<opus::Encoder>) {
    if opus_encoder.is_none() {
        match opus::Encoder::new(
            OPUS_SAMPLE_RATE,
            opus::Channels::Mono,
            opus::Application::Voip,
        ) {
            Ok(enc) => {
                *opus_encoder = Some(enc);
                info!(
                    "[webrtc:{}] Opus encoder initialized (48kHz mono, VoIP)",
                    tag
                );
            }
            Err(e) => {
                error!("[webrtc:{}] Failed to create Opus encoder: {}", tag, e);
            }
        }
    }
}

/// Encode a single 20ms PCM16 frame to Opus and write via str0m.
///
/// Called by the pacing timer — exactly one frame (960 samples) per tick.
fn encode_and_send_one_frame(
    tag: &str,
    rtc: &mut Rtc,
    mid: Mid,
    frame: &[i16],
    opus_encoder: &mut Option<opus::Encoder>,
    encode_buf: &mut [u8],
    media_time: &mut u64,
) {
    let encoder = match opus_encoder.as_mut() {
        Some(e) => e,
        None => return,
    };

    match encoder.encode(frame, encode_buf) {
        Ok(encoded_len) => {
            let opus_data = &encode_buf[..encoded_len];
            // Look up the payload type first (this borrows rtc immutably)
            let pt = rtc
                .writer(mid)
                .and_then(|w| w.payload_params().next().map(|p| p.pt()));
            if let Some(pt) = pt {
                // Now get a fresh writer for the actual write call
                if let Some(writer) = rtc.writer(mid) {
                    let wallclock = Instant::now();
                    let mt = str0m::media::MediaTime::new(*media_time, Frequency::FORTY_EIGHT_KHZ);
                    if let Err(e) = writer.write(pt, wallclock, mt, opus_data) {
                        warn!("[webrtc:{}] str0m write error: {}", tag, e);
                    }
                }
            }
            *media_time += OPUS_FRAME_SAMPLES as u64;
        }
        Err(e) => {
            warn!("[webrtc:{}] Opus encode error: {}", tag, e);
        }
    }
}
