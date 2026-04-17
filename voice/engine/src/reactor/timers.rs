//! Timer handlers, idle timer management, and re-engagement state machine.
//!
//! All timers flow through a single `DelayQueue<TimerKey>` — no spawned tasks,
//! no generation counters. Removing a key from the queue is an O(1) cancellation.
//!
//! [`ReengagementTimer`] lives here (rather than in a separate file) because
//! `timers.rs` is its only consumer. The two concerns are tightly coupled:
//! every timer arm/fire call in `on_timer` delegates directly to it.

use std::time::Duration;

use tokio_util::time::{delay_queue, DelayQueue};
use tracing::{debug, info, warn};

use super::policies::should_nudge_idle;
use crate::types::{TimerKey, TurnCompletion};

use super::session::{HangUpPhase, LlmTurnPhase};
use super::turn_phase::TurnEvent;
use super::Reactor;

// ── Re-engagement timer state machine ─────────────────────────────────────────
//
// Encapsulates the two timers that re-engage the user after silence:
//
// - **`TurnCompletion`** — LLM emitted ○/◐. Wait N seconds then inject a
//   contextual nudge ("you were cut off" / "still thinking?").
// - **`UserIdle`** — user silent after a normal turn. Wait M seconds then
//   inject a generic "are you still there?" check-in.
//
// ## Invariant
//
// Only one timer can be active at a time. `TurnCompletion` has higher priority
// — `arm_idle()` is a no-op while `TurnCompletion` is armed. This is enforced
// **structurally**: both timer states are distinct variants of `TimerState`,
// so they cannot coexist.
//
// ## Retry count
//
// `idle_retries` persists across the fire → re-arm cycle. When `UserIdle`
// fires and the reactor calls `start_idle_timer()` again (after the nudge
// TTS finishes), `arm_idle()` carries the existing count forward. Only
// `cancel()` resets it to zero — which happens when the user speaks or
// `cancel_pipeline()` is called. This ensures the hang-up gate ("hang up
// after N idle nudges") actually triggers.

#[derive(Debug, Default)]
enum TimerState {
    #[default]
    Idle,
    TurnCompletion {
        key: delay_queue::Key,
        /// `true` → ○ (short, "cut off"); `false` → ◐ (long, "deliberating").
        is_short: bool,
    },
    UserIdle {
        key: delay_queue::Key,
    },
}

/// Re-engagement timer state machine.
///
/// Holds at most one `DelayQueue` key at a time. `TurnCompletion` has strict
/// priority over `UserIdle` — `arm_idle()` is a no-op while `TurnCompletion`
/// is armed. `idle_retries` survives timer-fire/re-arm cycles so the hang-up
/// gate fires correctly, and resets only on explicit `cancel()`.
#[derive(Debug, Default)]
pub(in crate::reactor) struct ReengagementTimer {
    state: TimerState,
    /// Number of times `UserIdle` has fired without the user speaking.
    /// Persists across re-arm cycles; reset only when the user speaks or
    /// `cancel_pipeline()` is called.
    idle_retries: u32,
    /// Wall-clock boundary before which the user is considered "on hold".
    /// Dynamically extends the `UserIdle` timer window calculated in `start_idle_timer_inner`.
    pub(super) hold_until: Option<std::time::Instant>,
}

impl ReengagementTimer {
    // ── Arming ────────────────────────────────────────────────────────────

    /// Arm a `TurnCompletion` timer.
    ///
    /// Cancels whatever is currently armed (including a prior `TurnCompletion`
    /// if called twice — prevents DelayQueue key leaks).
    ///
    /// **`idle_retries` is intentionally preserved.** If `TurnCompletion` fires,
    /// the bot injects a nudge, and the user goes silent again, the existing count
    /// continues accumulating toward the hang-up gate. Resetting here would let
    /// the caller loop through ○/◐ indefinitely without ever hanging up.
    pub(super) fn arm_turn_completion(
        &mut self,
        timers: &mut DelayQueue<TimerKey>,
        duration: Duration,
        is_short: bool,
    ) {
        self.disarm(timers);
        let key = timers.insert(TimerKey::TurnCompletion, duration);
        self.state = TimerState::TurnCompletion { key, is_short };
    }

