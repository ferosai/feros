//! Shared type definitions for the voice pipeline.
//!
//! Contains all enums and structs used across the crate:
//! - Session-level types (`SessionState`, `TurnCompletion`)
//! - Reactor-internal types (`TimerKey`, `SttEvent`, `LlmEvent`, `TtsEvent`, `VadEvent`)
//! - Tool execution types (`ToolCallRequest`, `ToolResult`)

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use voice_trace::LlmCompletionData;

// ── Session Types ────────────────────────────────────────────────
// Visible to the outside world (sent over WebSocket, used in server.rs).

/// Current state of a voice session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Session created but not yet started, or fully closed.
    Idle,
    /// Mic open, waiting for user speech.
    Listening,
    /// STT finalized, waiting for LLM / running RAG.
    Processing,
    /// TTS audio is being streamed to the client.
    Speaking,
    /// LLM requested a tool; execution in progress.
    ToolCalling,
}

/// Turn completion marker detected in the first LLM token.
///
/// - ✓ = user's turn is complete, respond normally
/// - ○ = user was cut off mid-sentence, wait ~5s then nudge
/// - ◐ = user is thinking/deliberating, wait ~10s then check in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnCompletion {
    /// User finished speaking — respond normally.
    Complete,
    /// User cut off mid-sentence — wait briefly, then prompt.
    IncompleteShort,
    /// User needs time to think — wait longer, then check in.
    IncompleteLong,
}

impl TurnCompletion {
    /// Try to parse a turn completion marker from the beginning of a string.
    /// Returns the marker type and how many bytes to skip.
    pub fn detect(text: &str) -> Option<(Self, usize)> {
        let trimmed = text.trim_start();
        let skip_ws = text.len() - trimmed.len();

        if let Some(stripped) = trimmed.strip_prefix('✓') {
            // Also skip a trailing space if present
            let marker_bytes = skip_ws + '✓'.len_utf8();
            let total = if stripped.starts_with(' ') {
                marker_bytes + 1
            } else {
                marker_bytes
            };
            Some((TurnCompletion::Complete, total))
        } else if trimmed.starts_with('○') {
            Some((TurnCompletion::IncompleteShort, skip_ws + '○'.len_utf8()))
        } else if trimmed.starts_with('◐') {
            Some((TurnCompletion::IncompleteLong, skip_ws + '◐'.len_utf8()))
        } else {
            None
        }
    }

    /// Timeout in seconds before re-prompting the LLM.
    pub fn timeout_secs(self) -> u64 {
        match self {
            TurnCompletion::Complete => 0,
            TurnCompletion::IncompleteShort => 3, // ○ mid-sentence pause — 3s already feels long on a call
            TurnCompletion::IncompleteLong => 4,  // ◐ genuine deliberation — 4s before check-in
        }
    }

    /// Re-prompt message sent to the LLM when the timeout expires.
    #[allow(dead_code)]
    pub fn reprompt_message(self) -> &'static str {
        match self {
            TurnCompletion::Complete => "",
            TurnCompletion::IncompleteShort => {
                "The user seems to have been cut off. Gently prompt them to continue."
            }
            TurnCompletion::IncompleteLong => {
                "The user has been quiet for a while. Check in with them — they may still be thinking."
            }
        }
    }

    /// Generate a nudge prompt for re-engaging after an incomplete turn.
    ///
    /// Explicitly requires `✓` prefix to prevent infinite ○/◐ loops.
    ///
    /// `was_short` — true if the original marker was ○ (short), false for ◐ (long).
    pub fn nudge_prompt(was_short: bool) -> &'static str {
        if was_short {
            "The user paused briefly. Generate a brief, natural prompt to encourage \
            them to continue. Your response MUST begin with ✓ followed by your message. \
            Do NOT output ○ or ◐. Be concise (1 sentence max)."
        } else {
            "The user has been quiet for a while. Generate a friendly check-in. \
            Your response MUST begin with ✓ followed by your message. \
            Do NOT output ○ or ◐. Acknowledge they might be thinking, \
            and offer to help when ready. Be brief (1 sentence)."
        }
    }
}

// ── Reactor Types ────────────────────────────────────────────────
// Internal to the reactor's select! loop.

/// Unique timer categories managed by the Reactor's central `DelayQueue`.
///
/// When a timer fires, `delay_queue.next()` yields the associated `TimerKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimerKey {
    /// SmartTurn model took too long to respond — fall back to starting the
    /// STT pipeline immediately.
    EndOfTurnFallback,

    /// LLM emitted a `○` (short) or `◐` (long) turn-completion marker.
    /// On expiry, inject a silent nudge into the conversation.
    TurnCompletion,

    /// User has been silent since last interaction for `idle_timeout_secs`.
    UserIdle,

    /// Safety timer for side-effect tool execution. If a tool hangs,
    /// this fires to unblock the pipeline.
    SideEffectTimeout,

    /// SmartTurn said "turn complete" but STT hasn't returned a transcript yet.
    /// After `stt_p99_latency_ms` the turn is abandoned: state returns to
    /// Listening and any late transcript that drifts in is discarded.
    SttTimeout,

    /// Delay between TTS finishing and actually closing the transport
    /// after an agent-initiated hang-up. Gives the client time to
    /// finish playing the goodbye audio before the call is terminated.
    HangUpDelay,
}

