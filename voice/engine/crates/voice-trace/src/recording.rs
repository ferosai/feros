//! Session recording — audio capture + transcript logging.
//!
//! Subscribes to the voice engine's [`EventBus`] and produces two output files
//! per session:
//!
//! - `{session_id}.opus` (or `.wav`) — stereo PCM16 (L=user input, R=agent TTS)
//! - `{session_id}.json` — structured transcript with timestamps
//!
//! # Architecture
//!
//! Follows the same subscriber pattern as Langfuse and OTel — a background
//! Tokio task that consumes events from the bus and accumulates data.
//! The Reactor doesn't know recording exists; it's just another bus consumer.
//!
//! # Performance
//!
//! - Zero overhead when disabled (subscriber is never spawned).
//! - Audio is buffered in `Vec<i16>`; a 5-minute stereo session at 16 kHz ≈ 19 MB (mono ≈ 9.6 MB).
//! - All file I/O runs via `tokio::task::spawn_blocking` — never blocks the Reactor.

use std::io::{Cursor, Write};

use serde::Serialize;
use tracing::{info, warn};

use crate::event::{Event, LlmCompletionData};

// ── Transcript Log Schema ───────────────────────────────────────

/// A single entry in the session transcript log.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEntry {
    /// User or assistant speech.
    Message {
        role: String,
        text: String,
        timestamp: String,
        /// `true` when the agent's response was cancelled (barge-in) before
        /// any audio reached the client. The text is preserved in context
        /// but the user never heard it.
        // `Not::not` evaluates to `true` when the value is `false`, so the
        // field is skipped (omitted from JSON) in the common case where the
        // turn completed normally. Only `was_interrupted: true` is serialised.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        was_interrupted: bool,
    },
    /// A tool execution lifecycle event.
    ToolCall {
        tool_name: String,
        status: String,
        timestamp: String,
    },
    /// LLM generation metadata (optional — controlled by config).
    LlmGeneration {
        provider: String,
        model: String,
        prompt_tokens: u32,
        completion_tokens: u32,
        duration_ms: f64,
        timestamp: String,
    },
    /// Per-turn latency metrics.
    TurnMetrics {
        turn_id: u64,
        total_ms: f64,
        user_agent_latency_ms: Option<f64>,
        timestamp: String,
    },
    /// Session lifecycle event.
    SessionEvent {
        kind: String,
        timestamp: String,
    },
}

/// The complete session transcript document.
#[derive(Debug, Clone, Serialize)]
pub struct SessionTranscript {
    pub session_id: String,
    pub started_at: String,
    pub ended_at: String,
    pub duration_secs: f64,
    pub entries: Vec<TranscriptEntry>,
}

// ── Recording Config ────────────────────────────────────────────
// Canonical types live in the `common` crate (shared with agent-kit).
pub use common::{AudioFormat, AudioLayout, RecordingConfig};

// ── Recording Output ────────────────────────────────────────────

/// The complete output of a finished recording session.
///
/// Returned by `spawn_recording_subscriber` via the `on_complete` callback.
/// Voice-trace encodes the audio **and writes it to the configured destination**
/// (local file for `file://` URIs). The `storage_uri` field tells the caller
/// exactly where bytes landed so it can derive a public URL.
///
/// ## Public URL derivation
///
/// The caller (`voice-server`) maps `storage_uri` to a public URL:
/// - `file:///path/to/file.opus` → `/api/recordings/file.opus`
///   (served by the host application from the shared recordings directory)
/// - `s3://bucket/prefix/file.opus` → `https://bucket.s3.amazonaws.com/prefix/file.opus`
///   (or a CDN URL, feature-gated behind the `s3` Cargo feature)
#[derive(Debug)]
pub struct RecordingOutput {
    /// The session identifier (used as the base filename).
    pub session_id: String,
    /// Encoded audio bytes — valid until `persist_recording()` writes them to disk/S3.
    /// After `spawn_recording_subscriber` returns `RecordingOutput` to the caller,
    /// this field is empty (the bytes were consumed during persistence).
    pub(crate) audio_bytes: Vec<u8>,
    /// File extension matching the encoding: `"opus"` or `"wav"`.
    pub audio_extension: &'static str,
    /// Number of channels in the audio: 1 (mono) or 2 (stereo, L=user R=agent).
    pub channels: u16,
    /// Total **audio** duration in seconds (derived from the buffer length).
    ///
    /// This can be shorter than wall-clock session time when `max_duration_secs`
    /// caps the recording early. Callers writing DB records should use their own
    /// wall-clock measurement for the `duration_seconds` field.
    pub duration_secs: f64,
    /// JSON-serialised `SessionTranscript`, present when `save_transcript` is enabled.
    pub transcript_json: Option<Vec<u8>>,
    /// Canonical URI where the audio bytes were stored.
    ///
    /// Examples:
    /// - `file:///abs/path/recordings/session-id.opus`
    /// - `s3://my-bucket/recordings/session-id.opus`
    ///
    /// Use this in `voice-server` to derive the `recording_url` stored in the DB.
    pub storage_uri: String,
}

// ── WAV Writer ──────────────────────────────────────────────────

