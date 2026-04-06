//! Reactor — the single-loop event processor for the voice pipeline.
//!
//! Replaces the Hub-and-Spoke actor model with a centralized, serialized
//! `tokio::select!` loop. All decisions are made in one place, in one task,
//! eliminating channel races, Lamport guards, and generation counters.
//!
//! # Architecture
//!
//! # Cancellation
//!
//! `cancel_pipeline()` is 4 lines:
//!
//! - `self.llm.cancel()` drops `Receiver<LlmEvent>` → SSE task exits on next send
//! - `self.tts.cancel()` drops the token channel → TTS batch task exits on recv
//! - All `TimerKey`s removed from `DelayQueue` → zero stale events can fire
//!
//! No cancel channels. No generation counters. Rust's ownership does the work.
//!
//! # Module layout
//!
//! | File               | Responsibility                                                   |
//! |--------------------|------------------------------------------------------------------|
//! | `mod.rs`           | Struct def, `new()`, `start()`, `run()`, helpers                 |
//! | `audio.rs`         | `on_audio`, `on_vad_event` (+ barge-in logic)                    |
//! | `session.rs`       | STT / LLM / TTS handlers, `cancel_pipeline`, `start_llm_turn`,   |
//! |                    | `SideEffectToolsGuard`                                           |
//! | `timers.rs`        | `on_timer`, `start_idle_timer`, `ReengagementTimer`              |
//! | `turn_phase.rs`    | Pure `TurnPhase` state machine (no I/O)                          |

mod audio;
pub mod hooks;
pub mod policies;
pub(crate) mod proc;
pub mod replay;
mod session;
pub mod testing;
mod timers;
mod turn_phase;

use timers::ReengagementTimer;
use turn_phase::{NextPhase, TurnAction, TurnEvent, TurnPhase};

use agent_kit::AgentBackend;
use bytes::Bytes;
use futures_util::StreamExt as _;

use tokio::sync::mpsc;
use tokio_util::time::DelayQueue;
use tracing::{info, warn};
use voice_trace::{Event, Tracer};
use voice_transport::TransportCommand;

use crate::audio_ml::vad::VadConfig;
use crate::providers::stt::{build_stt_provider, SttProviderConfig};
use crate::providers::tts::{build_tts_provider, TtsMode, TtsProviderConfig};
use crate::reactor::hooks::ReactorHook;
use crate::reactor::proc::tts::WsCmd;
use crate::session::SessionConfig;
use crate::types::{SessionState, TimerKey};
use crate::utils::{AudioRingBuffer, PlaybackTracker, SAMPLE_RATE};

use proc::denoiser::DenoiserStage;
use proc::llm::LlmStage;
use proc::smart_turn::SmartTurnStage;
use proc::stt::SttStage;
use proc::tts::TtsStage;
use proc::vad::VadStage;

// ── Session lifecycle phase ─────────────────────────────────────────────

/// Tracks the high-level session lifecycle.
///
/// Replaces two boolean fields:
/// - `had_first_interaction: bool`
/// - `greeting_in_progress: bool`
///
/// State diagram:
///   Nascent → GreetingPlaying  (WS greeting configured; plays non-awaited)
///   Nascent → Active            (no greeting; or HTTP greeting plays synchronously)
///   GreetingPlaying → Active    (TtsEvent::Finished while greeting was in progress)
///   Nascent / GreetingPlaying → Active  (first SpeechStarted from user)
#[derive(Default)]
pub(crate) enum SessionPhase {
    /// Session started; no greeting is playing and the user has not yet spoken.
    /// Idle timer is suppressed until the session advances to `Active`.
    #[default]
    Nascent,
    /// Opening greeting TTS is currently playing (WS path).
    /// On completion, transitions to `Active` and arms the post-greeting idle timer.
    GreetingPlaying,
    /// First user speech received (or greeting completed).
    /// Normal idle timer arming is permitted.
    Active,
}

impl SessionPhase {
    /// True if the idle timer may be armed (greeting done or user already spoke).
    pub(super) fn allows_idle_timer(&self) -> bool {
        matches!(self, SessionPhase::Active | SessionPhase::GreetingPlaying)
    }
}

// ── Transport Control ───────────────────────────────────────────

/// Thin wrapper around the transport command channel.
///
/// Encapsulates transport-layer interactions so the Reactor doesn't
/// depend directly on `mpsc::UnboundedSender<TransportCommand>`.
/// If more transport interactions are needed later, they can be added
/// here without changing the Reactor's public API.
pub(crate) struct TransportControl {
    tx: mpsc::UnboundedSender<TransportCommand>,
}

impl TransportControl {
    pub fn new(tx: mpsc::UnboundedSender<TransportCommand>) -> Self {
        Self { tx }
    }

    /// Signal the transport layer to close (e.g. telephony REST hangup).
    pub fn close(&self) {
        let _ = self.tx.send(TransportCommand::Close);
    }
}

// ── Agent Audio Cursor ────────────────────────────────────────────

/// Timestamps `Event::AgentAudio` chunks for accurate recording placement.
///
/// Two-phase protocol:
/// 1. `begin_turn()` — call once at the start of each TTS turn.
///    Snaps the cursor forward to the current wall-clock sample position,
///    encoding real inter-turn silence (LLM thinking, user speech, tool calls).
/// 2. `stamp(pcm_bytes)` — call once per chunk.
///    Returns the chunk's start offset and advances the cursor by the chunk's
///    sample count. Correct for any TTS synthesis speed.
pub(super) struct AgentAudioCursor {
    session_start: std::time::Instant,
    sample_rate: u32,
    cursor: u64,
    /// Set by `begin_turn()`; cleared by the first `stamp()` in that turn.
    /// When true, `stamp()` snaps the cursor to wall-clock before placing
    /// the chunk, encoding the full inter-turn silence (including LLM + TTS
    /// latency) rather than just the time up to LLM-stream start.
    pending_turn_snap: bool,
}