    /// Arm a `UserIdle` timer.
    ///
    /// **No-ops if `TurnCompletion` is currently armed** — `TurnCompletion`
    /// owns the re-engagement slot and fires a contextually richer nudge.
    ///
    /// If `UserIdle` is already armed (re-arm after a nudge), the stale key
    /// is removed and a fresh one is inserted, preserving `idle_retries`.
    /// Returns `true` if the timer was armed, `false` if suppressed.
    pub(super) fn arm_idle(
        &mut self,
        timers: &mut DelayQueue<TimerKey>,
        duration: Duration,
    ) -> bool {
        match &self.state {
            // Priority rule encoded as a match arm — no external guard needed.
            TimerState::TurnCompletion { .. } => false,

            TimerState::UserIdle { key } => {
                // Re-arm: cancel stale key, preserve idle_retries.
                timers.remove(key);
                let new_key = timers.insert(TimerKey::UserIdle, duration);
                self.state = TimerState::UserIdle { key: new_key };
                true
            }

            TimerState::Idle => {
                // Fresh arm — idle_retries carries over from last cycle (or 0 if cancel()ed).
                let key = timers.insert(TimerKey::UserIdle, duration);
                self.state = TimerState::UserIdle { key };
                true
            }
        }
    }

    // ── Cancellation ──────────────────────────────────────────────────────

    /// Cancel whatever is currently armed **and** reset `idle_retries`.
    ///
    /// Call only when the user re-engages (speaks, barges in, or
    /// `cancel_pipeline` runs). These events mean the user is back —
    /// the nudge counter should start fresh on the next silence.
    ///
    /// For cancelling a timer without losing the nudge count
    /// (e.g. before starting the nudge LLM turn itself), use `disarm()`.
    pub(super) fn cancel(&mut self, timers: &mut DelayQueue<TimerKey>) {
        self.disarm(timers);
        self.idle_retries = 0;
        self.hold_until = None;
    }

    /// Cancel the current timer key without resetting `idle_retries`.
    ///
    /// Use this when starting the LLM turn that *delivers* a nudge — the
    /// turn is a consequence of the idle system, not a user re-engagement,
    /// so the nudge count must survive to the next `arm_idle()` call.
    /// Also used internally before re-arming (e.g. `arm_turn_completion`).
    pub(super) fn disarm(&mut self, timers: &mut DelayQueue<TimerKey>) {
        match &self.state {
            TimerState::TurnCompletion { key, .. } | TimerState::UserIdle { key } => {
                timers.remove(key);
            }
            TimerState::Idle => {}
        }
        self.state = TimerState::Idle;
    }

    // ── Timer-fire helpers ────────────────────────────────────────────────

    /// Called when `TimerKey::TurnCompletion` fires.
    ///
    /// Transitions to `Idle` and returns `Some(is_short)` so the handler
    /// can select the right nudge prompt. Returns `None` if the key was
    /// stale (already cancelled) — discard.
    pub(super) fn on_turn_completion_fired(&mut self) -> Option<bool> {
        match self.state {
            TimerState::TurnCompletion { is_short, .. } => {
                self.state = TimerState::Idle;
                Some(is_short)
            }
            _ => None,
        }
    }

    /// Called when `TimerKey::UserIdle` fires.
    ///
    /// Increments `idle_retries` (persisted on struct), transitions to `Idle`,
    /// and returns the new count for the hang-up gate / logging.
    /// Returns `None` if the key was stale — discard.
    pub(super) fn on_user_idle_fired(&mut self) -> Option<u32> {
        match self.state {
            TimerState::UserIdle { .. } => {
                self.idle_retries += 1;
                self.state = TimerState::Idle;
                Some(self.idle_retries)
            }
            _ => None,
        }
    }

    // ── Queries ───────────────────────────────────────────────────────────

    /// Current idle retry count. Useful for logging / observability.
    pub(super) fn idle_retries(&self) -> u32 {
        self.idle_retries
    }
}

// ── Reactor timer dispatch ────────────────────────────────────────────────────