/// Write a PCM16 WAV file to a byte buffer.
///
/// Supports mono (1 channel) and stereo (2 channels).
/// The WAV header is 44 bytes — no external crate needed.
fn write_wav(
    samples: &[i16],
    sample_rate: u32,
    num_channels: u16,
) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * num_channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = num_channels * (bits_per_sample / 8);
    let data_size = (samples.len() * 2) as u32;
    let file_size = 36 + data_size;

    let mut buf = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));

    // RIFF header
    buf.write_all(b"RIFF").unwrap();
    buf.write_all(&file_size.to_le_bytes()).unwrap();
    buf.write_all(b"WAVE").unwrap();

    // fmt sub-chunk
    buf.write_all(b"fmt ").unwrap();
    buf.write_all(&16u32.to_le_bytes()).unwrap(); // sub-chunk size
    buf.write_all(&1u16.to_le_bytes()).unwrap(); // PCM format
    buf.write_all(&num_channels.to_le_bytes()).unwrap();
    buf.write_all(&sample_rate.to_le_bytes()).unwrap();
    buf.write_all(&byte_rate.to_le_bytes()).unwrap();
    buf.write_all(&block_align.to_le_bytes()).unwrap();
    buf.write_all(&bits_per_sample.to_le_bytes()).unwrap();

    // data sub-chunk
    buf.write_all(b"data").unwrap();
    buf.write_all(&data_size.to_le_bytes()).unwrap();

    // PCM samples (little-endian i16)
    for &sample in samples {
        buf.write_all(&sample.to_le_bytes()).unwrap();
    }

    buf.into_inner()
}

/// Interleave two mono buffers into a stereo buffer (L=user, R=agent).
///
/// If buffers differ in length, the shorter one is zero-padded.
fn interleave_stereo(user: &[i16], agent: &[i16]) -> Vec<i16> {
    let len = user.len().max(agent.len());
    let mut stereo = Vec::with_capacity(len * 2);
    for i in 0..len {
        stereo.push(if i < user.len() { user[i] } else { 0 });
        stereo.push(if i < agent.len() { agent[i] } else { 0 });
    }
    stereo
}

/// Downmix two mono buffers into a single mono buffer.
///
/// Simple average mix: `(user + agent) / 2`, clamped to i16 range.
fn mix_mono(user: &[i16], agent: &[i16]) -> Vec<i16> {
    let len = user.len().max(agent.len());
    let mut mono = Vec::with_capacity(len);
    for i in 0..len {
        let u = if i < user.len() { user[i] as i32 } else { 0 };
        let a = if i < agent.len() { agent[i] as i32 } else { 0 };
        mono.push(((u + a) / 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16);
    }
    mono
}

// ── Opus Encoder (audiopus + ogg) ───────────────────────────────
//
// Gated behind the `opus` feature. `audiopus` vendors the Opus C source
// (no system libopus needed). `ogg` is a pure-Rust OGG container writer.
// Falls back to WAV at runtime when the feature is absent.

#[cfg(feature = "opus")]
mod opus_encode {
    use audiopus::coder::Encoder;
    use audiopus::{Application, Channels, SampleRate};
    use ogg::writing::PacketWriter;
    use std::io::Cursor;

    /// Map sample rate to audiopus SampleRate enum.
    /// Opus supports: 8000, 12000, 16000, 24000, 48000.
    fn to_opus_rate(rate: u32) -> Result<SampleRate, String> {
        match rate {
            8000 => Ok(SampleRate::Hz8000),
            12000 => Ok(SampleRate::Hz12000),
            16000 => Ok(SampleRate::Hz16000),
            24000 => Ok(SampleRate::Hz24000),
            48000 => Ok(SampleRate::Hz48000),
            _ => Err(format!(
                "Unsupported Opus sample rate: {}. Must be 8000/12000/16000/24000/48000",
                rate
            )),
        }
    }

    /// Build the 19-byte OpusHead header for the OGG stream.
    fn opus_head(channels: u8, pre_skip: u16, sample_rate: u32) -> Vec<u8> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");    // magic
        head.push(1);                           // version
        head.push(channels);                    // channel count
        head.extend_from_slice(&pre_skip.to_le_bytes());   // pre-skip
        head.extend_from_slice(&sample_rate.to_le_bytes()); // original sample rate
        head.extend_from_slice(&0u16.to_le_bytes());       // output gain
        head.push(0);                           // channel mapping family (0 = mono/stereo)
        head
    }

    /// Build OpusTags header (minimal — no metadata).
    fn opus_tags() -> Vec<u8> {
        let vendor = b"voice-trace";
        let mut tags = Vec::with_capacity(8 + 4 + vendor.len() + 4);
        tags.extend_from_slice(b"OpusTags");                   // magic
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes()); // 0 user comments
        tags
    }

    /// Encode interleaved PCM16 to OGG/Opus in memory.
    ///
    /// `channels` must be 1 (mono) or 2 (stereo).
    /// Returns the complete OGG/Opus file as bytes.
    fn encode_ogg_opus(
        pcm: &[i16],
        sample_rate: u32,
        channels: u8,
    ) -> Result<Vec<u8>, String> {
        if pcm.is_empty() {
            return Err("Cannot encode empty PCM buffer to Opus".to_string());
        }

        let opus_rate = to_opus_rate(sample_rate)?;
        let opus_channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => return Err(format!("Unsupported channel count: {}", channels)),
        };

        let encoder = Encoder::new(opus_rate, opus_channels, Application::Audio)
            .map_err(|e| format!("Opus encoder init: {}", e))?;

        // 20ms frame at the given sample rate, per channel
        let frame_samples = (sample_rate as usize * 20) / 1000; // samples per channel per frame
        let frame_total = frame_samples * channels as usize;    // total i16 values per frame

        // Pre-skip: standard Opus pre-skip is 312 samples at 48 kHz.
        // Scale to actual rate.
        let pre_skip = (312u32 * sample_rate / 48000).max(1) as u16;

        let serial: u32 = 1;
        let mut output = Cursor::new(Vec::new());
        let mut writer = PacketWriter::new(&mut output);

        // Write OGG page 0: OpusHead
        let head = opus_head(channels, pre_skip, sample_rate);
        writer.write_packet(
            head,
            serial,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0, // granule = 0 for header
        ).map_err(|e| format!("OGG write head: {}", e))?;

        // Write OGG page 1: OpusTags
        let tags = opus_tags();
        writer.write_packet(
            tags,
            serial,
            ogg::writing::PacketWriteEndInfo::EndPage,
            0,
        ).map_err(|e| format!("OGG write tags: {}", e))?;

        // Encode audio frames
        let mut opus_buf = vec![0u8; 4000]; // max Opus frame is ~4000 bytes
        let mut granule_pos: u64 = 0;
        let total_frames = (pcm.len() + frame_total - 1) / frame_total;

        for (i, chunk) in pcm.chunks(frame_total).enumerate() {
            // Pad last frame if needed
            let frame: Vec<i16>;
            let input = if chunk.len() < frame_total {
                frame = {
                    let mut padded = chunk.to_vec();
                    padded.resize(frame_total, 0);
                    padded
                };
                &frame
            } else {
                chunk
            };

            let encoded_len = encoder
                .encode(input, &mut opus_buf)
                .map_err(|e| format!("Opus encode frame: {}", e))?;

            granule_pos += frame_samples as u64;

            let is_last = i == total_frames - 1;
            let end_info = if is_last {
                ogg::writing::PacketWriteEndInfo::EndStream
            } else {
                ogg::writing::PacketWriteEndInfo::NormalPacket
            };

            writer.write_packet(
                opus_buf[..encoded_len].to_vec(),
                serial,
                end_info,
                granule_pos,
            ).map_err(|e| format!("OGG write frame: {}", e))?;
        }

        drop(writer);
        Ok(output.into_inner())
    }

    /// Encode mono PCM16 samples to OGG/Opus bytes.
    pub fn encode_mono(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, String> {
        encode_ogg_opus(samples, sample_rate, 1)
    }

    /// Encode stereo PCM16 samples (separate L/R) to OGG/Opus bytes.
    ///
    /// Interleaves L/R before encoding. Shorter buffer is zero-padded.
    pub fn encode_stereo(
        left: &[i16],
        right: &[i16],
        sample_rate: u32,
    ) -> Result<Vec<u8>, String> {
        let len = left.len().max(right.len());

        // Interleave L/R for Opus (expects [L0, R0, L1, R1, ...])
        let mut interleaved = Vec::with_capacity(len * 2);
        for i in 0..len {
            interleaved.push(if i < left.len() { left[i] } else { 0 });
            interleaved.push(if i < right.len() { right[i] } else { 0 });
        }

        encode_ogg_opus(&interleaved, sample_rate, 2)
    }
}