impl AgentAudioCursor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            session_start: std::time::Instant::now(),
            sample_rate,
            cursor: 0,
            pending_turn_snap: false,
        }
    }

    /// Mark the start of a new TTS turn.
    ///
    /// Does NOT snap the cursor immediately — the snap happens in `stamp()`
    /// when the first audio chunk actually arrives. This ensures the recorded
    /// gap reflects the full inter-turn silence: VAD + STT + LLM + TTS latency.
    pub fn begin_turn(&mut self) {
        self.pending_turn_snap = true;
    }

    /// Returns the start offset of this chunk and advances the cursor.
    ///
    /// On the first call after `begin_turn()`, snaps the cursor forward to
    /// wall-clock position (max of current cursor and elapsed), then places
    /// the chunk sequentially from there. Subsequent chunks in the same turn
    /// advance the cursor strictly by sample count — correct for any TTS speed.
    ///
    /// # Invariant
    ///
    /// `pcm_bytes` **must** be raw PCM16 at `self.sample_rate` (2 bytes per
    /// sample). The reactor satisfies this because TTS providers already output
    /// at `config.output_sample_rate` — no resampling occurs between synthesis
    /// and `Event::AgentAudio` emission.
    pub fn stamp(&mut self, pcm_bytes: usize) -> u64 {
        if self.pending_turn_snap {
            self.pending_turn_snap = false;
            let wc = self.elapsed_samples();
            if wc > self.cursor {
                self.cursor = wc;
            }
        }
        let offset = self.cursor;
        self.cursor += (pcm_bytes / 2) as u64;
        offset
    }

    /// Wall-clock elapsed time converted to samples at the configured rate.
    ///
    /// Uses integer nanosecond arithmetic to avoid float truncation error.
    fn elapsed_samples(&self) -> u64 {
        let nanos = self.session_start.elapsed().as_nanos() as u64;
        nanos * self.sample_rate as u64 / 1_000_000_000
    }
}

// ── Reactor ──────────────────────────────────────────────────────

pub struct Reactor {
    // ── Config ───────────────────────────────────────────────
    pub(super) config: SessionConfig,

    // ── Behavioral policies (injected at construction, default = current behavior) ──
    /// Word-count gate for accepting barge-in interruptions.
    pub(super) barge_in_policy: Box<dyn policies::BargeInPolicy>,

    // ── Observer plugins ──────────────────────────────────────────
    /// Side-effect and async behavior hooks.
    pub(super) hooks: Vec<Box<dyn ReactorHook>>,

    // ── Event recording ──────────────────────────────────────────
    /// Optional log of all `ReactorInput` events received this session.
    /// Disabled by default (zero-cost). Enable via `enable_recording()`.
    pub(super) replay_log: replay::ReplayLog,

    // ── Sync stages (called inline per audio frame) ───────────
    pub(super) denoiser: DenoiserStage,
    pub(super) vad: VadStage,
    pub(super) smart_turn: Option<SmartTurnStage>,

    // ── Input audio pipeline (resample + frame alignment) ────
    pub(super) resampler: soxr::SoxrStreamResampler,
    pub(super) ring_buffer: AudioRingBuffer,

    // ── Async stages (polled in select!) ─────────────────────
    pub(super) stt: SttStage,
    pub(super) llm: LlmStage,
    pub(super) tts: TtsStage,

    // ── Timer management: one DelayQueue, zero generation counters ──
    pub(super) timers: DelayQueue<TimerKey>,
    // All turn-lifecycle timer keys (SttTimeout, EotFallback) are owned
    // inside the active TurnPhase variant — no separate Option fields needed.
    /// Re-engagement state machine: owns TurnCompletion and UserIdle timers.
    /// Enforces the invariant that only one of them can be armed at a time,
    /// with TurnCompletion taking priority over UserIdle by construction.
    pub(in crate::reactor) reengagement: ReengagementTimer,

    // ── Turn coordinator (compile-time statechart) ────────────
    /// Unified turn lifecycle state. Replaces the former `SpeechState`,
    /// `SttCommitState`, and `eot_key` fields with a single enum that
    /// owns all state-specific data (timer keys, flags).
    ///
    /// **Ownership contract**: `dispatch_turn_event` is the sole *writer*.
    /// All other code (pipeline.rs guards, audio output permit checks) may
    /// *read* through the `pub(super)` visibility but MUST NOT mutate it.
    /// This ensures the state machine is the only thing that transitions
    /// `turn_phase` between phases, making timer key lifecycle trackable.
    pub(super) turn_phase: TurnPhase,

    // ── Side-effect protection window ─────────────────────────────────
    /// Encapsulates the side-effect tool protection window: in-flight count,
    /// tool ID map, watchdog timer key, and deferred transcript.
    /// Defined in `pipeline.rs`.
    pub(super) se_guard: session::SideEffectToolsGuard,

    // ── Session state ─────────────────────────────────────────
    pub(super) state: SessionState,

    // ── I/O channels ─────────────────────────────────────────
    /// Incoming raw audio from the transport layer.
    pub(super) audio_rx: mpsc::UnboundedReceiver<Bytes>,