impl Reactor {
    pub(super) async fn on_timer(&mut self, key: TimerKey) {
        match key {
            TimerKey::EndOfTurnFallback => {
                info!("[reactor] EoT fallback — dispatching to turn_phase");
                self.dispatch_turn_event(TurnEvent::EotFallback).await;
            }

            TimerKey::SttTimeout => {
                debug!("[reactor] STT timeout — no transcript arrived (noise-only utterance)");
                self.tracer.trace("SttTimeout");
                self.dispatch_turn_event(TurnEvent::SttTimeout).await;
            }

            TimerKey::TurnCompletion => {
                self.tracer.trace("TurnCompletionTimeout");

                // on_turn_completion_fired() transitions to Idle and returns
                // Some(is_short). Returns None if the key was stale (already
                // cancelled) — in that case, nothing to do.
                let Some(is_short) = self.reengagement.on_turn_completion_fired() else {
                    return;
                };

                // Skip if user started speaking or pipeline is active
                if self.is_pipeline_active() {
                    info!("[reactor] TurnCompletion timer fired but pipeline active — skipping");
                    return;
                }

                // If a suppressed response exists, replay it through TTS instead
                // of starting a new generic nudge LLM call.
                let suppressed =
                    if let LlmTurnPhase::Suppressing { ref mut response } = self.llm_turn {
                        response.take()
                    } else {
                        None
                    };
                if let Some(text) = suppressed {
                    self.llm_turn = LlmTurnPhase::Idle;
                    info!(
                        "[reactor] TurnCompletion timer — replaying suppressed response: {:?}",
                        text
                    );
                    // The suppressed text was already added to conversation history
                    // in the LlmEvent::Finished handler, so the LLM context is correct.
                    if self.start_tts_for_turn() {
                        return;
                    }
                    self.tracer.emit(voice_trace::Event::Transcript {
                        role: "assistant".into(),
                        text: text.clone(),
                    });
                    self.send_tts_token(&text);
                    self.tts.flush();
                    return;
                }

                // No suppressed response — fall back to the standard nudge.
                // Explicitly require ✓ prefix to prevent infinite ○/◐ loops.
                info!(
                    "[reactor] TurnCompletion timer — injecting nudge (is_short={}, idle_retries={})",
                    is_short,
                    self.reengagement.idle_retries(),
                );
                let nudge = TurnCompletion::nudge_prompt(is_short);
                self.llm.add_user_message(nudge.to_string());
                self.start_llm_turn().await;
                // Skip turn completion detection on nudge turns — the LLM may
                // still output ○/◐ despite the prompt saying "MUST begin with ✓".
                // start_llm_turn() set llm_turn = Generating { buf: Some(..) };
                // setting buf to None marks the check as already done.
                if let LlmTurnPhase::Generating { ref mut buf } = self.llm_turn {
                    *buf = None;
                }
            }

            TimerKey::UserIdle => {
                self.tracer.trace("UserIdle");

                // on_user_idle_fired() increments retry_count, transitions to
                // Idle, and returns the new count. Returns None if stale.
                let Some(retry_count) = self.reengagement.on_user_idle_fired() else {
                    return;
                };

                if !should_nudge_idle(
                    self.is_pipeline_active(),
                    self.config.idle_timeout_secs,
                    retry_count,
                    self.config.idle_max_nudges,
                ) {
                    if self.is_pipeline_active() {
                        info!("[reactor] UserIdle timer fired but pipeline active — skipping");
                    } else if retry_count > self.config.idle_max_nudges
                        && self.config.idle_max_nudges > 0
                    {
                        info!(
                            "[reactor] UserIdle nudges exhausted ({}/{}) — hanging up",
                            retry_count, self.config.idle_max_nudges
                        );
                        self.initiate_shutdown("idle nudges exhausted");
                    }
                    return;
                }

                info!(
                    "[reactor] UserIdle nudge ({}/{})",
                    retry_count, self.config.idle_max_nudges
                );
                self.emit_agent_event("user_idle");
                let nudge = "The user has been silent for a while. \
                    Check in with them naturally — ask if they need help \
                    or are still there. Be brief and warm.";
                self.llm.add_user_message(nudge.to_string());
                self.start_llm_turn().await;
                // Same as TurnCompletion nudge — bypass marker detection.
                // start_llm_turn() set llm_turn = Generating { buf: Some(..) };
                // setting buf to None marks the check as already done.
                if let LlmTurnPhase::Generating { ref mut buf } = self.llm_turn {
                    *buf = None;
                }
            }

            TimerKey::SideEffectTimeout => {
                // on_timeout() marks the window as expired and returns the count +
                // deferred transcript. It does NOT clear tool_ids — cancel_pipeline()
                // below calls se_guard.cancel() which drains tool_ids and emits
                // ToolActivity { status: "orphaned" } for each in-flight tool.
                let (in_flight, deferred) = self.se_guard.on_timeout();
                if in_flight > 0 {
                    warn!(
                        "[reactor] Side-effect tool timeout ({} still in flight) — forcing protection window closed",
                        in_flight
                    );

                    if let Some(text) = deferred {
                        info!(
                            "[reactor] Processing deferred transcript after tool timeout: '{}'",
                            text
                        );
                        self.cancel_pipeline(); // emits orphaned events, resets all state
                        self.llm.add_user_message(text.clone());
                        self.tracer.emit(voice_trace::Event::Transcript {
                            role: "user".into(),
                            text,
                        });
                        self.start_llm_turn().await;
                    } else {
                        self.cancel_pipeline(); // emits orphaned events, resets all state
                    }
                }
            }

            TimerKey::HangUpDelay => {
                // Clear the WaitingForDelay key — shutdown is now imminent.
                if let HangUpPhase::WaitingForDelay { .. } = self.hang_up {
                    self.hang_up = HangUpPhase::Idle;
                }
                info!("[reactor] HangUpDelay expired — client playback should be done");
                self.finalize_hang_up("hang_up playback delay expired");
            }
        }
    }

