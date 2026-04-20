//! Per-turn latency metrics and turn tracker.
//!
//! The [`TurnTracker`] records timestamps at key pipeline stages and computes
//! per-turn latency metrics (TTFA, STT latency, etc.) when the turn completes.

use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{info, warn};

// ── Turn Metrics ────────────────────────────────────────────────

/// Per-turn latency breakdown (all values in milliseconds).
#[derive(Debug, Clone, Serialize)]
pub struct TurnMetrics {
    /// Monotonic turn counter within this session.
    pub turn_id: u64,

    // ── Pipeline latency segments ──
    /// VAD silence window duration in ms.
    /// How long the VAD waited in silence before confirming speech ended.
    pub vad_silence_ms: f64,
    /// Time from VAD SpeechEnded → EOU decision (SmartTurn / timer).
    /// Zero when SmartTurn is disabled (instant decision).
    pub eou_delay_ms: f64,
    /// Time from EOU decision → STT Transcript.
    pub stt_ms: f64,
    /// Time from Transcript → first LLM token.
    pub llm_first_token_ms: f64,
    /// TTS TTFB: first text fed to TTS → first audio chunk received.
    pub tts_first_audio_ms: f64,

    // ── End-to-end aggregates ──
    /// Time from EOU decision → first TTS audio sent (end-to-end).
    pub ttfa_ms: f64,
    /// Time from EOU decision → TTS finished (full turn).
    pub total_ms: f64,
    /// Time from TTS start → TTS finished (synthesis time).
    pub tts_duration_ms: f64,
    /// User-perceived latency: vad_speech_ended → first TTS audio.
    /// Includes EOU delay. VAD timestamp is backdated by vad_silence_ms.
    pub user_agent_latency_ms: Option<f64>,

    // ── Audio durations ──
    /// Duration of the user's speech audio (input), in ms.
    pub input_audio_duration_ms: f64,
    /// Duration of the generated TTS audio (output), in ms.
    pub output_audio_duration_ms: f64,

    // ── Per-service metrics (single source of truth for Langfuse/OTel) ──
    /// Total STT duration (SpeechStarted → STT Transcript).
    pub stt_total_duration_ms: f64,
    /// STT TTFB (finalize_sent → first text received).
    pub stt_ttfb_ms: Option<f64>,
    /// TTS TTFB (first text fed to TTS → first audio chunk received).
    pub tts_ttfb_ms: Option<f64>,
    /// Total TTS duration (first text fed → TTS finished).
    pub tts_total_duration_ms: f64,
    /// Time from first LLM token → first text fed to TTS (ms).
    /// Captures turn-completion buffering / sentence aggregation delay.
    pub text_aggregation_ms: Option<f64>,
}

// ── Turn Tracker ────────────────────────────────────────────────

/// Timing anchors for user input (VAD/STT).
#[derive(Default, Clone, Copy)]
struct InteractionAnchors {
    vad_speech_ended: Option<Instant>,
    stt_audio_start: Option<Instant>,
    stt_finalize_sent: Option<Instant>,
    stt_first_text: Option<Instant>,
    transcript: Option<Instant>,
    input_audio_duration_ms: f64,
}

/// Timing anchors for the turn pipeline (LLM/TTS).
#[derive(Default, Clone, Copy)]
struct PipelineAnchors {
    speech_ended: Option<Instant>,
    llm_first_token: Option<Instant>,
    tts_start: Option<Instant>,
    tts_text_fed: Option<Instant>,
    tts_first_audio: Option<Instant>,
    tts_finished: Option<Instant>,
    output_audio_duration_ms: f64,
}

/// Tracks timestamps at key points within a single user turn.
///
/// Decouples "pending input" (VAD/STT anchors) from the "active turn"
/// (LLM/TTS pipeline). This ensures that a barge-in's early STT metrics
/// aren't wiped when the previous turn is cancelled.
#[derive(Default)]
pub struct TurnTracker {
    vad_silence_ms: f64,
    /// User input being tracked (before/during current turn).
    input: InteractionAnchors,
    /// User input that was "committed" to the current active turn.
    committed_input: InteractionAnchors,
    /// Pipeline activity for the current active turn.
    pipeline: PipelineAnchors,
}