// ── PCM conversion helpers ──────────────────────────────────────

/// Convert raw PCM16 little-endian bytes to i16 samples.
///
/// Panics in debug builds if `data` has an odd length (misaligned PCM).
fn bytes_to_i16(data: &[u8]) -> Vec<i16> {
    debug_assert!(data.len() % 2 == 0, "PCM data length must be even, got {} bytes", data.len());
    data.chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

// ── Recording Subscriber ────────────────────────────────────────

/// Spawn a recording subscriber for a voice session.
///
/// ## Usage pattern
///
/// Subscribe to the Tracer **before** passing it to voice-engine so that no
/// early events are missed:
///
/// ```ignore
/// let tracer = Tracer::new();
/// let rx = tracer.subscribe();          // subscribe first
/// spawn_recording_subscriber(rx, session_id, options, None::<fn(_)>);
/// // now pass `tracer` to voice-engine — recording subscriber is already listening
/// ```
///
/// ## Stereo timing
///
/// Audio chunks are placed at their wall-clock offset within the session
/// rather than being concatenated into independent per-channel buffers.
/// This ensures that a user turn at t=2s and an agent reply at t=5s land
/// at the correct positions in the final stereo recording, and playback
/// reflects the real conversation timeline.
///
/// ## Parameters
///
/// - `rx`: broadcast receiver already subscribed to the session Tracer.
/// - `session_id`: base name for output files.
/// - `options`: recording configuration (format, layout, sample rate, …).
/// - `on_complete`: optional callback fired with `Some(RecordingOutput)` on
///   success or `None` if nothing was recorded. Use this to write to disk,
///   upload to S3, write a DB record, etc.
///
/// Returns immediately — the subscriber runs in a background task.
pub fn spawn_recording_subscriber<F>(
    rx: tokio::sync::broadcast::Receiver<Event>,
    session_id: String,
    options: RecordingConfig,
    on_complete: Option<F>,
) where
    F: FnOnce(Option<RecordingOutput>) + Send + 'static,
{
    let started_at = utc_now_iso8601();

    // Compute the max number of samples allowed per channel (0 = unlimited).
    let max_samples: usize = if options.max_duration_secs > 0 {
        options.max_duration_secs as usize * options.sample_rate as usize
    } else {
        0
    };

    tokio::spawn(async move {
        let output = run_recording_loop(
            rx,
            &session_id,
            &options,
            &started_at,
            max_samples,
        )
        .await;

        // Persist bytes to the configured destination and attach storage_uri.
        let output = match output {
            Some(o) => Some(persist_recording(o, &options.output_uri).await),
            None => None,
        };

        if let Some(cb) = on_complete {
            cb(output);
        }

        info!("[recording] Session subscriber stopped: {}", session_id);
    });
}

// ── Storage URI helpers ──────────────────────────────────────────

/// Resolve a `file://` URI to an absolute filesystem path string.
///
/// Handles both `file:///absolute` and `file://./relative` forms.
/// Returns `None` if the URI doesn't start with `file://`.
fn resolve_file_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    if rest.starts_with('/') {
        // file:///abs/path → /abs/path
        Some(std::path::PathBuf::from(rest))
    } else if let Some(rel) = rest.strip_prefix("./") {
        // file://./relative → ./relative (resolved by std to CWD)
        Some(std::path::PathBuf::from(format!("./{rel}")))
    } else {
        // file://relative — treat as relative
        Some(std::path::PathBuf::from(rest))
    }
}