    // ── Observability ────────────────────────────────────────
    /// Unified event bus + ledger + turn tracker.
    pub(super) tracer: Tracer,

    // ── Session lifecycle ─────────────────────────────────────
    /// High-level session phase. Replaces `had_first_interaction` and
    /// `greeting_in_progress` with a compile-time-safe 3-variant enum.
    pub(super) session_phase: SessionPhase,

    // ── LLM generation phase (LLM → TTS streaming state) ────────────────
    /// Tracks the current LLM output-to-TTS streaming state.
    /// Defined in `pipeline.rs`. Makes invalid marker-detection /
    /// suppression combinations impossible at compile time.
    pub(super) llm_turn: session::LlmTurnPhase,

    // ── Hang-up coordination ──────────────────────────────────────
    /// Unified hang-up phase. Replaces `hang_up_pending`, `hang_up_delay_key`,
    /// and `hook_hang_up_pending` with a single enum whose variants own
    /// the data they need. Defined in `pipeline.rs`.
    pub(super) hang_up: session::HangUpPhase,

    // ── TTS URL (stored to avoid env lookup in the hot loop) ──────
    /// Config used to build a fresh TtsProvider for each synthesis session.
    pub(super) tts_provider_config: TtsProviderConfig,

    // ── Session-level WS TTS connection ──────────────────────────
    /// Sends commands to the session-scoped WS TTS command task.
    /// `None` when using an HTTP-only provider.
    pub(super) ws_tts_cmd_tx: Option<mpsc::UnboundedSender<WsCmd>>,
    /// Persistent audio receiver from the WS TTS provider.
    /// Owned exclusively by the Reactor — read directly in the select! arm,
    /// so no mutex or relay task is needed.
    pub(super) ws_tts_audio_rx: Option<mpsc::Receiver<crate::providers::tts::TtsAudioChunk>>,

    // ── Playback tracking ─────────────────────────────────────
    /// True after we've sent TTS audio to the client this turn.
    /// Cleared on barge-in or new user turn. Drives barge-in decisions.
    pub(super) bot_audio_sent: bool,
    /// Tracks PCM bytes sent to the client per turn for idle timer offset.
    pub(super) playback: PlaybackTracker,

    // ── Shutdown ──────────────────────────────────────────────
    /// Explicit exit flag — set to `true` to break the reactor loop.
    /// Decouples shutdown intent from channel state (audio_rx).
    pub(super) should_exit: bool,

    // ── Transport control ─────────────────────────────────────
    /// Sends commands (e.g. Close) back to the transport layer.
    /// Used to trigger telephony hangup via REST API.
    pub(super) transport: TransportControl,

    /// VAD silence window in ms (silence_frames * frame_duration_ms).
    /// Config-derived constant — used for STT timeout calculation.
    pub(super) vad_silence_ms: f64,

    /// Accumulated non-whitespace text length in the current LLM turn.
    /// Tracked for per-turn observability and telemetry.
    pub(super) turn_text_len: usize,
    /// Text actually sent to TTS in the current LLM turn (cleaned, stripped).
    ///
    /// **Lifecycle:**
    /// - Cleared at the start of every LLM turn (`start_llm_turn`).
    /// - Also cleared by `cancel_pipeline` (barge-in path).
    /// - Fed token-by-token via `send_tts_token`.
    /// - **Tool path**: drained by `mem::take` at `ToolCallStarted` to emit a
    ///   pre-tool assistant transcript (preamble text that preceded the call).
    /// - **Normal path** (no tool call): `LlmEvent::Finished` emits the
    ///   transcript from `content` (the full LLM response). `turn_spoken_text`
    ///   is intentionally NOT used — it would duplicate the transcript.
    pub(super) turn_spoken_text: String,
    /// Last user message text (for tool filler context).
    pub(super) last_user_message: Option<String>,
    /// Number of completed LLM turns in this session (for hang-up gate).
    pub(super) turn_count: u32,

    // ── Recording ──────────────────────────────────────────
    /// Cursor that timestamps every `Event::AgentAudio` chunk for recording placement.
    /// See [`AgentAudioCursor`] for the two-phase protocol.
    pub(super) tts_cursor: AgentAudioCursor,
    /// Guard to prevent duplicate `SessionEnded` events on the bus.
    pub(super) session_ended_emitted: bool,
    /// Temporary observability state for the opening greeting TTS.
    pub(super) greeting_observability: Option<GreetingObservability>,
}

pub(super) struct GreetingObservability {
    pub text: String,
    pub started_at: std::time::Instant,
    pub first_audio_at: Option<std::time::Instant>,
}

