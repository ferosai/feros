//! LLM / STT / TTS event handlers and pipeline lifecycle.
//!
//! Contains the hot path from transcript → LLM generation → TTS output,
//! plus `cancel_pipeline()` (the entire barge-in implementation) and the
//! synchronous greeting playback.

use bytes::Bytes;
use tokio_util::time::delay_queue;
use tracing::{info, warn};
use voice_trace::Event;

use crate::providers::tts::{build_tts_provider, TtsMode};
use crate::reactor::proc::tts::WsTtsHandle;
use crate::types::{LlmEvent, SessionState, SttEvent, TimerKey, TtsEvent, TurnCompletion};

use super::{Reactor, SessionPhase};

// ── LLM generation phase ────────────────────────────────────────

/// State of one LLM → TTS streaming turn.
///
/// Replaces four scattered fields:
/// - `turn_completion_buffer` + `turn_completion_checked` → `Generating { buf }`
/// - `suppressing_llm_output` + `suppressed_turn_response` → `Suppressing { response }`
///
/// A `cancel_pipeline()` / `start_llm_turn()` reset is now a single assignment.
#[derive(Default)]
pub(crate) enum LlmTurnPhase {
    /// No LLM turn is in flight.
    #[default]
    Idle,
    /// LLM is streaming tokens to TTS.
    ///
    /// `buf` accumulates the first tokens until we have ≥4 characters to run
    /// the turn-completion marker check (✓/○/◐). `None` once the check has run.
    Generating { buf: Option<String> },
    /// A ○ or ◐ marker was detected. All tokens are swallowed here instead of
    /// being sent to TTS. On `LlmEvent::Finished` (or a late detection) the
    /// `response` is stored and replayed by the `TurnCompletion` timer.
    Suppressing { response: Option<String> },
}

impl LlmTurnPhase {
    /// True after the marker scan is complete (whether or not a marker was found).
    pub(super) fn marker_checked(&self) -> bool {
        match self {
            LlmTurnPhase::Generating { buf } => buf.is_none(),
            LlmTurnPhase::Suppressing { .. } | LlmTurnPhase::Idle => true,
        }
    }

    /// Pure function for processing an incoming LLM token.
    pub(super) fn process_token(
        &mut self,
        token: &str,
        turn_completion_enabled: bool,
    ) -> TokenAction {
        match self {
            LlmTurnPhase::Suppressing { .. } => TokenAction::Drop,
            // The `buf.is_some()` guard guarantees `take().unwrap()` cannot panic.
            LlmTurnPhase::Generating { buf } if turn_completion_enabled && buf.is_some() => {
                let mut b = buf.take().unwrap();
                b.push_str(token);
                if b.len() < 4 {
                    *buf = Some(b);
                    TokenAction::Accumulate
                } else {
                    // Option::take() left `buf` as `None`, correctly marking it as checked.
                    if let Some((marker, skip)) = TurnCompletion::detect(&b) {
                        let emitted_text = if marker == TurnCompletion::Complete {
                            b[skip..].to_string()
                        } else {
                            String::new()
                        };
                        TokenAction::MarkerCheckCompleted {
                            marker: Some(marker),
                            emitted_text,
                        }
                    } else {
                        TokenAction::MarkerCheckCompleted {
                            marker: None,
                            emitted_text: b,
                        }
                    }
                }
            }
            _ => TokenAction::Passthrough {
                token: token.to_string(),
            },
        }
    }

    /// Process the final flush if the token stream ended before the buffer
    /// reached 4 bytes. Returns `Some(MarkerCheckCompleted)` when the check
    /// ran, `None` when there was nothing to do (disabled, already checked,
    /// wrong state, or empty buffer).
    pub(super) fn process_late_flush(
        &mut self,
        turn_completion_enabled: bool,
    ) -> Option<TokenAction> {
        if !turn_completion_enabled || self.marker_checked() {
            return None;
        }
        // `!marker_checked()` implies we are in `Generating { buf: Some(_) }`.
        let LlmTurnPhase::Generating { buf } = self else {
            return None;
        };
        // `take()` marks the slot as checked (buf → None), matching the
        // hot-path invariant where `buf: None` means "scan already ran".
        let buf_val = buf.take()?; // None would mean already checked — can't happen here
        if buf_val.is_empty() {
            return None; // nothing to flush
        }
        if let Some((marker, skip)) = TurnCompletion::detect(&buf_val) {
            let emitted_text = if marker == TurnCompletion::Complete {
                buf_val[skip..].to_string()
            } else {
                String::new()
            };
            Some(TokenAction::MarkerCheckCompleted {
                marker: Some(marker),
                emitted_text,
            })
        } else {
            Some(TokenAction::MarkerCheckCompleted {
                marker: None,
                emitted_text: buf_val,
            })
        }
    }
}

pub(super) enum TokenAction {
    /// Drop – we're already suppressing.
    Drop,
    /// Buffer not yet full – accumulate and emit nothing.
    Accumulate,
    /// Reached threshold (or flushed), performed marker check.
    ///
    /// `emitted_text` is the text to forward to TTS (may be empty).
    /// When `marker == None`, `emitted_text` equals the full buffer contents.
    /// When `marker == Some(Complete)`, `emitted_text` is the suffix after the marker.
    /// When `marker == Some(Incomplete*)`, `emitted_text` is always empty.
    MarkerCheckCompleted {
        marker: Option<TurnCompletion>,
        emitted_text: String,
    },
    /// Direct route – just send to TTS.
    Passthrough { token: String },
}

// ── Hang-up phase ─────────────────────────────────────────

/// Tracks the hang-up countdown after the agent decides to end the call.
///
/// Replaces three fields: `hang_up_pending: bool`, `hang_up_delay_key: Option<Key>`,
/// and `hook_hang_up_pending: bool`.
///
/// State diagram:
///   Idle → WaitingForTts    (LlmEvent::HangUp or hook classifier verdict)
///   WaitingForTts → WaitingForDelay { key }   (TtsFinished, delay needed)
///   WaitingForTts → done    (TtsFinished, no delay → initiate_shutdown)
///   WaitingForDelay { key } → done  (HangUpDelay timer fires → initiate_shutdown)
#[derive(Default)]
pub(crate) enum HangUpPhase {
    /// No hang-up is pending.
    #[default]
    Idle,
    /// Hang-up requested; waiting for the goodbye TTS to finish playing.
    WaitingForTts,
    /// TTS finished; waiting for the client playback buffer to drain.
    WaitingForDelay { timer_key: delay_queue::Key },
}
impl HangUpPhase {
    /// True when a hang-up has been requested (any non-Idle state).
    pub(super) fn is_pending(&self) -> bool {
        !matches!(self, HangUpPhase::Idle)
    }
}
// ── Side-effect protection window ──────────────────────────────────────