impl TurnTracker {
    /// Create a new turn tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the VAD silence window (ms) — called once at session startup
    /// after VAD config is computed. Used to backdate `vad_speech_ended`.
    pub fn set_vad_silence_ms(&mut self, ms: f64) {
        self.vad_silence_ms = ms;
    }

    /// Get the VAD silence window (ms).
    pub fn vad_silence_ms(&self) -> f64 {
        self.vad_silence_ms
    }

    /// VAD detected end of speech. Records the backdated VAD timestamp.
    ///
    /// Called immediately when `VadEvent::SpeechEnded` fires, before
    /// SmartTurn or any other decision logic.
    ///
    /// The timestamp is backdated by `vad_silence_ms` (set via
    /// `set_vad_silence_ms`) so the measurement starts from when
    /// the user actually stopped making sound.
    pub fn mark_vad_speech_ended(&mut self) {
        let now = Instant::now();
        let backdate = Duration::from_secs_f64(self.vad_silence_ms / 1000.0);
        self.input.vad_speech_ended = Some(now.checked_sub(backdate).unwrap_or(now));
    }

    /// Set the duration of the user's input speech audio (in ms).
    pub fn set_input_audio_duration(&mut self, ms: f64) {
        self.input.input_audio_duration_ms = ms;
    }

    /// EOU decision committed — anchors the start of the user→agent latency clock.
    ///
    /// First-write-wins: safe to call from multiple paths (EotFallback fires
    /// and the subsequent transcript both call this; the first one wins).
    pub fn mark_speech_ended(&mut self) {
        if self.pipeline.speech_ended.is_none() {
            self.pipeline.speech_ended = Some(Instant::now());
        }
    }

    /// STT streaming started (user speaking).
    pub fn mark_stt_audio_start(&mut self) {
        if self.input.stt_audio_start.is_none() {
            self.input.stt_audio_start = Some(Instant::now());
        }
    }

    /// STT finalize signal sent to provider.
    pub fn mark_stt_finalize_sent(&mut self) {
        self.input.stt_finalize_sent = Some(Instant::now());
    }

    /// First partial text received from STT.
    pub fn mark_stt_first_text(&mut self) {
        if self.input.stt_first_text.is_none() {
            self.input.stt_first_text = Some(Instant::now());
        }
    }

    /// STT returned the final transcript.
    pub fn mark_transcript(&mut self) {
        self.input.transcript = Some(Instant::now());
    }

    /// First LLM token received.
    pub fn mark_llm_first_token(&mut self) {
        if self.pipeline.llm_first_token.is_none() {
            self.pipeline.llm_first_token = Some(Instant::now());
        }
    }

    /// TTS synthesis started (session init).
    pub fn mark_tts_start(&mut self) {
        if self.pipeline.tts_start.is_none() {
            self.pipeline.tts_start = Some(Instant::now());
        }
    }

    /// First block of text fed to TTS synthesis.
    pub fn mark_tts_text_fed(&mut self) {
        if self.pipeline.tts_text_fed.is_none() {
            self.pipeline.tts_text_fed = Some(Instant::now());
        }
    }

    /// Whether `mark_tts_text_fed` was called this turn.
    pub fn has_tts_text_fed(&self) -> bool {
        self.pipeline.tts_text_fed.is_some()
    }

    /// First TTS audio chunk sent to client.
    pub fn mark_tts_first_audio(&mut self) {
        if self.pipeline.tts_first_audio.is_none() {
            self.pipeline.tts_first_audio = Some(Instant::now());
        }
    }

    /// TTS synthesis finished (last audio chunk sent).
    pub fn mark_tts_finished(&mut self) {
        self.pipeline.tts_finished = Some(Instant::now());
    }

    /// Compute TTS-specific metrics from the current timestamps.
    ///
    /// Returns `(tts_total_duration_ms, tts_ttfb_ms, text_aggregation_ms)`.
    /// Uses `tts_finished` as the end anchor (falls back to `Instant::now()`).
    pub fn tts_metrics(&self) -> (f64, Option<f64>, Option<f64>) {
        let end = self.pipeline.tts_finished.unwrap_or_else(|| {
            warn!("[turn_metrics] tts_finished anchor missing, falling back to Instant::now()");
            Instant::now()
        });
        let tts_total = self.pipeline.tts_text_fed
            .map(|s| ms_between(s, end))
            .unwrap_or(0.0);
        let tts_ttfb = self.pipeline.tts_text_fed
            .zip(self.pipeline.tts_first_audio)
            .map(|(s, a)| ms_between(s, a));
        let text_agg = self.pipeline.llm_first_token
            .zip(self.pipeline.tts_text_fed)
            .map(|(l, t)| ms_between(l, t));
        (tts_total, tts_ttfb, text_agg)
    }