/// Write encoded audio (and optional transcript) to a `file://` destination,
/// then return the output with `storage_uri` populated.
///
/// On S3 (feature-gated), upload asynchronously.
async fn persist_recording(mut output: RecordingOutput, output_uri: &str) -> RecordingOutput {
    if let Some(dir) = resolve_file_path(output_uri) {
        // ── file:// ──────────────────────────────────────────────
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("[recording] Could not create dir {:?}: {}", dir, e);
            output.storage_uri = String::new();
            return output;
        }

        let audio_filename = format!("{}.{}", output.session_id, output.audio_extension);
        let audio_path = dir.join(&audio_filename);

        // Note: audio_bytes are not stored on RecordingOutput after this call —
        // we write them here and discard to avoid holding the buffer in memory.
        // The storage_uri is the durable reference going forward.
        if let Err(e) = tokio::fs::write(&audio_path, &output.audio_bytes).await {
            warn!("[recording] Failed to write {:?}: {}", audio_path, e);
            output.storage_uri = String::new();
            return output;
        }

        info!(
            "[recording] Saved {:.1}s {}ch {} ({} bytes) → {:?}",
            output.duration_secs,
            output.channels,
            output.audio_extension.to_uppercase(),
            output.audio_bytes.len(),
            audio_path,
        );

        if let Some(ref tx_bytes) = output.transcript_json {
            let json_path = dir.join(format!("{}.json", output.session_id));
            if let Err(e) = tokio::fs::write(&json_path, tx_bytes).await {
                warn!("[recording] Failed to write transcript {:?}: {}", json_path, e);
            } else {
                info!("[recording] Saved transcript → {:?}", json_path);
            }
        }

        // Build canonical absolute file URI for the caller.
        let abs_path = audio_path
            .canonicalize()
            .unwrap_or(audio_path);
        output.storage_uri = format!("file://{}", abs_path.display());
        return output;
    }

    #[cfg(feature = "s3")]
    if output_uri.starts_with("s3://") {
        // S3 upload — implemented in the s3 feature module.
        output.storage_uri = crate::s3_store::upload_async(&output, output_uri).await
            .unwrap_or_else(|e| {
                warn!("[recording] S3 upload failed: {} — recording lost", e);
                String::new()
            });
        return output;
    }

    warn!("[recording] Unrecognised output_uri scheme: '{}' — recording bytes discarded", output_uri);
    output.storage_uri = String::new();
    output
}