/// Timer side-effect actions returned by [`SideEffectToolsGuard`] methods.
///
/// The guard is pure — it never touches the `DelayQueue` directly. Callsites
/// apply these actions themselves, keeping the guard testable without a tokio
/// runtime or a real timer queue.
pub(crate) enum SeGuardAction {
    /// Cancel the key (if `Some`) and insert a fresh 30-second watchdog.
    RearmWatchdog {
        /// Key to remove before inserting new one (`None` on first call).
        cancel_key: Option<delay_queue::Key>,
    },
    /// Cancel the watchdog key; all tools have finished.
    CancelWatchdog {
        /// Key to remove from the timer queue.
        cancel_key: Option<delay_queue::Key>,
        /// Transcript buffered during the window, to replay now.
        deferred: Option<String>,
    },
    /// Window cancelled (barge-in / reset) — emit orphan audit events.
    Orphaned {
        /// Key to remove.
        cancel_key: Option<delay_queue::Key>,
        /// Tool IDs still in-flight at cancellation time.
        tool_ids: std::collections::HashMap<String, String>,
    },
    /// No timer change needed (other tools still in flight).
    None,
}

/// Tracks the active side-effect tool window.
///
/// Side-effect tools (those that may produce observable writes — database
/// mutations, API calls, etc.) open a "protection window" that:
///
/// 1. Prevents barge-in from interrupting the pipeline mid-tool.
/// 2. Buffers any user transcript that arrives while tools are running.
/// 3. Arms a watchdog timeout so a hung tool can’t stall the session.
///
/// **This struct is pure** — no `DelayQueue` references. All timer operations
/// are expressed as [`SeGuardAction`] return values that the caller applies.
///
/// Invariant: `timeout_key` and `tool_ids` are non-empty iff `in_flight > 0`.
#[derive(Default)]
pub(crate) struct SideEffectToolsGuard {
    pub(super) in_flight: u32,
    tool_ids: std::collections::HashMap<String, String>,
    timeout_key: Option<delay_queue::Key>,
    deferred_transcript: Option<String>,
}

impl SideEffectToolsGuard {
    /// True when at least one side-effect tool is executing.
    pub(super) fn is_active(&self) -> bool {
        self.in_flight > 0
    }

    /// Register a new tool call. Returns [`SeGuardAction::RearmWatchdog`].
    ///
    /// The caller must: cancel `cancel_key` (if `Some`), insert a fresh
    /// `TimerKey::SideEffectTimeout` timer, then call [`set_watchdog_key`](Self::set_watchdog_key).
    pub(super) fn tool_started(&mut self, id: String, name: String) -> SeGuardAction {
        self.in_flight += 1;
        self.tool_ids.insert(id, name);
        SeGuardAction::RearmWatchdog {
            cancel_key: self.timeout_key.take(),
        }
    }

    /// Store the new watchdog key after the caller inserted the timer.
    /// Must be called immediately after applying a `RearmWatchdog` action.
    pub(super) fn set_watchdog_key(&mut self, key: delay_queue::Key) {
        self.timeout_key = Some(key);
    }

    /// Record a tool completion. Returns:
    /// - [`SeGuardAction::None`] while others are still in flight.
    /// - [`SeGuardAction::CancelWatchdog`] when the last tool finished.
    pub(super) fn tool_completed(&mut self, id: &str) -> SeGuardAction {
        self.tool_ids.remove(id);
        self.in_flight = self.in_flight.saturating_sub(1);
        if self.in_flight > 0 {
            return SeGuardAction::None;
        }
        SeGuardAction::CancelWatchdog {
            cancel_key: self.timeout_key.take(),
            deferred: self.deferred_transcript.take(),
        }
    }

    /// Append a transcript fragment that arrived during the protection window.
    pub(super) fn buffer_transcript(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let new_text = match self.deferred_transcript.take() {
            Some(mut existing) => {
                existing.push(' ');
                existing.push_str(text);
                existing
            }
            None => text.to_string(),
        };
        self.deferred_transcript = Some(new_text);
    }

    /// Cancel the window on barge-in / pipeline reset. Returns [`SeGuardAction::Orphaned`].
    pub(super) fn cancel(&mut self) -> SeGuardAction {
        self.deferred_transcript = None;
        self.in_flight = 0;
        SeGuardAction::Orphaned {
            cancel_key: self.timeout_key.take(),
            tool_ids: std::mem::take(&mut self.tool_ids),
        }
    }

    /// Timeout fired externally. Returns `(in_flight_count, deferred_transcript)`.
    /// Does NOT clear `tool_ids` — caller must invoke `cancel()` for the orphan audit.
    pub(super) fn on_timeout(&mut self) -> (u32, Option<String>) {
        self.timeout_key = None;
        let count = self.in_flight;
        let deferred = self.deferred_transcript.take();
        self.in_flight = 0;
        (count, deferred)
    }

    /// Take (drain) the deferred transcript, if any.
    pub(super) fn take_deferred(&mut self) -> Option<String> {
        self.deferred_transcript.take()
    }
}

#[cfg(test)]
mod se_guard_tests {
    use super::*;

    #[test]
    fn tool_started_rearms_no_prior_key() {
        let mut g = SideEffectToolsGuard::default();
        let action = g.tool_started("id1".into(), "tool".into());
        assert!(matches!(
            action,
            SeGuardAction::RearmWatchdog { cancel_key: None }
        ));
        assert!(g.is_active());
    }

    #[test]
    fn tool_completed_none_while_others_in_flight() {
        let mut g = SideEffectToolsGuard::default();
        g.tool_started("id1".into(), "a".into());
        g.tool_started("id2".into(), "b".into());
        let action = g.tool_completed("id1");
        assert!(matches!(action, SeGuardAction::None));
        assert!(g.is_active());
    }

    #[test]
    fn tool_completed_last_drains_deferred() {
        let mut g = SideEffectToolsGuard::default();
        g.tool_started("id1".into(), "a".into());
        g.buffer_transcript("hello");
        let action = g.tool_completed("id1");
        match action {
            SeGuardAction::CancelWatchdog {
                cancel_key: None,
                deferred: Some(t),
            } => {
                assert_eq!(t, "hello");
            }
            _ => panic!("expected CancelWatchdog with deferred"),
        }
        assert!(!g.is_active());
    }