    /// Compute STT-specific metrics from the current timestamps.
    ///
    /// Returns `(stt_total_duration_ms, stt_ttfb_ms)`.
    /// Uses committed input if available, otherwise current input.
    pub fn stt_metrics(&self) -> (f64, Option<f64>) {
        let input = if self.committed_input.transcript.is_some() {
            &self.committed_input
        } else {
            &self.input
        };

        let end = input.transcript.unwrap_or_else(|| {
            warn!("[turn_metrics] transcript anchor missing, falling back to Instant::now()");
            Instant::now()
        });
        let stt_total = input.stt_audio_start
            .map(|s| ms_between(s, end))
            .unwrap_or(0.0);
        // TTFB anchor selection:
        //   Batch STT  (e.g. Whisper): HTTP request fires at finalize(), so
        //              `finalize_sent → first_text` is the true API latency.
        //   Streaming STT (e.g. Deepgram): text may arrive before finalize() is
        //              sent while the user is still speaking; in that case
        //              `audio_start → first_text` captures time-to-first-word,
        //              which is the meaningful equivalent metric.
        //   Fallback: if neither anchor is available, the metric is omitted
        //             rather than silently emitting 0.
        let stt_ttfb = input.stt_first_text.and_then(|f| {
            let anchor = match input.stt_finalize_sent {
                Some(finalize) if f >= finalize => Some(finalize), // batch: finalize → first_text
                _ => input.stt_audio_start,                        // streaming: audio_start → first_text
            };
            anchor.map(|s| ms_between(s, f))
        });
        (stt_total, stt_ttfb)
    }

    /// Snapshot the current input anchors into `committed_input` and clear
    /// `input` so the tracker is ready for the next potential interruption.
    ///
    /// Called by `start_turn()` to freeze the VAD/STT timestamps that belong
    /// to this turn before any new barge-in can overwrite them.
    pub fn commit_input_anchors(&mut self) {
        self.committed_input = self.input;
        self.reset_input();
    }

    /// Set the duration of the generated TTS audio (in ms).
    pub fn set_output_audio_duration(&mut self, ms: f64) {
        self.pipeline.output_audio_duration_ms = ms;
    }

