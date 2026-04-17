//! Shared utilities — audio ring buffer, playback tracking, PCM constants, WAV helpers.

use std::time::{Duration, Instant};

/// Standard internal audio format: 16kHz mono PCM16.
pub const SAMPLE_RATE: u32 = 16_000;
/// TTS output sample rate.
pub const TTS_SAMPLE_RATE: u32 = 24_000;
/// WebRTC Opus clock rate.
pub const WEBRTC_RATE: u32 = 48_000;
/// 16-bit PCM = 2 bytes per sample.
pub const SAMPLE_WIDTH: usize = 2;
/// Silero VAD requires chunks of at least 512 samples at 16kHz.
pub const FRAME_SIZE: usize = 512;
/// Bytes per frame (512 samples × 2 bytes).
pub const FRAME_BYTES: usize = FRAME_SIZE * SAMPLE_WIDTH;

/// Generate 20ms of 440Hz transfer hold tone (mono PCM).
///
/// Updates the provided `phase` to maintain wave continuity across frames.
pub fn generate_transfer_hold_tone(phase: &mut f32, sample_rate: u32) -> Vec<u8> {
    let samples_per_frame = (sample_rate / 50) as usize; // 20ms
    let hold_freq = 440.0_f32; // A4 tone
    std::iter::from_fn(|| {
        let sample = (*phase * std::f32::consts::TAU).sin() * 4000.0;
        *phase = (*phase + hold_freq / sample_rate as f32).fract();
        Some((sample as i16).to_le_bytes())
    })
    .take(samples_per_frame)
    .flatten()
    .collect()
}

// ── WAV helpers ───────────────────────────────────────────────────────────────

/// Wrap raw PCM-16 LE mono 16 kHz bytes in a minimal 44-byte WAV container.
///
/// Used by the segmented HTTP STT providers (Groq Whisper, OpenAI Whisper,
/// ElevenLabs) to package the accumulated PCM buffer before POSTing it as a
/// multipart file.
pub fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let file_len = data_len + 36;

    let sample_rate = SAMPLE_RATE;
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;

    let mut wav = Vec::with_capacity(44 + pcm.len());

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);

    wav
}

/// Pre-allocated ring buffer for audio frame alignment.
///
/// Hot-path zero allocation after init:
/// - Fixed-size internal buffer (no growth after construction)
/// - [`process_frames`](AudioRingBuffer::process_frames) yields `&[u8]` slices
///   directly into the internal buffer — no `Vec<u8>` per frame
/// - [`add`](AudioRingBuffer::add) is provided for tests and non-hot callers
pub struct AudioRingBuffer {
    buf: Vec<u8>,
    write_pos: usize,
    frame_bytes: usize,
}

impl AudioRingBuffer {
    /// Create a new ring buffer.
    ///
    /// `capacity` defaults to 65536 bytes (~2 seconds at 16kHz PCM16).
    pub fn new(capacity: usize, frame_bytes: usize) -> Self {
        Self {
            buf: vec![0u8; capacity],
            write_pos: 0,
            frame_bytes,
        }
    }

    /// Add audio data and return complete frames.
    ///
    /// Allocates one `Vec<u8>` per frame. For the hot audio path, prefer
    /// [`process_frames`](Self::process_frames) which avoids all allocation.
    pub fn add(&mut self, audio: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        self.process_frames(audio, |frame| frames.push(frame.to_vec()));
        frames
    }