impl Reactor {
    /// Create a new Reactor with all stages initialized.
    ///
    /// Call `run()` to start the event loop.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: SessionConfig,
        backend: Box<dyn AgentBackend>,
        audio_rx: mpsc::UnboundedReceiver<Bytes>,
        mut tracer: Tracer,
        stt_config: SttProviderConfig,
        tts_config: TtsProviderConfig,
        transport_control_tx: mpsc::UnboundedSender<TransportCommand>,
    ) -> Self {
        let transport = TransportControl::new(transport_control_tx);
        let models_dir = &config.models_dir;

        let denoiser = DenoiserStage::new(
            models_dir,
            config.denoise_enabled,
            config.denoise_backend.clone(),
        );

        let vad_config = VadConfig::default();
        // VAD silence window: silence_frames * (FRAME_SIZE / sample_rate) * 1000
        let vad_silence_ms = vad_config.silence_frames as f64
            * (crate::utils::FRAME_SIZE as f64 / SAMPLE_RATE as f64)
            * 1000.0;
        tracer.set_vad_silence_ms(vad_silence_ms);

        let vad = VadStage::new(
            &format!("{}/silero_vad/silero_vad.onnx", models_dir),
            vad_config,
        );

        let smart_turn = if config.smart_turn_enabled {
            Some(SmartTurnStage::new(
                &format!("{}/smart_turn/smart-turn-v3.2-cpu.onnx", models_dir),
                config.smart_turn_threshold,
            ))
        } else {
            None
        };

        let stt = SttStage::new(build_stt_provider(&stt_config));
        let tts = TtsStage::new();

        let mut llm = LlmStage::new(backend);

        // Set system prompt (backend owns conversation)
        llm.set_system_prompt(config.system_prompt.clone());

        // Input resampler: client rate (e.g. 48kHz) → 16kHz internal
        let resampler = soxr::SoxrStreamResampler::new(config.input_sample_rate, SAMPLE_RATE)
            .expect("Failed to create SoxrStreamResampler for input audio");
        let ring_buffer = AudioRingBuffer::default();
        let output_sample_rate = config.output_sample_rate;

        Self {
            config,
            barge_in_policy: Box::new(policies::DefaultBargeInPolicy),
            hooks: Vec::new(),
            replay_log: replay::ReplayLog::new(),
            denoiser,
            vad,
            smart_turn,
            resampler,
            ring_buffer,
            stt,
            llm,
            tts,
            timers: DelayQueue::new(),
            reengagement: ReengagementTimer::default(),
            turn_phase: TurnPhase::default(),

            se_guard: session::SideEffectToolsGuard::default(),

            state: SessionState::Idle,
            audio_rx,
            tracer,
            session_phase: SessionPhase::Nascent,
            llm_turn: session::LlmTurnPhase::Idle,
            hang_up: session::HangUpPhase::Idle,
            tts_provider_config: tts_config,
            ws_tts_cmd_tx: None,
            ws_tts_audio_rx: None,
            bot_audio_sent: false,
            playback: PlaybackTracker::new(output_sample_rate),
            should_exit: false,
            transport,
            vad_silence_ms,
            turn_text_len: 0,
            turn_spoken_text: String::new(),
            last_user_message: None,
            turn_count: 0,
            tts_cursor: AgentAudioCursor::new(output_sample_rate),
            session_ended_emitted: false,
            greeting_observability: None,
        }
    }

    /// Register a lifecycle and behavior hook. Must be called before [`start()`](Self::start).
    pub fn add_hook(&mut self, hook: Box<dyn ReactorHook>) {
        self.hooks.push(hook);
    }

    /// Dispatch a hook notification to all registered hooks.
    ///
    /// Call this after every state transition. Example:
    /// ```ignore
    /// self.notify_hooks(|p| p.on_turn_start());
    /// ```
    #[inline]
    pub(super) fn notify_hooks(&mut self, f: impl Fn(&mut Box<dyn ReactorHook>)) {
        for hook in &mut self.hooks {
            f(hook);
        }
    }

    /// Enable input-event recording for this session.
    ///
    /// When enabled, every event received in the `select!` loop is appended to
    /// `replay_log`. Retrieve events via `replay_log.events()` or `replay_log.drain()`.
    pub fn enable_recording(&mut self) {
        self.replay_log.enable();
    }

    /// Take the replay log, leaving an empty disabled log in its place.
    pub fn take_replay_log(&mut self) -> replay::ReplayLog {
        std::mem::take(&mut self.replay_log)
    }

    /// Initialize all stages (loads ONNX models), connect STT, play greeting.
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.denoiser
            .initialize()
            .map_err(|e| format!("Denoiser init: {}", e))?;
        self.vad
            .initialize()
            .map_err(|e| format!("VAD init: {}", e))?;
        if let Some(st) = &mut self.smart_turn {
            if let Err(e) = st.initialize() {
                warn!("[reactor] SmartTurn init failed: {} — disabling", e);
                self.smart_turn = None;
            }
        }
        self.stt
            .connect()
            .await
            .map_err(|e| format!("STT connect: {}", e))?;

        // ── Session-level WS TTS connection ──────────────────────
        // If the provider is a WS streaming type, connect once here and
        // keep the connection alive across all turns.
        let tts_mode = build_tts_provider(&self.tts_provider_config);
        if let TtsMode::Streaming(mut provider) = tts_mode {
            provider.set_voice(&self.config.voice_id);
            match provider.connect().await {
                Ok(()) => {
                    info!("[reactor] WS TTS connected (session-scoped)");
                    let audio_rx = provider.take_audio_rx();

                    // Spawn a command task that drives the WS provider.
                    // This task lives for the entire session.
                    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<WsCmd>();
                    tokio::spawn(async move {
                        while let Some(cmd) = cmd_rx.recv().await {
                            match cmd {
                                WsCmd::SendText { text, context_id } => {
                                    provider.send_text(&text, &context_id).await;
                                }
                                WsCmd::Flush { context_id } => {
                                    provider.flush(&context_id).await;
                                }
                                WsCmd::Cancel { context_id } => {
                                    provider.cancel(&context_id).await;
                                }
                            }
                        }
                        // cmd_tx dropped — session is ending; close the WS cleanly.
                        provider.close().await;
                        info!("[reactor] WS TTS command task exiting");
                    });

                    self.ws_tts_cmd_tx = Some(cmd_tx);
                    self.ws_tts_audio_rx = audio_rx;
                }
                Err(e) => {
                    warn!(
                        "[reactor] WS TTS connect failed: {} — falling back to HTTP per-turn",
                        e
                    );
                    // ws_tts_cmd_tx stays None → all turns use HTTP path
                }
            }
        }

        self.tracer.emit(Event::SessionReady);
        self.set_state(SessionState::Listening);
        info!("[reactor] Started");

        // Kick off the greeting (if configured) non-awaited.
        // The existing run() select! arms handle WS audio routing and
        // on_tts_event() handles cleanup — no inner loop needed.
        if let Some(greeting) = self.config.greeting.clone() {
            if self.ws_tts_cmd_tx.is_some() {
                // WS path: kick off non-awaited; run()'s select! arms handle routing.
                self.initiate_greeting_ws(&greeting);
                self.session_phase = SessionPhase::GreetingPlaying;
            } else {
                // HTTP path: synchronous one-shot synthesis (no WS connection to reuse).
                self.play_greeting_http(&greeting).await;
            }
        } else {
            // No greeting — still need idle detection from session start so
            // the call disconnects if the user never speaks. Bypasses the
            // `had_first_interaction` guard (same as the post-greeting path).
            self.start_idle_timer_after_greeting();
        }

        Ok(())
    }

    /// The main event loop. Returns when the transport closes or
    /// `should_exit` is set (e.g. after agent hang-up).
    pub async fn run(&mut self) {
        loop {
            // ── Explicit exit check (decoupled from channel state) ──
            if self.should_exit {
                info!("[reactor] should_exit set — stopping");
                self.cancel_pipeline();
                break;
            }

            tokio::select! {
                biased;

                // ── Highest priority: incoming audio (keep VAD responsive) ──
                //
                //   replay_log.record(ReactorInput::*)
                //     → typed input snapshot (opt-in, zero-cost when disabled).
                //       Used by the replay/sim harness for deterministic testing.
                //
                // Each arm decides independently whether to call one or both.
                // See reactor/replay.rs for the ReactorInput type documentation.
                msg = self.audio_rx.recv() => {
                    match msg {
                        Some(raw) => {
                            // Record sample count only — raw PCM is too large.
                            self.replay_log.record(replay::ReactorInput::AudioIn {
                                samples: (raw.len() / 2) as u32,
                            });
                            self.on_audio(raw).await;
                        }
                        None => {
                            // Transport closed (WebSocket, WebRTC, etc.)
                            self.replay_log.record(replay::ReactorInput::TransportClosed);
                            info!("[reactor] Transport closed — stopping");
                            self.cancel_pipeline();
                            break;
                        }
                    }
                }

                // ── STT transcripts ──
                Some(ev) = self.stt.recv() => {
                    self.replay_log.record(replay::ReactorInput::SttEvent(ev.clone()));
                    self.on_stt_event(ev).await;
                }

                // ── LLM tokens / tool calls / finished ──
                Some(ev) = self.llm.recv(), if self.llm.is_active() => {
                    self.replay_log.record(replay::ReactorInput::LlmEvent(ev.clone()));
                    self.on_llm_event(ev).await;
                }

                // ── TTS audio chunks ──
                Some(ev) = self.tts.recv(), if self.tts.is_active() => {
                    // Record non-audio events only — audio chunks are large and
                    // replay-irrelevant (only Finished / Error drive state transitions).
                    if !matches!(ev, crate::types::TtsEvent::Audio(_)) {
                        self.replay_log.record(replay::ReactorInput::TtsEvent(ev.clone()));
                    }
                    self.on_tts_event(ev).await;
                }

                // ── WS TTS audio routing ──
                // Reads directly from the session-level WS channel (no relay task,
                // no mutex). All providers tag chunks with context_id; the reactor
                // forwards only those matching the active context and drops the rest.
                Some(chunk) = async {
                    match self.ws_tts_audio_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if self.tts.accepts_context_id(&chunk.context_id) {
                        self.tts.push_ws_chunk(chunk);
                    }
                    // else: stale chunk (cancelled context) — drop silently
                }

                // ── Timers (DelayQueue — one source for all timers) ──
                Some(expired) = self.timers.next() => {
                    let timer_key: TimerKey = expired.into_inner();
                    self.replay_log.record(replay::ReactorInput::TimerFired(timer_key));
                    self.on_timer(timer_key).await;
                }

                // ── Fallback: all channels closed (degenerate case) ──
                else => {
                    info!("[reactor] All channels closed — stopping");
                    self.cancel_pipeline();
                    break;
                }
            }

            // ── Post-dispatch: derive and emit UI state ───────────────────
            // Fires exactly once per loop iteration regardless of which select!
            // arm fired. No event handler needs to call compute_and_emit_state()
            // individually — state is always consistent at turn boundaries here.
            // (start_llm_turn() still calls it explicitly because it may be
            // invoked from START() before the loop enters.)
            self.compute_and_emit_state();
        }

        // Guarantee SessionEnded reaches all bus subscribers (recording, OTel, etc.)
        // even on abnormal exits (transport drop, all-channels-closed).
        if !self.session_ended_emitted {
            self.tracer.emit(Event::SessionEnded);
            self.session_ended_emitted = true;
        }

        // Explicitly close STT WebSocket and other connections
        self.close();
    }

    // ── Helpers ──────────────────────────────────────────────────

    /// Feed a token to the TTS stage (cleaned for audio first).
    pub(super) fn send_tts_token(&mut self, token: &str) {
        let cleaned = clean_for_tts(token);
        if !cleaned.is_empty() {
            self.tracer.mark_tts_text_fed();
            self.tracer.append_tts_text(&cleaned);
            self.turn_spoken_text.push_str(&cleaned);
            self.tts.feed_token(&cleaned);
        }
    }

    /// Strip internal turn-completion markers (✓/○/◐) from text.
    ///
    /// Applied before emitting assistant transcripts to the event bus so
    /// markers never appear in the UI or call recordings. Also applies
    /// the standard `clean_for_tts` emoji/markdown cleanup.
    pub(super) fn strip_turn_markers(&self, text: &str) -> String {
        clean_for_tts(text)
            .chars()
            .filter(|c| !matches!(c, '\u{2713}' | '\u{25CB}' | '\u{25D0}'))
            .collect()
    }

    /// Update session state and emit a StateChanged event.
    ///
    /// This is kept for explicit transitions (e.g. Idle→Listening on start,
    /// greeting playback). For pipeline-driven transitions, prefer
    /// `compute_and_emit_state()` which derives state from actual flags.
    pub(super) fn set_state(&mut self, new_state: SessionState) {
        if self.state != new_state {
            info!("[reactor] State: {:?} → {:?}", self.state, new_state);
            self.state = new_state;
            self.tracer.emit(Event::StateChanged {
                state: format!("{:?}", new_state).to_lowercase(),
            });
        }
    }

    /// Derive the UI state from actual pipeline flags and emit if changed.
    ///
    /// State is computed from the underlying truth (LLM active, TTS
    /// active, tools in flight) rather than manually tracked.
    ///
    /// Note: This intentionally never produces `SessionState::Idle`.
    /// `Idle` only exists between `new()` and `start()` and is not a
    /// valid pipeline-derived state.
    pub(super) fn compute_and_emit_state(&mut self) {
        let new_state = if self.se_guard.is_active() {
            SessionState::ToolCalling
        } else if self.tts.is_active() {
            SessionState::Speaking
        } else if self.llm.is_active() {
            SessionState::Processing
        } else {
            SessionState::Listening
        };
        self.set_state(new_state);
    }

    // ── Turn Coordinator dispatch ────────────────────────────────────

    /// Central dispatch for all turn-lifecycle events.
    ///
    /// 1. Runs pre-dispatch guards (hang_up, side_effect protection).
    /// 2. Calls the pure `TurnPhase::transition()` function.
    /// 3. Fills in timer keys for `ArmSttTimeout` / `ArmEotFallback`.
    /// 4. Executes the returned `TurnAction`s.
    ///
    /// All turn-lifecycle decisions flow through this single point.
    pub(in crate::reactor) async fn dispatch_turn_event(&mut self, event: TurnEvent) {
        // ── Pre-dispatch guards ──────────────────────────────────

        // Guard: hang-up in progress — suppress new-turn events.
        // SpeechEnded is NOT suppressed: it must flow through to re-open
        // the TTS output gate (transition out of UserSpeaking).
        if self.hang_up.is_pending() {
            match &event {
                TurnEvent::SpeechStarted { .. } | TurnEvent::Transcript { .. } => {
                    info!("[coordinator] {:?} suppressed (hang_up pending)", event);
                    return;
                }
                _ => {} // SpeechEnded, timers still fire during hangup
            }
        }

        // Guard: side-effect tools running — buffer transcripts, protect SpeechEnded.
        if self.se_guard.is_active() {
            match &event {
                TurnEvent::Transcript { ref text } => {
                    info!(
                        "[coordinator] Buffering transcript (side-effect active): '{}'",
                        text
                    );
                    self.se_guard.buffer_transcript(text);
                    return;
                }
                TurnEvent::SpeechEnded { .. } => {
                    // Side-effect tools are running. We must NOT let the state
                    // machine's barge-in actions (CancelPipeline, FinalizeSTT)
                    // fire — that would kill the running tool. Instead:
                    //   1. Finalize STT so the transcript arrives (buffered above).
                    //   2. Reset turn_phase to Listening to re-open the TTS gate.
                    //      (Without this, UserSpeaking persists and closes the gate.)
                    //
                    // reset() is safe here because:
                    // - turn_phase can only be UserSpeaking at this point. A
                    //   side-effect tool starts during an LLM turn when turn_phase
                    //   is Listening; UserSpeaking is set by SpeechStarted.
                    // - Timer-owning phases (WaitingForTranscript, EotFallback)
                    //   require SmartTurn/EoT decisions which only run after
                    //   SpeechEnded — unreachable here since this IS SpeechEnded.
                    // - reset() on UserSpeaking has no timer keys to clean up.
                    self.tracer.mark_stt_finalize_sent();
                    self.stt.finalize();
                    self.turn_phase.reset(&mut self.timers);
                    return;
                }
                _ => {} // SpeechStarted, timers pass through
            }
        }

        // ── State machine transition ─────────────────────────────
        let old_phase = std::mem::take(&mut self.turn_phase);
        let (next_phase, actions) = old_phase.transition(event);

        // ── Resolve NextPhase → TurnPhase, allocating timer keys ─────────
        // NextPhase::WaitForTranscript and NextPhase::StartEotFallback need
        // a timer key from the reactor's DelayQueue — transition() can't
        // create them without reactor access. This is the single resolution
        // point; no second pass over actions needed.
        self.turn_phase = match next_phase {
            NextPhase::Set(phase) => phase,
            NextPhase::WaitForTranscript => {
                let timeout_ms = self
                    .config
                    .stt_p99_latency_ms
                    .saturating_sub(self.vad_silence_ms as u32)
                    .max(100);
                let dur = std::time::Duration::from_millis(timeout_ms as u64);
                let timer_key = self.timers.insert(crate::types::TimerKey::SttTimeout, dur);
                TurnPhase::WaitingForTranscript { timer_key }
            }
            NextPhase::StartEotFallback => {
                let dur = std::time::Duration::from_secs(3);
                let timer_key = self
                    .timers
                    .insert(crate::types::TimerKey::EndOfTurnFallback, dur);
                TurnPhase::EotFallback { timer_key }
            }
        };

        // ── Execute side effects ─────────────────────────────────
        for action in actions {
            self.execute_turn_action(action).await;
        }
    }

    /// Execute a single [`TurnAction`] returned by the transition function.
    ///
    /// This is the "dumb runner" — it maps action variants to Reactor method
    /// calls. No decisions are made here; all logic lives in `transition()`.
    async fn execute_turn_action(&mut self, action: TurnAction) {
        match action {
            TurnAction::EmitInterrupt => {
                self.tracer.emit(Event::Interrupt);
            }
            TurnAction::ClearBotAudioSent => {
                self.bot_audio_sent = false;
            }
            TurnAction::EmitAgentEvent(kind) => {
                self.emit_agent_event(kind);
            }
            TurnAction::CancelPipeline => {
                self.cancel_pipeline();
            }
            TurnAction::FinalizeSTT => {
                self.stt.finalize();
            }
            TurnAction::CommitTurn(text) => {
                // Prepend any buffered orphaned transcript (I5).
                let text = if let Some(prefix) = self.se_guard.take_deferred() {
                    let full = format!("{} {}", prefix, text);
                    info!(
                        "[coordinator] Prepended orphaned transcript: '{}' + '{}' = '{}'",
                        prefix, text, full
                    );
                    full
                } else {
                    text
                };

                // Empty-text guard: STT sometimes returns whitespace-only text.
                // We dispatch ALL transcripts through the coordinator (so timer
                // cleanup always runs), but we don't start a LLM turn for nothing.
                if text.trim().is_empty() {
                    info!("[coordinator] Empty transcript — ignoring, starting idle timer");
                    self.start_idle_timer();
                    return;
                }

                self.commit_turn_text(text).await;
            }
            TurnAction::BufferTranscript(text) => {
                info!("[coordinator] Buffering orphaned transcript: '{}'", text);
                self.se_guard.buffer_transcript(&text);
            }
            TurnAction::CommitBargeIn(text) => {
                // The barge-in was already approved by the state machine when it
                // entered BargeInWordCount. Pre-dispatch guards in dispatch_turn_event
                // already handle hang_up (suppresses transcript) and side_effect
                // (buffers transcript) before the transition function runs.
                //
                // We must NOT re-check interrupt_policy() here: by this point,
                // CancelPipeline has reset pipeline_active=false and ClearBotAudioSent
                // has reset bot_audio_sent=false, so the policy would return Block
                // and silently drop the user's transcript.
                use unicode_segmentation::UnicodeSegmentation;

                let word_count = text.unicode_words().count() as u32;
                // Always pass tts_active=true: CancelPipeline has already run
                // so self.tts.is_active() is always false here, which would
                // bypass the word gate entirely. By construction, we only reach
                // BargeInWordCount when TTS was active at SpeechEnded time —
                // the gate should apply as if bot audio is still playing.
                if self.barge_in_policy.should_accept(
                    true,
                    word_count,
                    self.config.min_barge_in_words,
                ) {
                    info!(
                        "[coordinator] BARGE-IN accepted: '{}' ({} words ≥ {} required)",
                        text, word_count, self.config.min_barge_in_words
                    );
                    self.commit_turn_text(text).await;
                } else {
                    info!(
                        "[coordinator] BARGE-IN rejected: '{}' ({} words < {} required) — bot continues",
                        text, word_count, self.config.min_barge_in_words
                    );
                    self.start_idle_timer();
                }
            }
            TurnAction::CommitLateTranscript(text) => {
                // Accept only if pipeline is idle — prevents a duplicate turn
                // when the pipeline already started with the timed-out path.
                if self.is_pipeline_active() {
                    info!(
                        "[coordinator] Late STT transcript dropped (pipeline active): '{}'",
                        text
                    );
                } else {
                    info!(
                        "[coordinator] Late STT transcript accepted (pipeline idle): '{}'",
                        text
                    );
                    self.commit_turn_text(text).await;
                }
            }
            TurnAction::CancelTimer(key) => {
                self.timers.remove(&key);
            }
            TurnAction::StartIdleTimer => {
                self.start_idle_timer();
            }
            TurnAction::MarkSpeechEnded => {
                self.tracer.mark_speech_ended();
            }
            TurnAction::Trace(name) => {
                self.tracer.trace(name);
            }
            TurnAction::MarkSttFinalizeSent => {
                self.tracer.mark_stt_finalize_sent();
            }
        }
    }

    /// Commit `text` as the current user turn: emit transcript event, add to
    /// LLM history, open a tracer span, and start the LLM response.
    ///
    /// Single source of truth for all commit paths (`CommitTurn`,
    /// `CommitBargeIn`, `CommitLateTranscript`). Callers are responsible for
    /// any pre-commit guards (empty text, word count, pipeline active).
    async fn commit_turn_text(&mut self, text: String) {
        self.tracer.emit(Event::Transcript {
            role: "user".into(),
            text: text.clone(),
        });
        self.llm.add_user_message(text.clone());
        self.last_user_message = Some(text.clone());
        self.tracer.start_turn(
            &self.config.stt_provider,
            &self.config.stt_model,
            &text,
            &self.config.language,
            self.config.smart_turn_enabled,
        );
        self.notify_hooks(|p| p.on_turn_start());
        self.start_llm_turn().await;
    }

    /// Whether the pipeline is actively doing work (LLM, TTS, or tools).
    ///
    /// This replaces `state != Listening` checks — it queries the actual
    /// underlying flags instead of a derived enum.
    #[inline]
    pub(super) fn is_pipeline_active(&self) -> bool {
        self.llm.is_active() || self.tts.is_active() || self.se_guard.is_active()
    }

    /// Emit an agent lifecycle event.
    pub(super) fn emit_agent_event(&self, kind: &str) {
        self.tracer.emit(Event::AgentEvent {
            kind: kind.to_string(),
        });
    }

    /// Subscribe to the event bus.
    ///
    /// Returns a broadcast receiver. Multiple consumers can subscribe.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.tracer.subscribe()
    }

    /// Get a reference to the Tracer (for filtered subscriptions).
    pub fn tracer(&self) -> &Tracer {
        &self.tracer
    }

    /// Close all connections.
    pub fn close(&self) {
        self.stt.close();
    }
}