    /// Turn complete (TTS finished). Computes metrics and logs them.
    ///
    /// `turn_id` is the 1-based turn counter owned by the [`Tracer`].
    /// Returns `None` if the turn was never started (e.g. cancelled).
    pub fn finish(&mut self, turn_id: u64) -> Option<TurnMetrics> {
        let start = self.pipeline.speech_ended?;
        let now = Instant::now();
        let input = if self.committed_input.transcript.is_some() {
            &self.committed_input
        } else {
            &self.input
        };

        let eou_delay_ms = input
            .vad_speech_ended
            .map(|v| ms_between(v, start))
            .unwrap_or(0.0);
        let stt_ms = input.transcript.map(|t| ms_between(start, t)).unwrap_or(0.0);
        let llm_first_token_ms = input
            .transcript
            .zip(self.pipeline.llm_first_token)
            .map(|(t, l)| ms_between(t, l))
            .unwrap_or(0.0);
        let tts_first_audio_ms = self
            .pipeline
            .tts_start
            .zip(self.pipeline.tts_first_audio)
            .map(|(s, a)| ms_between(s, a))
            .unwrap_or(0.0);
        let ttfa_ms = self
            .pipeline
            .tts_first_audio
            .map(|a| ms_between(start, a))
            .unwrap_or(0.0);
        let total_ms = ms_between(start, now);
        let tts_duration_ms = self.pipeline.tts_start.map(|t| ms_between(t, now)).unwrap_or(0.0);
        // User-perceived latency: vad_speech_ended → first TTS audio.
        let user_agent_latency_ms = input
            .vad_speech_ended
            .zip(self.pipeline.tts_first_audio)
            .map(|(v, a)| ms_between(v, a));

        // ── Per-service metrics (delegate to helpers for single source of truth) ──
        let (stt_total_duration_ms, stt_ttfb_ms) = self.stt_metrics();
        let (tts_total_duration_ms, tts_ttfb_ms, text_aggregation_ms) = self.tts_metrics();

        let metrics = TurnMetrics {
            turn_id,
            vad_silence_ms: self.vad_silence_ms,
            eou_delay_ms,
            stt_ms,
            llm_first_token_ms,
            tts_first_audio_ms,
            ttfa_ms,
            total_ms,
            tts_duration_ms,
            user_agent_latency_ms,
            input_audio_duration_ms: input.input_audio_duration_ms,
            output_audio_duration_ms: self.pipeline.output_audio_duration_ms,

            stt_total_duration_ms,
            stt_ttfb_ms,
            tts_ttfb_ms,
            tts_total_duration_ms,
            text_aggregation_ms,
        };

        info!(
            turn_id = metrics.turn_id,
            vad_silence_ms = format!("{:.1}", metrics.vad_silence_ms),
            eou_delay_ms = format!("{:.1}", metrics.eou_delay_ms),
            stt_ms = format!("{:.1}", metrics.stt_ms),
            llm_first_token_ms = format!("{:.1}", metrics.llm_first_token_ms),
            tts_ttfb_ms = format!("{:.1}", metrics.tts_ttfb_ms.unwrap_or(0.0)),
            ttfa_ms = format!("{:.1}", metrics.ttfa_ms),
            total_ms = format!("{:.1}", metrics.total_ms),
            tts_duration_ms = format!("{:.1}", metrics.tts_duration_ms),
            input_audio_ms = format!("{:.1}", metrics.input_audio_duration_ms),
            output_audio_ms = format!("{:.1}", metrics.output_audio_duration_ms),
            "[turn_metrics] Turn #{} | VAD {:.0}ms | EOU {:.0}ms \u{2192} STT {:.0}ms \u{2192} LLM {:.0}ms \u{2192} TTS_TTFB {:.0}ms | TTFA {:.0}ms | UAL {:.0}ms | Total {:.0}ms | in={:.1}s out={:.1}s",
            metrics.turn_id,
            metrics.vad_silence_ms,
            metrics.eou_delay_ms,
            metrics.stt_ms,
            metrics.llm_first_token_ms,
            metrics.tts_ttfb_ms.unwrap_or(0.0),
            metrics.ttfa_ms,
            metrics.user_agent_latency_ms.unwrap_or(0.0),
            metrics.total_ms,
            metrics.input_audio_duration_ms / 1000.0,
            metrics.output_audio_duration_ms / 1000.0,
        );

        // Reset pipeline state for next turn. Preserve `self.input` — the
        // user may already be speaking (SpeechStarted fired before TTS
        // finished) and input anchors like `stt_audio_start` must survive
        // until consumed by `commit_input_anchors()` in the next `start_turn()`.
        self.reset_pipeline();

        Some(metrics)
    }

    /// Reset ONLY the turn pipeline state (LLM/TTS).
    ///
    /// Called on barge-in to clear the old turn's output state while
    /// preserving the new turn's STT/VAD anchors.
    pub fn reset_pipeline(&mut self) {
        self.pipeline = PipelineAnchors::default();
        self.committed_input = InteractionAnchors::default();
    }

    /// Reset ONLY the user input state (VAD/STT).
    pub fn reset_input(&mut self) {
        self.input = InteractionAnchors::default();
    }

    /// Full reset — clears both pipeline (LLM/TTS) and input (VAD/STT) state.
    ///
    /// Use when the session ends or a hard reset is required. For a barge-in
    /// that only cancels the current agent output, prefer [`reset_pipeline`](Self::reset_pipeline)
    /// to preserve the incoming user's VAD/STT anchors.
    pub fn reset(&mut self) {
        self.reset_pipeline();
        self.reset_input();
    }
}

fn ms_between(a: Instant, b: Instant) -> f64 {
    b.duration_since(a).as_secs_f64() * 1000.0
}