    #[test]
    fn cancel_returns_orphaned_map() {
        let mut g = SideEffectToolsGuard::default();
        g.tool_started("id1".into(), "x".into());
        g.buffer_transcript("dropped");
        let action = g.cancel();
        match action {
            SeGuardAction::Orphaned {
                cancel_key: None,
                tool_ids,
            } => {
                assert!(tool_ids.contains_key("id1"));
            }
            _ => panic!("expected Orphaned"),
        }
        assert!(!g.is_active());
        assert!(g.take_deferred().is_none()); // transcript dropped
    }

    #[test]
    fn buffer_transcript_joins_with_space() {
        let mut g = SideEffectToolsGuard::default();
        g.buffer_transcript("hello");
        g.buffer_transcript("world");
        assert_eq!(g.take_deferred(), Some("hello world".into()));
    }

    #[test]
    fn on_timeout_returns_count_leaves_tool_ids() {
        let mut g = SideEffectToolsGuard::default();
        g.tool_started("id1".into(), "t".into());
        g.buffer_transcript("queued");
        let (count, deferred) = g.on_timeout();
        assert_eq!(count, 1);
        assert_eq!(deferred, Some("queued".into()));
        assert_eq!(g.in_flight, 0);
        assert!(g.tool_ids.contains_key("id1")); // intact for cancel audit
    }

    #[test]
    fn saturating_sub_prevents_underflow() {
        let mut g = SideEffectToolsGuard::default();
        let action = g.tool_completed("ghost");
        assert!(matches!(action, SeGuardAction::CancelWatchdog { .. }));
    }
}

impl Reactor {
    // ── STT ──────────────────────────────────────────────────────

    pub(super) async fn on_stt_event(&mut self, event: SttEvent) {
        match event {
            SttEvent::FirstTextReceived => {
                // Capture STT TTFB: finalize_sent → first text from STT server.
                self.tracer.mark_stt_first_text();
            }
            SttEvent::PartialTranscript(_) => {
                // Interim results (e.g. Deepgram with interim_results=true).
                // Not acted upon in the current pipeline — only final transcripts
                // drive LLM turns. Included for completeness / future consumers.
            }
            SttEvent::Error(reason) => {
                // The STT WS connection died and reconnect was exhausted.
                // Log and continue — the Reactor can still receive audio and
                // the connection may recover on the next session.
                warn!("[reactor] STT connection error: {}", reason);
            }
            SttEvent::Transcript(text) => {
                self.tracer.trace("Transcript");
                self.tracer.mark_transcript();
                info!("[reactor] Transcript: '{}'", text);

                // Notify hooks — called before dispatch so hooks see every
                // transcript including empty ones (which cancel idle timers).
                self.notify_hooks(|p| p.on_stt_transcript(&text, true));

                // Always dispatch to coordinator — even empty transcripts must
                // flow through so the state machine can cancel timers that are
                // currently armed (e.g. WaitingForTranscript timer). The empty
                // check lives inside CommitTurn where it calls start_idle_timer
                // instead of start_llm_turn.
                let event = super::turn_phase::TurnEvent::Transcript { text };
                self.dispatch_turn_event(event).await;
            }
        }
    }

    // ── LLM ──────────────────────────────────────────────────────