// ── Text Processing ───────────────────────────────────────────────

/// Strip emoji and markdown formatting from LLM tokens for clean TTS input.
///
/// Note: turn-completion markers (✓/○/◐) are intentionally NOT filtered here.
/// They are stripped separately by `strip_turn_markers` at the transcript
/// emission site only, so that legitimate uses of ✓ in conversational text
/// can still be spoken.
fn clean_for_tts(token: &str) -> String {
    token
        .chars()
        .filter(|c| !c.is_ascii_control() && *c != '*' && *c != '#' && *c != '_')
        .filter(|c| {
            !matches!(c, '\u{1F600}'..='\u{1F6FF}' | '\u{2600}'..='\u{26FF}' | '\u{2700}'..='\u{27BF}')
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::AgentAudioCursor;

    /// PCM16 bytes for N samples at a given rate (2 bytes per sample).
    fn pcm_bytes(samples: usize) -> usize {
        samples * 2
    }

    #[test]
    fn cursor_never_moves_backward() {
        // stamp() must never return a value less than the previous chunk's end,
        // even across turn boundaries where begin_turn() + wall-clock snap fires.
        let mut c = AgentAudioCursor::new(24_000);

        // Turn 1: stamp 24 000 samples (1 second of audio, emitted instantly).
        c.begin_turn();
        let o1 = c.stamp(pcm_bytes(24_000));
        assert_eq!(o1, 0, "first stamp should start at 0");
        assert_eq!(c.cursor, 24_000);

        // Turn 2: begin_turn() marks a snap pending; the snap fires in stamp().
        // Wall-clock elapsed ≈ 0 (instant), so cursor stays at 24_000 (not rewound).
        c.begin_turn();
        let o2 = c.stamp(pcm_bytes(100));
        assert!(
            o2 >= 24_000,
            "second turn must start at or after the first turn's end, got {o2}"
        );
    }

    #[test]
    fn stamp_advances_by_sample_count() {
        let mut c = AgentAudioCursor::new(crate::utils::SAMPLE_RATE);
        c.begin_turn();

        // 512 samples → 1024 bytes
        let o1 = c.stamp(pcm_bytes(512));
        let o2 = c.stamp(pcm_bytes(512));
        let o3 = c.stamp(pcm_bytes(256));

        assert_eq!(o1, 0);
        assert_eq!(o2, 512);
        assert_eq!(o3, 1024);
        assert_eq!(c.cursor, 1280);
    }

    #[test]
    fn elapsed_samples_is_approximately_correct() {
        // elapsed_samples() must return a value within 1 sample of the
        // expected wall-clock sample count.  We use a real Instant, so we
        // allow ±2 samples of slack for scheduler jitter.
        let rate: u32 = 24_000;
        let c = AgentAudioCursor::new(rate);

        // Instant::now() was captured inside new(); calling elapsed_samples()
        // immediately should return close to 0.
        let e = c.elapsed_samples();
        assert!(
            e <= 2,
            "elapsed_samples() immediately after construction should be ~0, got {e}"
        );
    }

    #[test]
    fn begin_turn_snaps_forward_after_real_time() {
        let mut c = AgentAudioCursor::new(24_000);
        c.begin_turn();

        let o1 = c.stamp(pcm_bytes(100)); // emits 100 samples instantly
        assert_eq!(o1, 0);
        assert_eq!(c.cursor, 100);

        // Sleep for 200ms. At 24 kHz this implies ~4800 elapsed samples.
        // We assert > 2400 (50 % of expected) to give 2× headroom against
        // macOS scheduler jitter and loaded CI runners where the actual wakeup
        // can slip by 30–50 ms beyond the requested duration.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Start turn 2. The cursor should snap forward to wall-clock position.
        c.begin_turn();
        let o2 = c.stamp(pcm_bytes(100));

        assert!(
            o2 > 2400,
            "cursor did not snap forward after sleep: got {o2}"
        );
    }
}