    /// (Re)start the user-idle detection timer.
    ///
    /// No-ops if: idle_timeout_secs is 0, the user hasn't interacted yet,
    /// or a TurnCompletion timer is currently armed (enforced by ReengagementTimer).
    pub(super) fn start_idle_timer(&mut self) {
        self.start_idle_timer_inner(Duration::ZERO, false);
    }

    /// Start idle timer immediately after greeting playback.
    ///
    /// Unlike `start_idle_timer`, this bypasses the `session_phase` guard —
    /// the greeting has already played, so we want to detect silence even
    /// before the caller has spoken for the first time.
    pub(super) fn start_idle_timer_after_greeting(&mut self) {
        self.start_idle_timer_inner(Duration::ZERO, true);
    }

    /// `start_idle_timer_after_greeting` with an extra playback offset.
    ///
    /// Pass `playback.remaining_playback()` so the idle window starts after
    /// the client has finished playing the greeting, not when the server
    /// finishes receiving it from TTS.
    pub(super) fn start_idle_timer_after_greeting_with_offset(&mut self, offset: Duration) {
        self.start_idle_timer_inner(offset, true);
    }

    /// Start idle timer with an extra offset (e.g. client playback duration).
    pub(super) fn start_idle_timer_with_offset(&mut self, offset: Duration) {
        self.start_idle_timer_inner(offset, false);
    }

    fn start_idle_timer_inner(&mut self, offset: Duration, force: bool) {
        if self.config.idle_timeout_secs == 0 {
            return;
        }
        // After the greeting we want idle detection even before first speech.
        // In all other contexts, don't start until the session is Active or the
        // greeting is playing (Nascent suppresses the timer entirely).
        if !force && !self.session_phase.allows_idle_timer() {
            return;
        }
        let mut dur = Duration::from_secs(self.config.idle_timeout_secs as u64) + offset;

        // If the user called `on_hold`, push the idle window up to the hold barrier.
        if let Some(hold_time) = self.reengagement.hold_until {
            if let Some(remaining) = hold_time.checked_duration_since(std::time::Instant::now()) {
                dur += remaining;
            } else {
                // Hold time expired in the past, clear it.
                self.reengagement.hold_until = None;
            }
        }

        // arm_idle() is a no-op when TurnCompletion is armed — the invariant
        // is enforced inside ReengagementTimer, not here.
        self.reengagement.arm_idle(&mut self.timers, dur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::time::DelayQueue;

    fn make_timers() -> DelayQueue<TimerKey> {
        DelayQueue::new()
    }

    // NOTE: `#[tokio::test]` is required even though these tests have no `.await`
    // calls. `DelayQueue::new()` registers a tokio time handle internally and
    // panics if no runtime is present.
    #[tokio::test]
    async fn turn_completion_blocks_arm_idle() {
        let mut r = ReengagementTimer::default();
        let mut q = make_timers();
        r.arm_turn_completion(&mut q, Duration::from_secs(5), true);
        let armed = r.arm_idle(&mut q, Duration::from_secs(5));
        assert!(!armed, "UserIdle must not arm when TurnCompletion is armed");
        assert_eq!(q.len(), 1); // only one key in queue
    }

    #[tokio::test]
    async fn cancel_resets_idle_retries_but_disarm_does_not() {
        let mut r = ReengagementTimer::default();
        let mut q = make_timers();
        r.arm_idle(&mut q, Duration::from_secs(5));
        r.on_user_idle_fired(); // retries → 1
        r.arm_idle(&mut q, Duration::from_secs(5));
        r.on_user_idle_fired(); // retries → 2

        // disarm: retries survive
        r.arm_idle(&mut q, Duration::from_secs(5));
        r.disarm(&mut q);
        assert_eq!(r.idle_retries(), 2);

        // cancel: retries reset
        r.arm_idle(&mut q, Duration::from_secs(5));
        r.cancel(&mut q);
        assert_eq!(r.idle_retries(), 0);
    }
}