    /// Feed audio into the buffer and call `f` with each complete frame slice.
    ///
    /// **Zero allocation** — `f` receives a `&[u8]` pointing directly into the
    /// ring buffer's internal storage. No `Vec<u8>` is allocated per frame.
    ///
    /// # Constraints
    ///
    /// The slice passed to `f` is only valid for the duration of the call.
    /// If you need to retain audio across calls (e.g. for STT batching),
    /// copy it with `frame.to_vec()` inside `f`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ring_buffer.process_frames(&raw, |frame: &[u8]| {
    ///     denoiser.process_into(frame, &mut scratch);
    ///     vad.process(&scratch);
    /// });
    /// ```
    pub fn process_frames(&mut self, audio: &[u8], mut f: impl FnMut(&[u8])) {
        let n = audio.len();
        let end = self.write_pos + n;

        // Compact if we'd overflow
        if end > self.buf.len() {
            let remaining = self.write_pos;
            if remaining > 0 {
                self.buf.copy_within(..remaining, 0);
            }
            self.write_pos = remaining;
        }

        // Write incoming audio
        let end = self.write_pos + n;
        if end <= self.buf.len() {
            self.buf[self.write_pos..end].copy_from_slice(audio);
            self.write_pos = end;
        }

        // Yield complete frames as slices — zero allocation
        let mut read_pos = 0;
        while read_pos + self.frame_bytes <= self.write_pos {
            f(&self.buf[read_pos..read_pos + self.frame_bytes]);
            read_pos += self.frame_bytes;
        }

        // Compact remaining
        let remaining = self.write_pos - read_pos;
        if remaining > 0 && read_pos > 0 {
            self.buf.copy_within(read_pos..self.write_pos, 0);
        }
        self.write_pos = remaining;
    }

    /// Discard all buffered audio.
    pub fn clear(&mut self) {
        self.write_pos = 0;
    }
}

impl Default for AudioRingBuffer {
    fn default() -> Self {
        Self::new(65536, FRAME_BYTES)
    }
}

// ── Playback Tracker ────────────────────────────────────────────

/// Network/jitter buffer added to the estimated playback end.
const PLAYBACK_BUFFER: Duration = Duration::from_millis(2000);

/// Estimates when the client will finish playing streamed TTS audio.
///
/// TTS chunks are sent over time. The client starts playing chunk 1
/// while chunk 2 is still being synthesized. Remaining playback:
///
///   earliest_done = max(first_chunk_ts + total_audio_duration, last_chunk_ts)
///   estimated_end = earliest_done + buffer
///   remaining     = estimated_end - now
///
/// Using `max()` handles both fast TTS (all audio sent quickly, so
/// `first_chunk_ts + total_audio_duration` dominates) and slow TTS
/// (synthesis takes longer than the audio duration, so `last_chunk_ts`
/// dominates and we must wait for the final chunk to play out).
pub struct PlaybackTracker {
    sample_rate: u32,
    first_chunk_ts: Option<Instant>,
    last_chunk_ts: Option<Instant>,
    total_bytes: usize,
}