    pub(super) async fn on_llm_event(&mut self, event: LlmEvent) {
        match event {
            LlmEvent::Token(token) => {
                // First token: mark for metrics
                self.tracer.mark_llm_first_token();

                // Track non-whitespace text length for tool filler gate
                self.turn_text_len += token.trim().len();

                // ── Turn Completion Marker Detection ────────────────────────────

                match self
                    .llm_turn
                    .process_token(&token, self.config.turn_completion_enabled)
                {
                    TokenAction::Drop | TokenAction::Accumulate => {}
                    TokenAction::Passthrough { token } => {
                        self.send_tts_token(&token);
                        self.notify_hooks(|p| p.on_llm_token(&token));
                    }
                    TokenAction::MarkerCheckCompleted {
                        marker,
                        emitted_text,
                    } => match marker {
                        Some(TurnCompletion::Complete) => {
                            info!("[reactor] Turn completion: ✓ COMPLETE");
                            if !emitted_text.is_empty() {
                                self.send_tts_token(&emitted_text);
                            }
                        }
                        Some(
                            m @ (TurnCompletion::IncompleteShort | TurnCompletion::IncompleteLong),
                        ) => {
                            self.arm_turn_completion_wait(m);
                            self.llm_turn = LlmTurnPhase::Suppressing { response: None };
                        }
                        None => {
                            // When no marker is detected, `emitted_text` carries the
                            // full original buffer (see `TokenAction::MarkerCheckCompleted` docs).
                            info!(
                                "[reactor] Turn completion: no marker detected (buffer={:?})",
                                emitted_text
                                    .char_indices()
                                    .nth(20)
                                    .map(|(i, _)| &emitted_text[..i])
                                    .unwrap_or(&emitted_text)
                            );
                            if !emitted_text.is_empty() {
                                self.send_tts_token(&emitted_text);
                            }
                        }
                    },
                }
            }

            LlmEvent::ToolCallStarted {
                id,
                name,
                side_effect,
            } => {
                self.tracer.trace("ToolCall");
                info!(
                    "[reactor] ToolCallStarted: {} (id={}, side_effect={})",
                    name, id, side_effect
                );
                self.notify_hooks(|p| p.on_tool_call(&name, &id));

                // Emit assistant transcript for any preamble text that was spoken
                // *before* this tool call. The LLM may produce text + a tool call in
                // the same response (e.g. "Let me confirm your booking..." immediately
                // followed by a tool invocation). Those tokens go to TTS right away but
                // are NOT present in LlmEvent::Finished's `content`, which only carries
                // the *post-tool* response. Without this step the preamble would be
                // spoken aloud but invisible in the UI and absent from the recording.
                let pre_tool_text = std::mem::take(&mut self.turn_spoken_text);
                if !pre_tool_text.is_empty() {
                    let clean = self.strip_turn_markers(&pre_tool_text);
                    if !clean.is_empty() {
                        info!("[reactor] Emitting pre-tool assistant text: {:?}", clean);
                        self.tracer.emit(Event::Transcript {
                            role: "assistant".into(),
                            text: clean,
                        });
                    }
                }

                if side_effect {
                    let action = self.se_guard.tool_started(id.clone(), name.clone());
                    if let SeGuardAction::RearmWatchdog { cancel_key } = action {
                        if let Some(k) = cancel_key {
                            self.timers.remove(&k);
                        }
                        let key = self.timers.insert(
                            crate::types::TimerKey::SideEffectTimeout,
                            std::time::Duration::from_secs(30),
                        );
                        self.se_guard.set_watchdog_key(key);
                    }
                }
                // TTS is NOT cancelled or restarted here. The same Cartesia
                // context stays alive across tool calls — the batching task
                // idles while the LLM waits for the tool result, then resumes
                // feeding tokens to the same context. This avoids the audio
                // stutter caused by Cartesia restarting its voice model.

                // Notify all subscribers (UI, OTel, etc.)
                self.tracer.emit(Event::ToolActivity {
                    tool_call_id: Some(id.clone()),
                    tool_name: name.clone(),
                    status: "executing".into(),
                    error_message: None,
                });
            }

            LlmEvent::ToolCallCompleted {
                id,
                name,
                success,
                error_message,
            } => {
                info!(
                    "[reactor] ToolCallCompleted: {} (id={}, success={})",
                    name, id, success
                );

                let action = self.se_guard.tool_completed(&id);
                let (all_done, deferred) = match action {
                    SeGuardAction::CancelWatchdog {
                        cancel_key,
                        deferred,
                    } => {
                        if let Some(k) = cancel_key {
                            self.timers.remove(&k);
                        }
                        (true, deferred)
                    }
                    _ => (false, None),
                };

                if all_done {
                    if let Some(text) = deferred {
                        info!("[reactor] Side-effect tools finished. Replaying deferred transcript: {}", text);

                        // Notify all subscribers of tool completion *before* cancelling pipeline
                        self.tracer.emit(Event::ToolActivity {
                            tool_call_id: Some(id.clone()),
                            tool_name: name.clone(),
                            status: if success { "completed" } else { "error" }.into(),
                            error_message: error_message.clone(),
                        });

                        // Forcefully cancel the pipeline (kills any sibling non-side-effect tools)
                        self.cancel_pipeline();

                        // Add user message after the tool result, then restart stream
                        self.llm.add_user_message(text.clone());
                        self.tracer.emit(Event::Transcript {
                            role: "user".into(),
                            text,
                        });
                        self.start_llm_turn().await;
                        return;
                    }
                }

                // Notify all subscribers
                self.tracer.emit(Event::ToolActivity {
                    tool_call_id: Some(id),
                    tool_name: name,
                    status: if success { "completed" } else { "error" }.into(),
                    error_message,
                });

                // TTS is NOT restarted here — the same Cartesia context
                // continues from before the tool call. The LLM resumes
                // streaming tokens into the existing batching task.
                //
                // turn_completion_buffer/checked/turn_text_len are intentionally
                // NOT reset: this is still the same LLM turn. The ✓ marker was
                // already detected pre-tool, so post-tool tokens skip marker
                // detection and go straight to TTS — which is correct.
            }

            LlmEvent::Finished { content } => {
                self.tracer.trace("LlmFinished");
                info!(
                    "[reactor] LLM finished (response={:?})",
                    content.as_deref().unwrap_or("<none>")
                );

                // If we were suppressing output (○/◐), skip TTS but preserve
                // the response for replay when the TurnCompletion timer fires.
                // `save_suppressed_response` sets llm_turn = Suppressing { response }
                // directly — we don't reset to Idle first so there's no transient
                // invalid state if the content is None/empty.
                if let LlmTurnPhase::Suppressing { .. } = self.llm_turn {
                    self.save_suppressed_response(&content);
                    self.start_idle_timer();
                    return;
                }

                // ── Late turn-completion check ────────────────────────────
                // If the LLM output was very short (< 4 bytes, e.g. just "○"),
                // the token handler never ran the marker detection. Flush now.
                if let Some(TokenAction::MarkerCheckCompleted {
                    marker,
                    emitted_text,
                }) = self
                    .llm_turn
                    .process_late_flush(self.config.turn_completion_enabled)
                {
                    match marker {
                        Some(TurnCompletion::Complete) => {
                            info!("[reactor] Turn completion (late): ✓ COMPLETE (short output)");
                            if !emitted_text.is_empty() {
                                self.send_tts_token(&emitted_text);
                            }
                        }
                        Some(
                            m @ (TurnCompletion::IncompleteShort | TurnCompletion::IncompleteLong),
                        ) => {
                            self.arm_turn_completion_wait(m);
                            self.save_suppressed_response(&content);
                            return;
                        }
                        None => {
                            info!(
                                "[reactor] Turn completion (late): no marker detected (buffer={:?})",
                                emitted_text
                                    .char_indices()
                                    .nth(20)
                                    .map(|(i, _)| &emitted_text[..i])
                                    .unwrap_or(&emitted_text)
                            );
                            if !emitted_text.is_empty() {
                                self.send_tts_token(&emitted_text);
                            }
                        }
                    }
                }

                // Tell TTS to flush remaining buffer
                self.tts.flush();

                // Send assistant text to all subscribers (markers stripped)
                if let Some(ref text) = content {
                    let clean_text = self.strip_turn_markers(text);
                    if !clean_text.is_empty() {
                        self.tracer.emit(Event::Transcript {
                            role: "assistant".into(),
                            text: clean_text,
                        });
                    }
                }

                // Notify hooks: turn ended, with full spoken text.
                let spoken = self.strip_turn_markers(&self.turn_spoken_text.clone());
                self.notify_hooks(|p| p.on_turn_end(&spoken));

                // Increment turn count.
                self.turn_count += 1;
            }

            LlmEvent::Error(e) => {
                warn!("[reactor] LLM error: {}", e);
                self.tracer.emit(Event::Error {
                    source: "llm".into(),
                    message: e.clone(),
                });
                self.tts.cancel();
                self.start_idle_timer();
            }

            LlmEvent::OnHold { duration_secs } => {
                info!(
                    "[reactor] Agent called on_hold (duration: {}s) — suppressing idle timer",
                    duration_secs
                );
                self.tracer.trace("OnHold");
                self.reengagement.hold_until = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(duration_secs as u64),
                );
                self.start_idle_timer();
            }

            LlmEvent::HangUp { reason, content } => {
                info!(
                    "[reactor] Agent hang_up: {} (tts_active={}, response={:?})",
                    reason,
                    self.tts.is_active(),
                    content.as_deref().unwrap_or("<none>")
                );
                self.tracer.trace("HangUp");

                // Emit assistant transcript so recordings capture the final response
                if let Some(ref text) = content {
                    let clean_text = self.strip_turn_markers(text);
                    if !clean_text.is_empty() {
                        self.tracer.emit(Event::Transcript {
                            role: "assistant".into(),
                            text: clean_text,
                        });
                    }
                }

                // Let the goodbye TTS finish playing before closing
                self.tts.flush();
                self.hang_up = HangUpPhase::WaitingForTts;

                // If there was no TTS buffered (agent called hang_up without
                // any preceding text), close immediately
                if !self.tts.is_active() {
                    info!("[reactor] hang_up: TTS not active — shutting down immediately");
                    self.finalize_hang_up("no pending TTS");
                }
            }

            LlmEvent::EscalateCall {
                destination,
                reason,
            } => {
                info!(
                    "[reactor] Agent escalate_call: {} → {} (tts_active={})",
                    reason,
                    destination,
                    self.tts.is_active()
                );
                self.tracer.trace("EscalateCall");
                self.tracer.emit(Event::ToolActivity {
                    tool_call_id: None,
                    tool_name: "escalate_call".into(),
                    status: "executing".into(),
                    error_message: None,
                });

                // Store the destination; initiate_shutdown will send Transfer before Close.
                self.pending_transfer_destination = Some(destination);

                // Reuse the hang-up state machine: drain TTS first, then shutdown.
                self.tts.flush();
                self.hang_up = HangUpPhase::WaitingForTts;

                if !self.tts.is_active() {
                    info!("[reactor] escalate_call: TTS not active — transferring immediately");
                    self.finalize_hang_up("escalate_call (no pending TTS)");
                }
            }

            LlmEvent::LlmComplete(data) => {
                // Forward to the event bus for Langfuse subscriber
                self.tracer.emit(Event::LlmComplete(data));
            }
        }
    }

    // ── TTS ──────────────────────────────────────────────────────

    pub(super) async fn on_tts_event(&mut self, event: TtsEvent) {
        match event {
            TtsEvent::Audio(pcm) => {
                // ── OutputGate (I2): silence bot audio while user is speaking ──
                // AudioOutputPermit can only be obtained when the user is NOT
                // speaking. If None, the TTS gate is closed — drop the chunk.
                if self.turn_phase.audio_output_permit().is_none() {
                    return;
                }
                self.tracer.mark_tts_first_audio();
                if matches!(self.session_phase, SessionPhase::GreetingPlaying) {
                    if let Some(obs) = self.greeting_observability.as_mut() {
                        if obs.first_audio_at.is_none() {
                            obs.first_audio_at = Some(std::time::Instant::now());
                        }
                    }
                }
                self.bot_audio_sent = true;
                self.playback.record(pcm.len());
                let offset_samples = self.tts_cursor.stamp(pcm.len());
                self.tracer.emit(Event::AgentAudio {
                    pcm,
                    sample_rate: self.config.output_sample_rate,
                    offset_samples,
                });
            }
            TtsEvent::Finished => {
                self.tracer.trace("TtsFinished");
                info!("[reactor] TTS finished");

                // Don't end the turn while the agent is still in a tool round
                // or side-effect tools are executing. TTS was flushed before
                // tool execution started; the real TTS finish comes after the
                // post-tool LLM response.
                if self.llm.is_active() || self.se_guard.is_active() {
                    info!("[reactor] TTS finished during tool execution — keeping turn open");
                    self.tts.mark_finished();
                    self.playback.reset();
                    return;
                }

                // Mark TTS stage as inactive so is_pipeline_active() returns
                // false and compute_and_emit_state() transitions to Listening.
                self.tts.mark_finished();

                // ── Greeting path ─────────────────────────────────────────
                // The greeting TTS was kicked off non-awaited from start()
                // before run() entered the loop. No LLM turn was started, so
                // we skip turn metrics and use the after-greeting idle timer
                // variant (which bypasses the Nascent guard).
                if matches!(self.session_phase, SessionPhase::GreetingPlaying) {
                    self.emit_greeting_tts_complete();
                    // Reset pipeline anchors (tts_first_audio, tts_start, etc.)
                    // that were set during the greeting. Without this, Turn 1's
                    // user_agent_latency_ms would be computed as
                    //   vad_speech_ended (after greeting) → tts_first_audio (during greeting)
                    // which is a negative duration — saturating to 0ms in release,
                    // panicking in debug. cancel_turn() resets the pipeline without
                    // emitting a spurious TurnEnded since no turn is currently open.
                    self.tracer.cancel_turn();
                    self.session_phase = SessionPhase::Active;
                    let remaining = self.playback.remaining_playback();
                    self.playback.reset();
                    self.start_idle_timer_after_greeting_with_offset(remaining);
                    info!("[reactor] Greeting complete");
                    return;
                }

                self.tracer.mark_tts_finished();

                // Compute output audio duration from total bytes sent.
                // Output is mono PCM16 at output_sample_rate.
                let bytes_per_ms = self.config.output_sample_rate as f64 * 2.0 / 1000.0;
                if bytes_per_ms > 0.0 {
                    let output_duration_ms = self.playback.total_bytes() as f64 / bytes_per_ms;
                    self.tracer.set_output_audio_duration(output_duration_ms);
                }

                // Finish turn: emit TtsComplete + TurnMetrics + TurnEnded
                self.tracer.finish_turn(
                    false,
                    &self.config.tts_provider,
                    &self.config.tts_model,
                    &self.config.voice_id,
                );

                let remaining = self.playback.remaining_playback();
                self.playback.reset();

                // If the agent requested hang_up, delay shutdown until the
                // client has had time to finish playing the goodbye audio.
                if matches!(self.hang_up, HangUpPhase::WaitingForTts) {
                    let delay = remaining;
                    if delay.is_zero() {
                        self.finalize_hang_up("TTS finished after hang_up (no playback remaining)");
                    } else {
                        info!(
                            "[reactor] hang_up: delaying shutdown by {:?} for client playback",
                            delay
                        );
                        let key = self.timers.insert(TimerKey::HangUpDelay, delay);
                        self.hang_up = HangUpPhase::WaitingForDelay { timer_key: key };
                    }
                    return;
                }

                // Hook: check if the hang-up classifier finished and said YES
                // (`hang_up` was set to WaitingForTts by the hook verdict handler).
                // This path is identical — the enum collapses the two sources into one.
                // (Handled above: WaitingForTts covers both primary and hook sources.)

                self.start_idle_timer_with_offset(remaining);
            }
            TtsEvent::Error(reason) => {
                warn!(
                    "[reactor] TTS connection error: {} — cleaning up turn",
                    reason
                );
                self.tts.mark_finished();
                self.playback.reset();
                // Treat like LlmEvent::Error: arm the idle timer so the session
                // doesn't stall silently after a TTS connection failure.
                self.start_idle_timer();
            }
        }
    }

    // ── Pipeline control ─────────────────────────────────────────

    /// Start an LLM generation with the backend's internal conversation.
    pub(super) async fn start_llm_turn(&mut self) {
        // Reset pipeline phase — single assignment replaces 4 field clears.
        self.llm_turn = LlmTurnPhase::Generating {
            buf: if self.config.turn_completion_enabled {
                Some(String::new())
            } else {
                None
            },
        };
        self.turn_text_len = 0;
        self.turn_spoken_text.clear();
        self.playback.reset();

        // Disarm any orphaned TurnCompletion or UserIdle timer key.
        // A new LLM turn supersedes any pending ○/◐ wait — the stale
        // timer would otherwise inject a surprise nudge on top of this turn.
        //
        // Use disarm() not cancel(): if this LLM turn IS the nudge delivery
        // (started from the UserIdle handler), the idle_retries count must
        // be preserved so the next silence correctly advances toward hang-up.
        self.reengagement.disarm(&mut self.timers);

        // BargeInGate check: if speech was pending while idle, the gate fires
        // here and cancels the whole turn before LLM starts. Early-return so
        // we don't start the LLM stream immediately after cancelling it.
        if self.start_tts_for_turn() {
            return;
        }
        self.tracer.mark_tts_start();
        info!("[reactor] Starting LLM stream...");
        match self.llm.start().await {
            Ok(()) => {
                info!("[reactor] LLM stream started OK");
            }
            Err(e) => {
                warn!("[reactor] LLM start failed: {}", e);
                self.tts.cancel();
                self.start_idle_timer();
            }
        }
    }

    /// Cancel the active LLM + TTS pipeline instantly.
    ///
    /// This is the entire barge-in implementation:
    /// - `self.llm.cancel()` cancels the LLM stream + in-flight tool tasks
    /// - `self.tts.cancel()` drops the token channel → TTS batch task exits
    /// - All timer keys removed from `DelayQueue`
    ///
    /// The backend handles its own cleanup (placeholder tool results, etc).
    pub(super) fn cancel_pipeline(&mut self) {
        // Cancel returns an optional LlmComplete event for the partial
        // generation — forward it to the event bus before resetting.
        if let Some(LlmEvent::LlmComplete(data)) = self.llm.cancel() {
            self.tracer.emit(Event::LlmComplete(data));
        }
        self.tts.cancel();
        // If a barge-in fires during the greeting, mark session as Active so
        // the TtsFinished handler doesn't re-trigger the greeting idle timer.
        if matches!(self.session_phase, SessionPhase::GreetingPlaying) {
            self.session_phase = SessionPhase::Active;
        }

        // Notify hooks of barge-in before clearing state.
        self.notify_hooks(|p| p.on_barge_in());

        // Tell subscribers to stop playback and flush audio buffers
        self.tracer.emit(Event::Interrupt);

        if let HangUpPhase::WaitingForDelay { ref timer_key } = self.hang_up {
            self.timers.remove(timer_key);
        }

        // Emit orphaned events for in-flight side-effect tools so
        // operators can audit writes that outlive the session.
        let action = self.se_guard.cancel();
        let orphaned = match action {
            SeGuardAction::Orphaned {
                cancel_key,
                tool_ids,
            } => {
                if let Some(k) = cancel_key {
                    self.timers.remove(&k);
                }
                tool_ids
            }
            _ => std::collections::HashMap::new(),
        };
        if !orphaned.is_empty() {
            for (tool_call_id, tool_name) in &orphaned {
                self.tracer.emit(Event::ToolActivity {
                    tool_call_id: Some(tool_call_id.clone()),
                    tool_name: tool_name.clone(),
                    status: "orphaned".to_string(),
                    error_message: None,
                });
            }
            info!(
                "[reactor] {} side-effect tool(s) orphaned by pipeline cancel",
                orphaned.len()
            );
        }

        self.tracer.cancel_turn();

        // Reset pipeline phase (clears completion buffer, suppression state,
        // and any suppressed response) with a single assignment.
        self.llm_turn = LlmTurnPhase::Idle;
        // Reset hang-up phase (including any pending delay key). The old
        // hang_up_delay_key was removed from the DelayQueue above.
        self.hang_up = HangUpPhase::Idle;
        self.turn_text_len = 0;
        self.turn_spoken_text.clear();

        // NOTE: turn_phase is NOT reset here. The dispatch function
        // (`dispatch_turn_event`) is the exclusive owner of turn_phase writes.
        //
        // Safe for every caller:
        //
        // 1. execute_turn_action(CancelPipeline) — barge-in path: the state
        //    machine already set turn_phase = BargeInWordCount. Resetting here
        //    would clobber it and silently drop the pending transcript.
        //
        // 2. start_tts_for_turn() / start_llm_turn() — only reachable via
        //    commit_turn_text(). The coordinator already executed CommitTurn/
        //    CommitBargeIn actions, which run AFTER turn_phase = Listening is
        //    committed (line 847 of dispatch_turn_event). So turn_phase is
        //    Listening when start_tts_for_turn()'s is_speaking() check runs —
        //    that branch can never be taken at this point.
        //
        // 3. Shutdown paths (should_exit, transport close, all-channels-closed,
        //    initiate_shutdown): the Reactor is dropped immediately after —
        //    timer key cleanup is unnecessary because the DelayQueue is freed
        //    with the struct.

        // Reset re-engagement state (retry count, any armed timer).
        self.reengagement.cancel(&mut self.timers);
        self.bot_audio_sent = false;
        info!("[reactor] Pipeline cancelled");
    }

    /// Initiate a clean session shutdown (agent-initiated hang-up or escalation).
    ///
    /// 1. Emits `SessionEnded` for all subscribers (UI, OTel, etc.)
    /// 2. Cancels the active pipeline (LLM + TTS)
    /// 3. If a call escalation is pending, sends `TransportCommand::Transfer` first.
    /// 4. Tells the transport layer to close (triggers telephony REST hangup)
    /// 5. Sets `should_exit` to break the reactor loop on the next iteration
    pub(super) fn initiate_shutdown(&mut self, reason: &str) {
        info!("[reactor] Initiating shutdown: {}", reason);
        self.notify_hooks(|p| p.on_session_end());
        if !self.session_ended_emitted {
            self.tracer.emit(Event::SessionEnded);
            self.session_ended_emitted = true;
        }
        self.cancel_pipeline();
        self.transport.close();
        self.should_exit = true;
    }

    /// Actually tear down the session OR initiate a transfer if one is pending.
    /// Called when the HangUpPhase has completed waiting for the client to drain.
    pub(super) fn finalize_hang_up(&mut self, reason: &str) {
        if let Some(dest) = self.pending_transfer_destination.take() {
            info!("[reactor] Escalation: transferring call to {}", dest);
            self.transport.transfer(dest);
            // Do NOT shut down. The transport will report TransferResult via control_rx.
            // If it succeeds, reactor terminates. If it fails, session resumes.
            self.holding = true;
        } else {
            self.initiate_shutdown(reason);
        }
    }

    // ── TTS turn management ───────────────────────────────────────

    /// Start TTS for a new turn — reuses session-level WS connection when
    /// available, otherwise builds a fresh HTTP provider.
    ///
    /// Also advances `tts_cursor` to wall-clock position so that
    /// silence since the last turn is correctly encoded in recordings.
    ///
    /// Returns `true` if the BargeInGate fired and the turn was cancelled.
    /// The caller **must** return immediately in that case — the pipeline
    /// has been reset and starting the LLM would undo the cancellation.
    pub(super) fn start_tts_for_turn(&mut self) -> bool {
        // ── TTS start guard: don't start while user is speaking ──────────
        // The TTS output gate drops audio while UserSpeaking, but we also
        // avoid starting the TTS stage entirely to save resources.
        if self.turn_phase.is_speaking() {
            info!("[reactor] start_tts_for_turn: user is speaking — cancelling turn");
            self.cancel_pipeline();
            return true;
        }

        self.tts_cursor.begin_turn();

        if let Some(ref cmd_tx) = self.ws_tts_cmd_tx {
            // WS path: the reactor's select! arm feeds audio directly into
            // TtsStage via push_ws_chunk() — no relay task, no mutex needed.
            let handle = WsTtsHandle {
                cmd_tx: cmd_tx.clone(),
            };
            self.tts.start_ws(handle);
        } else {
            // HTTP path: build a fresh provider per turn.
            let tts_mode = build_tts_provider(&self.tts_provider_config);
            match tts_mode {
                TtsMode::Http(provider) => {
                    self.tts.start_http(provider, self.config.voice_id.clone());
                }
                TtsMode::Streaming(_) => {
                    // WS provider that failed to connect at session init;
                    // we logged the error there, so log a brief reminder.
                    warn!("[reactor] WS TTS unavailable — turn will be silent");
                }
            }
        }
        false
    }

    // ── Greeting ─────────────────────────────────────────────────

    /// Synchronously plays the greeting via HTTP TTS (WS path is handled
    /// non-awaited from `start()` via the `greeting_in_progress` flag).
    pub(super) async fn play_greeting_http(&mut self, greeting: &str) {
        self.set_state(SessionState::Speaking);
        self.llm.add_assistant_message(greeting.to_string());
        // Greeting is spoken to the user and must appear in transcript timeline
        // so UI highlighting and recording transcript offsets stay aligned.
        if !greeting.trim().is_empty() {
            self.tracer.emit(Event::Transcript {
                role: "assistant".into(),
                text: greeting.to_string(),
            });
        }

        // HTTP path: one-shot synthesis.
        let tts_mode = build_tts_provider(&self.tts_provider_config);
        let synth_started_at = std::time::Instant::now();
        if let TtsMode::Http(mut provider) = tts_mode {
            if let Some(pcm) = provider
                .synthesize_chunk(greeting, &self.config.voice_id)
                .await
            {
                self.playback.record(pcm.len());
                // HTTP greeting bypasses start_tts_for_turn() entirely: the provider
                // is built inline and synthesis completes synchronously (awaited) before
                // we emit. start_tts_for_turn() is not called here because there is no
                // TtsStage pipeline to set up — this is a one-shot synthesize_chunk().
                //
                // begin_turn() snaps the cursor to the current wall-clock position,
                // which already includes the synthesis latency. This is correct:
                // the greeting chunk ends at "now", so placing it starting from
                // the wall-clock position reflects reality. Note that start_tts_for_turn()
                // also calls begin_turn() for regular turns — there is no double-call
                // here because this code path is mutually exclusive with that function.
                self.tts_cursor.begin_turn();
                let offset_samples = self.tts_cursor.stamp(pcm.len());
                self.tracer.emit(Event::AgentAudio {
                    pcm: Bytes::from(pcm),
                    sample_rate: self.config.output_sample_rate,
                    offset_samples,
                });
                self.bot_audio_sent = true;

                let cleaned = self.strip_turn_markers(greeting);
                if !cleaned.is_empty() {
                    self.tracer.emit(Event::TtsComplete {
                        provider: self.config.tts_provider.clone(),
                        model: self.config.tts_model.clone(),
                        text: cleaned.clone(),
                        voice_id: self.config.voice_id.clone(),
                        character_count: cleaned.chars().count(),
                        duration_ms: synth_started_at.elapsed().as_secs_f64() * 1000.0,
                        ttfb_ms: None,
                        text_aggregation_ms: None,
                    });
                }
            }
        }

        self.set_state(SessionState::Listening);
        let remaining = self.playback.remaining_playback();
        self.playback.reset();
        self.start_idle_timer_after_greeting_with_offset(remaining);
        info!("[reactor] Greeting complete");
    }

    /// Kick off the greeting non-awaited via the WS TTS path.
    ///
    /// Called from `start()` before `run()` enters the select! loop.
    /// The run() WS audio arm and on_tts_event(Finished) handle the rest.
    pub(super) fn initiate_greeting_ws(&mut self, greeting: &str) {
        self.set_state(SessionState::Speaking);
        self.llm.add_assistant_message(greeting.to_string());
        // Keep transcript/event stream consistent with the spoken opening greeting.
        if !greeting.trim().is_empty() {
            self.tracer.emit(Event::Transcript {
                role: "assistant".into(),
                text: greeting.to_string(),
            });
        }
        self.greeting_observability = Some(super::GreetingObservability {
            text: self.strip_turn_markers(greeting),
            started_at: std::time::Instant::now(),
            first_audio_at: None,
        });
        self.start_tts_for_turn();
        self.tts.feed_token(greeting);
        self.tts.flush();
    }

    fn emit_greeting_tts_complete(&mut self) {
        let Some(obs) = self.greeting_observability.take() else {
            return;
        };
        if obs.text.trim().is_empty() {
            return;
        }
        let duration_ms = obs.started_at.elapsed().as_secs_f64() * 1000.0;
        let ttfb_ms = obs
            .first_audio_at
            .map(|t| t.duration_since(obs.started_at).as_secs_f64() * 1000.0);
        self.tracer.emit(Event::TtsComplete {
            provider: self.config.tts_provider.clone(),
            model: self.config.tts_model.clone(),
            text: obs.text.clone(),
            voice_id: self.config.voice_id.clone(),
            character_count: obs.text.chars().count(),
            duration_ms,
            ttfb_ms,
            text_aggregation_ms: None,
        });
    }

    /// Shared helper for ○/◐ marker handling.
    ///
    /// Traces, logs, cancels TTS, and arms the `TurnCompletion` timer.
    /// Callers add context-specific behaviour (e.g. `suppressing_llm_output`
    /// during streaming, `compute_and_emit_state` on late detection).
    fn arm_turn_completion_wait(&mut self, marker: TurnCompletion) {
        let is_short = marker == TurnCompletion::IncompleteShort;
        let label = if is_short { "○ SHORT" } else { "◐ LONG" };
        self.tracer.trace(if is_short {
            "TurnIncompleteShort"
        } else {
            "TurnIncompleteLong"
        });
        info!(
            "[reactor] Turn completion: {} — waiting {}s",
            label,
            marker.timeout_secs()
        );
        self.tts.cancel();
        let dur = std::time::Duration::from_secs(marker.timeout_secs());
        // arm_turn_completion() atomically: cancels existing TurnCompletion or
        // UserIdle, inserts a fresh timer, and records is_short — all in one call.
        self.reengagement
            .arm_turn_completion(&mut self.timers, dur, is_short);
    }

    /// Preserve a suppressed ○/◐ response for later replay.
    ///
    /// Strips turn markers, adds the clean text to conversation history
    /// (so the nudge LLM has context), and stores it in
    /// `suppressed_turn_response` for the TurnCompletion timer to replay.
    ///
    /// `add_assistant_message` is called exactly once per turn:
    /// - Path 1 (`suppressing_llm_output`): streaming tokens are swallowed by
    ///   the token arm and never added to history; this is the only add call.
    /// - Path 2 (late TurnCompletion detection): only reached when
    ///   `suppressing_llm_output` is false — the two paths are mutually
    ///   exclusive (path 1 returns early at the top of the Finished arm).
    ///
    /// Neither path adds twice.
    fn save_suppressed_response(&mut self, content: &Option<String>) {
        if let Some(ref text) = content {
            let clean = self.strip_turn_markers(text);
            if !clean.is_empty() {
                info!(
                    "[reactor] Suppressed response saved for replay: {:?}",
                    clean
                );
                self.llm.add_assistant_message(clean.clone());
                // Store inside the Suppressing variant so it's guaranteed
                // to be present when the TurnCompletion timer fires.
                self.llm_turn = LlmTurnPhase::Suppressing {
                    response: Some(clean),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_token_suppressing_drops() {
        let mut phase = LlmTurnPhase::Suppressing { response: None };
        let action = phase.process_token("hello", true);
        assert!(matches!(action, TokenAction::Drop));
    }

    #[test]
    fn process_token_accumulates() {
        let mut phase = LlmTurnPhase::Generating {
            buf: Some(String::new()),
        };
        // "a" (1 byte) stays below the 4-byte threshold.
        let action = phase.process_token("a", true);
        assert!(matches!(action, TokenAction::Accumulate));
        // "bc" brings the buffer to "abc" (3 bytes) — still below threshold.
        let action = phase.process_token("bc", true);
        assert!(matches!(action, TokenAction::Accumulate));
        // "d" brings the buffer to "abcd" (4 bytes) — threshold crossed.
        // Must run the marker check now, not accumulate further.
        let action = phase.process_token("d", true);
        assert!(
            matches!(action, TokenAction::MarkerCheckCompleted { .. }),
            "4th byte must trigger the marker check, not further accumulation"
        );
        assert!(
            phase.marker_checked(),
            "marker should be marked checked after threshold"
        );
    }

    #[test]
    fn process_token_detects_complete() {
        // "✓" is 3 bytes in UTF-8.
        let mut phase = LlmTurnPhase::Generating {
            buf: Some("✓".to_string()),
        };
        let action = phase.process_token(" hello", true);
        match action {
            TokenAction::MarkerCheckCompleted {
                marker,
                emitted_text,
                ..
            } => {
                assert_eq!(marker, Some(TurnCompletion::Complete));
                assert_eq!(emitted_text, "hello");
            }
            _ => panic!("Expected MarkerCheckCompleted"),
        }
        assert!(
            phase.marker_checked(),
            "marker_checked() should be true after marker scan"
        );
    }

    #[test]
    fn process_token_passthrough_when_disabled() {
        // When turn_completion_enabled=false, the Generating guard condition fails
        // and we fall through to the `_` arm regardless of whether buf is Some or None.
        let mut phase = LlmTurnPhase::Generating {
            buf: Some("".to_string()),
        };
        let action = phase.process_token("a", false);
        match action {
            TokenAction::Passthrough { token } => assert_eq!(token, "a"),
            _ => panic!("Expected Passthrough"),
        }
    }

    #[test]
    fn process_late_flush_detects_marker() {
        // "○" is 3 bytes in UTF-8
        let mut phase = LlmTurnPhase::Generating {
            buf: Some("○".to_string()),
        };
        let action = phase.process_late_flush(true);
        match action {
            Some(TokenAction::MarkerCheckCompleted { marker, .. }) => {
                assert_eq!(marker, Some(TurnCompletion::IncompleteShort));
            }
            _ => panic!("Expected Some(MarkerCheckCompleted)"),
        }
        assert!(
            phase.marker_checked(),
            "marker_checked() should be true after late flush"
        );
    }

    #[test]
    fn process_late_flush_returns_none_when_disabled() {
        let mut phase = LlmTurnPhase::Generating {
            buf: Some("○".to_string()),
        };
        let action = phase.process_late_flush(false);
        assert!(
            action.is_none(),
            "should return None when turn_completion_enabled=false"
        );
    }

    #[test]
    fn process_late_flush_returns_none_when_already_checked() {
        let mut phase = LlmTurnPhase::Generating { buf: None }; // buf=None means already checked
        let action = phase.process_late_flush(true);
        assert!(
            action.is_none(),
            "should return None when marker already checked"
        );
    }
}