/// Core recording loop — collects audio/events, flushes at SessionEnded.
///
/// Returns a [`RecordingOutput`] with the encoded audio bytes and optional
/// transcript JSON. No file I/O happens inside voice-trace; the caller
/// receives bytes and decides where to write them.
async fn run_recording_loop(
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    session_id: &str,
    options: &RecordingConfig,
    started_at: &str,
    max_samples: usize,
) -> Option<RecordingOutput> {
    // ── Timeline-based audio accumulation ───────────────────────
    //
    // Both channels use a sample-position timeline: `tl_user[n]` and
    // `tl_agent[n]` hold the sample at position n from session start.
    // Gaps (silence) stay at 0; overlapping chunks are mixed (add+clamp).
    let mut tl_user: Vec<i16> = Vec::new();
    let mut tl_agent: Vec<i16> = Vec::new();

    let session_start = tokio::time::Instant::now();

    let mut user_resampler: Option<soxr::SoxrStreamResampler> = None;
    let mut agent_resamplers = std::collections::HashMap::<u32, soxr::SoxrStreamResampler>::new();

    // ── User channel: cursor-based placement ────────────────────
    // Continuous 512-sample frames arrive even during silence, so a monotonic
    // cursor advances correctly without any wall-clock arithmetic.
    let mut user_write_cursor: usize = 0;

    // ── Agent channel: hybrid cursor placement ──────────────────
    // We use a monotonic write cursor (like the user channel) that advances
    // by the actual resampled output size. This avoids the soxr filter-delay
    // problem: soxr's group delay means its output is shorter than the
    // proportional stride between reactor timestamps, creating gaps that later
    // chunks fill additively — the additive mix of displaced samples causes
    // the "static" distortion.
    //
    // Inter-turn silence is captured by snapping the cursor forward on the
    // first chunk of each new turn using the reactor's `offset_samples`
    // (converted to recording rate). This is the same model as AgentAudioCursor
    // in the reactor: begin_turn snaps to wall clock, subsequent chunks
    // advance strictly by sample count.
    let mut agent_write_cursor: usize = 0;

    // Barge-in tracking: when Event::Interrupt arrives, the agent is muted.
    // All in-flight AgentAudio chunks from the cancelled turn are discarded.
    // The muting is lifted exactly when the reactor signals the start of a
    // new turn via Event::TurnStarted.
    let mut agent_muted: bool = false;

    let mut transcript: Vec<TranscriptEntry> = Vec::new();
    let mut warned_large_buffer = false;
    let mut duration_capped = false;
    // Index of the most recent `assistant` Message in transcript — used to
    // retroactively mark it `was_interrupted` when TurnEnded fires.
    let mut last_assistant_idx: Option<usize> = None;

    transcript.push(TranscriptEntry::SessionEvent {
        kind: "started".to_string(),
        timestamp: started_at.to_string(),
    });

    loop {
        match rx.recv().await {
            Ok(event) => match event {
                Event::UserAudio { pcm: data, sample_rate: in_rate } => {
                    if duration_capped {
                        continue;
                    }
                    // User audio: continuous frames — cursor advances by chunk size.
                    let resampled_bytes = if in_rate != options.sample_rate {
                        let resampler = user_resampler.get_or_insert_with(|| {
                            soxr::SoxrStreamResampler::new(in_rate, options.sample_rate).unwrap()
                        });
                        resampler.process(&data)
                    } else {
                        data.to_vec()
                    };
                    let samples = bytes_to_i16(&resampled_bytes);
                    place_samples(&mut tl_user, &samples, user_write_cursor);
                    user_write_cursor += samples.len();

                    if max_samples > 0 && tl_user.len() >= max_samples {
                        duration_capped = true;
                        let secs = tl_user.len() as f64 / options.sample_rate as f64;
                        warn!(
                            "[recording] Max recording duration reached ({:.0}s) — \
                            audio capture stopped, transcript continues",
                            secs,
                        );
                    }
                }

                Event::AgentAudio { pcm: data, sample_rate: chunk_rate, offset_samples: chunk_offset } => {
                    if duration_capped {
                        continue;
                    }

                    // Convert reactor offset to recording-rate samples (for silence snapping).
                    let converted_offset = if chunk_rate == options.sample_rate {
                        chunk_offset as usize
                    } else {
                        (chunk_offset as f64 * options.sample_rate as f64 / chunk_rate as f64)
                            .round() as usize
                    };

                    // Barge-in guard: drop in-flight chunks from the cancelled turn.
                    if agent_muted {
                        continue;
                    }

                    // Resample if needed.
                    let resampled_bytes = if chunk_rate != options.sample_rate {
                        let resampler = agent_resamplers.entry(chunk_rate).or_insert_with(|| {
                            soxr::SoxrStreamResampler::new(chunk_rate, options.sample_rate).unwrap()
                        });
                        resampler.process(&data)
                    } else {
                        data.to_vec()
                    };
                    let samples = bytes_to_i16(&resampled_bytes);

                    // Hybrid cursor: snap forward when the reactor's offset is ahead
                    // of our write cursor.  This happens on the first chunk of each
                    // new turn because AgentAudioCursor::begin_turn() snaps to wall
                    // clock, producing an offset larger than the previous turn's end.
                    // Snapping here encodes the inter-turn silence (LLM latency, user
                    // speech, tool calls) without needing a separate "turn started" event.
                    //
                    // Within a turn we advance strictly by the actual resampled sample
                    // count — NOT by the converted reactor offset stride — to avoid the
                    // soxr filter-delay mismatch that would create tiny gaps filled by
                    // displaced samples additively, causing static/distortion.
                    if converted_offset > agent_write_cursor {
                        agent_write_cursor = converted_offset;
                    }
                    let start = agent_write_cursor;
                    agent_write_cursor += samples.len();

                    place_samples(&mut tl_agent, &samples, start);

                    if max_samples > 0 {
                        let longer = tl_user.len().max(tl_agent.len());
                        if longer >= max_samples {
                            duration_capped = true;
                            let secs = longer as f64 / options.sample_rate as f64;
                            warn!(
                                "[recording] Max recording duration reached ({:.0}s) — \
                                audio capture stopped, transcript continues",
                                secs,
                            );
                            continue;
                        }
                    }

                    // Warn once when memory usage exceeds ~100 MB
                    if !warned_large_buffer {
                        let total_samples = tl_user.len() + tl_agent.len();
                        if total_samples * 2 > 100 * 1024 * 1024 {
                            warned_large_buffer = true;
                            let mb = (total_samples * 2) / (1024 * 1024);
                            warn!(
                                "[recording] Audio buffer is large ({} MB, ~{:.0}min) — \
                                consider setting max_duration_secs",
                                mb,
                                total_samples as f64 / options.sample_rate as f64 / 60.0,
                            );
                        }
                    }
                }

                Event::Transcript { role, text } => {
                    let is_assistant = role == "assistant";
                    let idx = transcript.len();
                    transcript.push(TranscriptEntry::Message {
                        role,
                        text,
                        timestamp: utc_now_iso8601(),
                        was_interrupted: false,
                    });
                    if is_assistant {
                        last_assistant_idx = Some(idx);
                    } else {
                        // User spoke — any previous assistant turn is now confirmed
                        // as heard (or already marked interrupted). Clear the index
                        // so a new assistant response starts fresh.
                        last_assistant_idx = None;
                    }
                }

                Event::TurnEnded { was_interrupted, .. } => {
                    if was_interrupted {
                        // Mark the last assistant message as silenced.
                        // The LLM continued running during barge-in, so the text
                        // is in context, but no audio reached the client.
                        if let Some(idx) = last_assistant_idx.take() {
                            if let Some(TranscriptEntry::Message {
                                was_interrupted: ref mut flag, ..
                            }) = transcript.get_mut(idx)
                            {
                                *flag = true;
                            }
                        }
                    } else {
                        // Turn completed normally (or no assistant message yet).
                        last_assistant_idx = None;
                    }
                }

                Event::ToolActivity {
                    tool_name, status, ..
                } => {
                    if options.include_tool_details {
                        transcript.push(TranscriptEntry::ToolCall {
                            tool_name,
                            status,
                            timestamp: utc_now_iso8601(),
                        });
                    }
                }

                Event::LlmComplete(LlmCompletionData {
                    provider,
                    model,
                    prompt_tokens,
                    completion_tokens,
                    duration_ms,
                    ..
                }) => {
                    if options.include_llm_metadata {
                        transcript.push(TranscriptEntry::LlmGeneration {
                            provider,
                            model,
                            prompt_tokens,
                            completion_tokens,
                            duration_ms,
                            timestamp: utc_now_iso8601(),
                        });
                    }
                }

                Event::TurnMetrics(metrics) => {
                    transcript.push(TranscriptEntry::TurnMetrics {
                        turn_id: metrics.turn_id,
                        total_ms: metrics.total_ms,
                        user_agent_latency_ms: metrics.user_agent_latency_ms,
                        timestamp: utc_now_iso8601(),
                    });
                }

                Event::SessionEnded => {
                    transcript.push(TranscriptEntry::SessionEvent {
                        kind: "ended".to_string(),
                        timestamp: utc_now_iso8601(),
                    });
                    break;
                }

                Event::TurnStarted { turn_number } => {
                    // Unmute the agent when a new turn officially starts.
                    // This lifts the barge-in guard, allowing new TTS audio.
                    agent_muted = false;
                    transcript.push(TranscriptEntry::SessionEvent {
                        kind: format!("turn_started ({})", turn_number),
                        timestamp: utc_now_iso8601(),
                    });
                }

                Event::Interrupt => {
                    // Barge-in: the reactor cancelled TTS mid-stream.
                    // Let the agent channel be muted until the next TurnStarted.
                    agent_muted = true;
                    agent_resamplers.clear();

                    // Because TTS generates audio much faster than real-time, we likely
                    // appended chunks into tl_agent far future of the actual wall clock.
                    // To accurately reflect what the user heard, we truncate the agent
                    // timeline to the current elapsed wall-clock time (plus 200ms grace
                    // for network buffering).
                    let elapsed_secs = session_start.elapsed().as_secs_f64();
                    let cutoff_samples = ((elapsed_secs + 0.200) * options.sample_rate as f64).round() as usize;

                    if cutoff_samples < tl_agent.len() {
                        info!(
                            "[recording] Barge-in cutoff: truncating tl_agent from {} to {}",
                            tl_agent.len(),
                            cutoff_samples
                        );
                        tl_agent.truncate(cutoff_samples);
                        
                        // We also pull the write cursor back. If the user hangs up
                        // without starting a new turn, we won't accidentally skip ahead.
                        agent_write_cursor = cutoff_samples;
                    } else {
                        info!("[recording] Barge-in at agent sample {} — agent muted", agent_write_cursor);
                    }
                }

                _ => {}
            },
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!("[recording] Lagged behind by {n} events — some audio may be missing");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break;
            }
        }
    }

    let ended_at = utc_now_iso8601();
    let duration_secs = tl_user.len().max(tl_agent.len()) as f64 / options.sample_rate as f64;
    let session_id = session_id.to_string();
    let options = options.clone();
    let started_at = started_at.to_string();

    tokio::task::spawn_blocking(move || {
        encode_recording(
            &session_id,
            &options,
            &tl_user,
            &tl_agent,
            transcript,
            &started_at,
            &ended_at,
            duration_secs,
        )
    })
    .await
    .ok()
    .flatten()
}