impl PlaybackTracker {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            first_chunk_ts: None,
            last_chunk_ts: None,
            total_bytes: 0,
        }
    }

    /// Record a PCM chunk being sent to the client.
    pub fn record(&mut self, pcm_bytes: usize) {
        let now = Instant::now();
        if self.first_chunk_ts.is_none() {
            self.first_chunk_ts = Some(now);
        }
        self.last_chunk_ts = Some(now);
        self.total_bytes += pcm_bytes;
    }

    /// Total PCM bytes sent this turn.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Total audio duration from all PCM sent this turn (mono PCM16).
    fn total_audio_duration(&self) -> Duration {
        let bytes_per_sec = self.sample_rate as usize * 2;
        if bytes_per_sec == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.total_bytes as f64 / bytes_per_sec as f64)
    }

    /// Estimated remaining playback time on the client.
    ///
    /// Returns `Duration::ZERO` if playback should already be finished
    /// (or no audio was sent).
    pub fn remaining_playback(&self) -> Duration {
        let Some(first_ts) = self.first_chunk_ts else {
            return Duration::ZERO;
        };
        let last_ts = self.last_chunk_ts.unwrap_or(first_ts);

        // The client can't finish before *both*:
        //   (a) enough wall-clock time has passed to play all audio
        //       (first_chunk_ts + total_audio_duration), AND
        //   (b) the last chunk has at least arrived (last_chunk_ts).
        let by_audio = first_ts + self.total_audio_duration();
        let earliest_done = by_audio.max(last_ts);
        let estimated_end = earliest_done + PLAYBACK_BUFFER;

        estimated_end
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
    }

    /// Reset for a new turn.
    pub fn reset(&mut self) {
        self.first_chunk_ts = None;
        self.last_chunk_ts = None;
        self.total_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_basic() {
        let mut rb = AudioRingBuffer::new(4096, 4);
        let frames = rb.add(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![1, 2, 3, 4]);
    }

    #[test]
    fn process_frames_yields_same_as_add() {
        // process_frames() must produce the same bytes as add(), just without
        // the intermediate allocation.
        let data: Vec<u8> = (0u8..=127).collect(); // 128 bytes
        let frame_size = 16;

        let mut rb_alloc = AudioRingBuffer::new(4096, frame_size);
        let expected = rb_alloc.add(&data);

        let mut rb_zero = AudioRingBuffer::new(4096, frame_size);
        let mut got: Vec<Vec<u8>> = Vec::new();
        rb_zero.process_frames(&data, |frame| got.push(frame.to_vec()));

        assert_eq!(
            got, expected,
            "process_frames must yield identical frames to add()"
        );
    }

    #[test]
    fn process_frames_partial_accumulates_across_calls() {
        // Send 3 bytes at a time with frame_size=4. Frame should only fire
        // after the 2nd call (6 bytes total >= 4).
        let mut rb = AudioRingBuffer::new(4096, 4);
        let mut count = 0usize;
        rb.process_frames(&[1, 2, 3], |_| count += 1);
        assert_eq!(count, 0, "no complete frame yet");
        rb.process_frames(&[4, 5, 6], |_| count += 1);
        assert_eq!(count, 1, "exactly one complete frame now");
    }

    #[test]
    fn process_frames_multi_batch() {
        // 1024-byte chunk → 1 frame at 1024-byte frame size, 2 frames at 512.
        let data = vec![0u8; 1024];
        let mut rb512 = AudioRingBuffer::new(65536, 512);
        let mut count = 0usize;
        rb512.process_frames(&data, |_| count += 1);
        assert_eq!(count, 2);

        let mut rb1024 = AudioRingBuffer::new(65536, 1024);
        let mut count = 0usize;
        rb1024.process_frames(&data, |_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn playback_no_audio_returns_zero() {
        let t = PlaybackTracker::new(24000);
        assert_eq!(t.remaining_playback(), Duration::ZERO);
    }

    #[test]
    fn playback_remaining_decreases_over_time() {
        let mut t = PlaybackTracker::new(24000);
        // 2 seconds of audio at 24kHz mono PCM16 = 96000 bytes
        t.record(96000);
        let r1 = t.remaining_playback();
        std::thread::sleep(Duration::from_millis(100));
        let r2 = t.remaining_playback();
        assert!(r2 < r1);
    }

    #[test]
    fn playback_expired_returns_zero() {
        let mut t = PlaybackTracker::new(24000);
        t.record(480); // 10ms of audio
        std::thread::sleep(Duration::from_millis(2100));
        assert_eq!(t.remaining_playback(), Duration::ZERO);
    }

    #[test]
    fn playback_reset_clears_state() {
        let mut t = PlaybackTracker::new(24000);
        t.record(48000);
        t.reset();
        assert_eq!(t.remaining_playback(), Duration::ZERO);
    }

    /// Simulate slow TTS: synthesis time exceeds audio duration.
    ///
    /// Send 10ms of audio, wait 600ms (well past its 10ms + 500ms buffer),
    /// then send another 10ms chunk. The old formula (first_ts + total_duration)
    /// would return zero because 20ms of audio starting 600ms ago is long
    /// "done". The corrected formula uses `max(first_ts + duration, last_ts)`
    /// so `last_ts` dominates and we still report ~500ms remaining.
    #[test]
    fn playback_slow_tts_uses_last_chunk() {
        let mut t = PlaybackTracker::new(24000);
        // 10ms of audio at 24kHz PCM16 = 480 bytes
        t.record(480);
        // Simulate slow synthesis: wait long enough that first_ts + 20ms + 500ms
        // buffer has expired.
        std::thread::sleep(Duration::from_millis(600));
        // Second chunk arrives now — client still needs to play it
        t.record(480);

        let remaining = t.remaining_playback();
        // last_chunk_ts is ~now, so remaining should be ≈ PLAYBACK_BUFFER (2000ms).
        // With the old formula this would be Duration::ZERO.
        assert!(
            remaining >= Duration::from_millis(1900),
            "expected ≥1900ms remaining for just-sent chunk, got {:?}",
            remaining
        );
    }
}