/// Events emitted by STT stage into the Reactor loop.
#[derive(Debug, Clone)]
pub enum SttEvent {
    /// The STT server returned its first text (interim or final) for this utterance.
    /// Used for TTFB tracking: finalize_sent → first_text_received.
    FirstTextReceived,
    /// An interim / partial transcript (not yet finalized).
    /// Emitted by streaming providers (e.g. Deepgram) that support real-time
    /// interim results. The Reactor currently ignores these — they are included
    /// so the type is complete and future consumers can act on them.
    PartialTranscript(String),
    /// A finalized transcript from the STT server.
    Transcript(String),
    /// The STT connection encountered an unrecoverable error.
    /// The Reactor should attempt to reconnect or degrade gracefully.
    Error(String),
}

/// Events emitted by the LLM stage into the Reactor loop.
#[derive(Debug, Clone)]
pub enum LlmEvent {
    /// A raw text token from the LLM stream.
    Token(String),
    /// The entire agentic turn has finished (all tool rounds resolved).
    /// Contains the final assistant text content (if any) for the UI.
    Finished { content: Option<String> },
    /// A tool execution has started (for UI updates).
    ToolCallStarted {
        id: String,
        name: String,
        side_effect: bool,
    },
    /// A tool execution has completed (for UI updates).
    ToolCallCompleted {
        id: String,
        name: String,
        success: bool,
        error_message: Option<String>,
    },
    /// A recoverable error (e.g. network glitch) — reactor can retry.
    Error(String),
    /// The agent has decided to end the call.
    HangUp {
        reason: String,
        content: Option<String>,
    },
    /// The agent has requested call escalation / human handoff.
    ///
    /// The reactor should drain TTS, then send `TransportCommand::Transfer { destination }`
    /// to the telephony transport before closing the session.
    EscalateCall {
        /// E.164 phone number or SIP URI to forward the call to.
        destination: String,
        /// Human-readable reason (for telemetry/logs).
        reason: String,
    },
    /// The user asked to hold / pause — suppress idle shutdown until they return.
    OnHold { duration_secs: u32 },
    /// An LLM generation has completed (for observability).
    /// Forwarded to the event bus as `Event::LlmComplete`.
    LlmComplete(LlmCompletionData),
}

/// Events emitted by the TTS stage into the Reactor loop.
#[derive(Debug, Clone)]
pub enum TtsEvent {
    /// A PCM audio chunk ready to send to the WebSocket client.
    Audio(Bytes),
    /// TTS finished synthesizing all queued tokens.
    Finished,
    /// The TTS connection encountered an unrecoverable error.
    /// The Reactor should attempt to reconnect or degrade gracefully.
    Error(String),
}

/// VAD events emitted inline by the VAD stage (no channel hop).
#[derive(Debug, Clone)]
pub enum VadEvent {
    /// User started speaking.
    SpeechStarted,
    /// User stopped speaking. Contains all the audio from this utterance.
    SpeechEnded(Bytes),
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_complete_marker() {
        let (tc, skip) = TurnCompletion::detect("✓ Hello there!").unwrap();
        assert_eq!(tc, TurnCompletion::Complete);
        assert_eq!(&"✓ Hello there!"[skip..], "Hello there!");
    }

    #[test]
    fn detect_complete_no_space() {
        let (tc, skip) = TurnCompletion::detect("✓Hello").unwrap();
        assert_eq!(tc, TurnCompletion::Complete);
        assert_eq!(&"✓Hello"[skip..], "Hello");
    }

    #[test]
    fn detect_incomplete_short() {
        let (tc, _) = TurnCompletion::detect("○").unwrap();
        assert_eq!(tc, TurnCompletion::IncompleteShort);
    }

    #[test]
    fn detect_incomplete_long() {
        let (tc, _) = TurnCompletion::detect("◐").unwrap();
        assert_eq!(tc, TurnCompletion::IncompleteLong);
    }

    #[test]
    fn detect_no_marker() {
        assert!(TurnCompletion::detect("Hello world").is_none());
    }

    #[test]
    fn detect_leading_whitespace() {
        let (tc, skip) = TurnCompletion::detect("  ✓ Response").unwrap();
        assert_eq!(tc, TurnCompletion::Complete);
        assert_eq!(&"  ✓ Response"[skip..], "Response");
    }

    // ── Late detection (short output) ────────────────────────────
    // ○ and ◐ are 3 UTF-8 bytes each, which is below the 4-byte
    // token accumulation threshold. These tests document why the
    // late-detection codepath in LlmEvent::Finished is required.

    #[test]
    fn incomplete_short_is_under_4_bytes() {
        assert!(
            "○".len() < 4,
            "○ should be <4 bytes to trigger late detection"
        );
        let (tc, _) = TurnCompletion::detect("○").unwrap();
        assert_eq!(tc, TurnCompletion::IncompleteShort);
    }

    #[test]
    fn incomplete_long_is_under_4_bytes() {
        assert!(
            "◐".len() < 4,
            "◐ should be <4 bytes to trigger late detection"
        );
        let (tc, _) = TurnCompletion::detect("◐").unwrap();
        assert_eq!(tc, TurnCompletion::IncompleteLong);
    }
}
/// A pre-configured escalation destination (PSTN number or SIP URI).
///
/// Agents inject these into the `escalate_call` tool schema so the LLM can
/// choose the appropriate destination by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationDestination {
    /// Human-readable label shown to the LLM (e.g. "Support Team", "Billing").
    pub name: String,
    /// E.164 phone number (e.g. "+18005551234") or SIP URI.
    pub phone_number: String,
}