/// Write PCM samples into `timeline` starting at `start`, growing as needed.
///
/// Chunks that overlap an existing position are mixed (add + clamp to i16).
/// Safety: the timeline is capped at ~2 hours of 48 kHz audio (~345 M samples)
/// to prevent unbounded allocation from wall-clock spikes (debugger pauses,
/// system sleep, etc.).
fn place_samples(timeline: &mut Vec<i16>, samples: &[i16], start: usize) {
    // ~2 hours at 48 kHz — generous ceiling that prevents OOM.
    const MAX_TIMELINE_SAMPLES: usize = 48_000 * 60 * 120;
    // Max allowable forward jump at once (5 minutes at 48kHz = 14.4M samples)
    const MAX_GAP_SAMPLES: usize = 48_000 * 60 * 5;

    let mut actual_start = start;
    if actual_start > timeline.len() + MAX_GAP_SAMPLES {
        warn!(
            "[recording] Huge offset jump detected ({} > end + {} gap). Capping to prevent zero-fill allocation.",
            actual_start, MAX_GAP_SAMPLES
        );
        actual_start = timeline.len() + MAX_GAP_SAMPLES;
    }

    let end = actual_start + samples.len();
    if end > MAX_TIMELINE_SAMPLES {
        // Clock likely jumped — don't allocate a huge buffer.
        warn!(
            "[recording] Timeline cap exceeded (offset {} + {} samples > {} max) — audio dropped",
            actual_start, samples.len(), MAX_TIMELINE_SAMPLES
        );
        return;
    }
    if timeline.len() < end {
        timeline.resize(end, 0);
    }
    for (i, &s) in samples.iter().enumerate() {
        let mixed = timeline[actual_start + i] as i32 + s as i32;
        timeline[actual_start + i] = mixed.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
}

/// Resolves whether Opus encoding should be used at runtime.
fn resolve_use_opus(format: AudioFormat) -> bool {
    #[cfg(feature = "opus")]
    { return format == AudioFormat::Opus; }
    #[cfg(not(feature = "opus"))]
    {
        if format == AudioFormat::Opus {
            warn!("[recording] Opus format requested but `opus` feature not enabled — using WAV");
        }
        false
    }
}

/// Encode PCM timelines into a [`RecordingOutput`] — **no file I/O**.
///
/// Callers receive raw bytes and decide where to write them (disk, S3, …).
/// Returns `None` if both buffers are empty.
fn encode_recording(
    session_id: &str,
    options: &RecordingConfig,
    user_pcm: &[i16],
    agent_pcm: &[i16],
    transcript: Vec<TranscriptEntry>,
    started_at: &str,
    ended_at: &str,
    duration_secs: f64,
) -> Option<RecordingOutput> {
    // ── Audio encoding ────────────────────────────────────────────
    if user_pcm.is_empty() && agent_pcm.is_empty() {
        info!("[recording] No audio captured — skipping audio for {}", session_id);
        return None;
    }

    let is_mono = options.audio_layout == AudioLayout::Mono;
    let use_opus = resolve_use_opus(options.audio_format);
    let num_channels: u16 = if is_mono { 1 } else { 2 };

    #[cfg_attr(not(feature = "opus"), allow(unused_variables))]
    let (audio_bytes, audio_extension): (Vec<u8>, &'static str) = if use_opus {
        #[cfg(feature = "opus")]
        {
            let result = if is_mono {
                let mixed = mix_mono(user_pcm, agent_pcm);
                opus_encode::encode_mono(&mixed, options.sample_rate)
            } else {
                opus_encode::encode_stereo(user_pcm, agent_pcm, options.sample_rate)
            };
            match result {
                Ok(bytes) => (bytes, "opus"),
                Err(e) => {
                    warn!("[recording] Opus encoding failed: {} — falling back to WAV", e);
                    let wav = if is_mono {
                        write_wav(&mix_mono(user_pcm, agent_pcm), options.sample_rate, 1)
                    } else {
                        write_wav(&interleave_stereo(user_pcm, agent_pcm), options.sample_rate, 2)
                    };
                    (wav, "wav")
                }
            }
        }
        #[cfg(not(feature = "opus"))]
        { unreachable!("resolve_use_opus must return false without the `opus` feature") }
    } else {
        let wav = if is_mono {
            write_wav(&mix_mono(user_pcm, agent_pcm), options.sample_rate, 1)
        } else {
            write_wav(&interleave_stereo(user_pcm, agent_pcm), options.sample_rate, 2)
        };
        (wav, "wav")
    };

    info!(
        "[recording] Encoded {:.1}s {}ch {} ({} bytes)",
        duration_secs, num_channels, audio_extension.to_uppercase(), audio_bytes.len(),
    );

    // ── Transcript JSON ───────────────────────────────────────────
    let transcript_json: Option<Vec<u8>> = if options.save_transcript {
        let doc = SessionTranscript {
            session_id: session_id.to_string(),
            started_at: started_at.to_string(),
            ended_at: ended_at.to_string(),
            duration_secs,
            entries: transcript,
        };
        match serde_json::to_vec_pretty(&doc) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warn!("[recording] Failed to serialise transcript: {}", e);
                None
            }
        }
    } else {
        None
    };

    Some(RecordingOutput {
        session_id: session_id.to_string(),
        audio_extension,
        channels: num_channels,
        duration_secs,
        transcript_json,
        // audio_bytes are held here temporarily — persist_recording() drains them
        // when writing to disk/S3, then drops the buffer.
        audio_bytes,
        // storage_uri is filled in by persist_recording() after encode_recording() returns.
        storage_uri: String::new(),
    })
}

// ── Time helpers ────────────────────────────────────────────────

/// Current wall-clock time as ISO-8601 string: `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC).
fn utc_now_iso8601() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_correctness() {
        let samples = vec![0i16; 1600]; // 0.1s at 16kHz
        let wav = write_wav(&samples, 16000, 1);

        // RIFF header
        assert_eq!(&wav[0..4], b"RIFF");
        let file_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
        assert_eq!(file_size, 36 + 3200); // 36 + data_size

        // WAVE marker
        assert_eq!(&wav[8..12], b"WAVE");

        // fmt sub-chunk
        assert_eq!(&wav[12..16], b"fmt ");
        let fmt_size = u32::from_le_bytes([wav[16], wav[17], wav[18], wav[19]]);
        assert_eq!(fmt_size, 16);
        let format = u16::from_le_bytes([wav[20], wav[21]]);
        assert_eq!(format, 1); // PCM
        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(channels, 1);
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16000);

        // data sub-chunk
        assert_eq!(&wav[36..40], b"data");
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_size, 3200); // 1600 samples × 2 bytes

        // Total file size
        assert_eq!(wav.len(), 44 + 3200);
    }

    #[test]
    fn wav_stereo_header() {
        let samples = vec![0i16; 3200]; // 0.1s stereo at 16kHz (1600 L + 1600 R)
        let wav = write_wav(&samples, 16000, 2);

        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(channels, 2);

        let byte_rate = u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]);
        assert_eq!(byte_rate, 64000); // 16000 × 2 × 2

        let block_align = u16::from_le_bytes([wav[32], wav[33]]);
        assert_eq!(block_align, 4); // 2 channels × 2 bytes
    }

    #[test]
    fn interleave_equal_length() {
        let user = vec![1, 2, 3];
        let agent = vec![10, 20, 30];
        let stereo = interleave_stereo(&user, &agent);
        assert_eq!(stereo, vec![1, 10, 2, 20, 3, 30]);
    }

    #[test]
    fn interleave_user_longer() {
        let user = vec![1, 2, 3, 4];
        let agent = vec![10, 20];
        let stereo = interleave_stereo(&user, &agent);
        assert_eq!(stereo, vec![1, 10, 2, 20, 3, 0, 4, 0]);
    }

    #[test]
    fn interleave_agent_longer() {
        let user = vec![1];
        let agent = vec![10, 20, 30];
        let stereo = interleave_stereo(&user, &agent);
        assert_eq!(stereo, vec![1, 10, 0, 20, 0, 30]);
    }

    #[test]
    fn mix_mono_equal_length() {
        let user = vec![100, 200, -100];
        let agent = vec![100, -200, 100];
        let mono = mix_mono(&user, &agent);
        assert_eq!(mono, vec![100, 0, 0]);
    }

    #[test]
    fn mix_mono_clamp() {
        let user = vec![i16::MAX];
        let agent = vec![i16::MAX];
        let mono = mix_mono(&user, &agent);
        // (32767 + 32767) / 2 = 32767 — no overflow
        assert_eq!(mono[0], i16::MAX);
    }


    #[test]
    fn bytes_to_i16_roundtrip() {
        let samples = vec![0i16, 1, -1, i16::MAX, i16::MIN];
        let mut bytes = Vec::new();
        for s in &samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let decoded = bytes_to_i16(&bytes);
        assert_eq!(decoded, samples);
    }

    #[test]
    fn utc_now_iso8601_format() {
        let ts = utc_now_iso8601();
        // Should match: YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], ".");
        assert_eq!(&ts[23..24], "Z");
    }

    #[test]
    fn transcript_entry_serializes() {
        let entry = TranscriptEntry::Message {
            role: "user".to_string(),
            text: "Hello".to_string(),
            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            was_interrupted: false,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"message\""));
        assert!(json.contains("\"role\":\"user\""));
    }

    #[test]
    fn session_transcript_roundtrip() {
        let doc = SessionTranscript {
            session_id: "test-123".to_string(),
            started_at: "2026-01-01T00:00:00.000Z".to_string(),
            ended_at: "2026-01-01T00:01:00.000Z".to_string(),
            duration_secs: 60.0,
            entries: vec![
                TranscriptEntry::SessionEvent {
                    kind: "started".to_string(),
                    timestamp: "2026-01-01T00:00:00.000Z".to_string(),
                },
                TranscriptEntry::Message {
                    role: "user".to_string(),
                    text: "Hello".to_string(),
                    timestamp: "2026-01-01T00:00:05.000Z".to_string(),
                    was_interrupted: false,
                },
                TranscriptEntry::ToolCall {
                    tool_name: "get_weather".to_string(),
                    status: "completed".to_string(),
                    timestamp: "2026-01-01T00:00:06.000Z".to_string(),
                },
                TranscriptEntry::SessionEvent {
                    kind: "ended".to_string(),
                    timestamp: "2026-01-01T00:01:00.000Z".to_string(),
                },
            ],
        };

        let json = serde_json::to_string_pretty(&doc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["session_id"], "test-123");
        assert_eq!(parsed["entries"].as_array().unwrap().len(), 4);
        assert_eq!(parsed["entries"][0]["type"], "session_event");
        assert_eq!(parsed["entries"][1]["type"], "message");
        assert_eq!(parsed["entries"][2]["type"], "tool_call");
    }

    #[cfg(feature = "opus")]
    #[test]
    fn opus_mono_encode() {
        // 1 second of 440 Hz sine wave at 48 kHz (Opus native rate)
        let sample_rate = 48000u32;
        let samples: Vec<i16> = (0..sample_rate as usize)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (f64::sin(2.0 * std::f64::consts::PI * 440.0 * t) * 16000.0) as i16
            })
            .collect();

        let opus = opus_encode::encode_mono(&samples, sample_rate).expect("Opus mono encode");

        // Opus should be much smaller than WAV (48000 samples × 2 bytes = 96000 bytes)
        assert!(opus.len() > 100, "Opus too small: {} bytes", opus.len());
        assert!(opus.len() < 96000, "Opus should be smaller than WAV: {} bytes", opus.len());

        // OGG files start with "OggS" magic
        assert_eq!(&opus[..4], b"OggS", "Invalid OGG header");
    }

    #[cfg(feature = "opus")]
    #[test]
    fn opus_stereo_encode() {
        let sample_rate = 48000u32;
        let left: Vec<i16> = (0..sample_rate as usize)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (f64::sin(2.0 * std::f64::consts::PI * 440.0 * t) * 16000.0) as i16
            })
            .collect();
        let right: Vec<i16> = (0..sample_rate as usize)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (f64::sin(2.0 * std::f64::consts::PI * 880.0 * t) * 16000.0) as i16
            })
            .collect();

        let opus = opus_encode::encode_stereo(&left, &right, sample_rate)
            .expect("Opus stereo encode");

        assert!(opus.len() > 100, "Opus too small: {} bytes", opus.len());
        assert!(opus.len() < 192000, "Opus unexpectedly large: {} bytes", opus.len());
    }

    #[cfg(feature = "opus")]
    #[test]
    fn opus_stereo_unequal_lengths() {
        let sample_rate = 48000u32;
        let left: Vec<i16> = vec![1000; 48000]; // 1 second
        let right: Vec<i16> = vec![1000; 24000]; // 0.5 seconds (shorter)

        let opus = opus_encode::encode_stereo(&left, &right, sample_rate)
            .expect("Opus unequal encode");
        assert!(opus.len() > 100, "Opus too small: {} bytes", opus.len());
    }
}
