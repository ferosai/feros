//! Context summarization — LLM-powered conversation compression.
//!
//! When a voice session runs long, the `conversation` history grows and
//! eventually exceeds the LLM's context window. This module provides
//! utilities for:
//!
//! 1. **Token estimation** — cheap heuristic (`chars / 4`)
//! 2. **Message selection** — determines which messages to summarize
//!    while preserving system prompt, recent context, and incomplete
//!    tool-call boundaries
//! 3. **Transcript formatting** — formats messages for the summarization LLM
//! 4. **Summary application** — replaces summarized messages with a
//!    single summary message

use std::collections::HashMap;

use crate::providers::{collect_text, LlmCallConfig, LlmProvider};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent_backends::ChatMessage;

// ── Constants ───────────────────────────────────────────────────

/// Maximum conversation turns to keep (each turn ≈ 2 messages).
/// This is a safety net — context summarization handles the normal case.
/// Only triggers when summarization is disabled, fails, or is overwhelmed.
pub(crate) const MAX_HISTORY_TURNS: usize = 50;

/// Industry-standard heuristic: 1 token ≈ 4 characters.
const CHARS_PER_TOKEN: usize = 4;

/// Estimated structural overhead per message (role, separators, etc.).
const TOKEN_OVERHEAD_PER_MESSAGE: usize = 10;

// ── Configuration ───────────────────────────────────────────────

/// Configuration for automatic context summarization.
#[derive(Debug, Clone)]
pub struct ContextSummarizationConfig {
    /// Maximum estimated tokens before triggering summarization.
    pub max_context_tokens: usize,

    /// Total message count threshold (excluding the system prompt) before
    /// triggering summarization. Computed as `conversation.len() - 1`.
    pub max_unsummarized_messages: usize,

    /// Number of recent messages to always preserve after summarization.
    pub min_messages_after_summary: usize,

    /// Max tokens allocated for the generated summary (passed as `max_tokens` to the LLM).
    pub target_summary_tokens: u32,
}

impl Default for ContextSummarizationConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 20_000,
            max_unsummarized_messages: 20,
            min_messages_after_summary: 4,
            target_summary_tokens: 500,
        }
    }
}

// ── Result of message selection ─────────────────────────────────

/// Result of `get_messages_to_summarize`.
pub struct MessagesToSummarize {
    /// Messages to include in the summary (cloned from conversation).
    pub messages: Vec<ChatMessage>,
    /// Index (in the original conversation) of the last summarized message.
    /// Used to apply the summary back correctly.
    pub last_summarized_index: usize,
    /// Length of the conversation at snapshot time.
    /// Used by `apply_summary` to detect if the conversation shrank (e.g.
    /// due to `trim_history` or a prior summarization) before the result arrives.
    pub snapshot_len: usize,
}

// ── Token estimation ────────────────────────────────────────────

/// Estimate token count for a text string.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / CHARS_PER_TOKEN
}

/// Estimate total token count for a conversation.
///
/// Accounts for message content, tool call arguments, tool call IDs,
/// and per-message structural overhead.
pub fn estimate_context_tokens(conversation: &[ChatMessage]) -> usize {
    let mut total = 0;

    for msg in conversation {
        // Role and structure overhead
        total += TOKEN_OVERHEAD_PER_MESSAGE;

        // Message content
        if let Some(ref content) = msg.content {
            if let serde_json::Value::String(s) = content {
                total += estimate_tokens(s);
            } else {
                total += estimate_tokens(&content.to_string());
            }
        }

        // Tool calls (arguments)
        if let Some(ref tool_calls) = msg.tool_calls {
            for tc in tool_calls {
                if let Some(func) = tc.get("function") {
                    let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let args = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                    total += estimate_tokens(name) + estimate_tokens(args);
                }
            }
        }

        // Tool call ID overhead
        if msg.tool_call_id.is_some() {
            total += TOKEN_OVERHEAD_PER_MESSAGE;
        }
    }

    total
}

// ── Threshold checking ──────────────────────────────────────────

/// Returns `true` if the conversation should be summarized.
///
/// Triggers when either:
/// - Estimated token count exceeds `config.max_context_tokens`
/// - Message count exceeds `config.max_unsummarized_messages`
///
/// Uses `conversation.len() - 1` as the message count (subtracting the system
/// prompt at index 0). After applying a summary the context is `[system, user_summary, ack, ...recent]`,
/// so `len - 1 = 2 + num_recent`, which stays well below the threshold until enough
/// new messages accumulate — avoiding immediate re-summarization.
pub fn should_summarize(conversation: &[ChatMessage], config: &ContextSummarizationConfig) -> bool {
    // len - 1 excludes the system prompt.
    let message_count = conversation.len().saturating_sub(1);
    if message_count >= config.max_unsummarized_messages {
        return true;
    }

    estimate_context_tokens(conversation) >= config.max_context_tokens
}

// ── Boundary-safe message selection ─────────────────────────────

/// Find the earliest message index with an unresolved tool call
/// within the range `[start_idx, end_idx)`.
///
/// A tool call is "unresolved" if its `tool_call_id` has not been
/// matched by a corresponding `tool` role message within the range.
///
/// Returns `None` if all tool calls in the range are resolved.
fn find_earliest_unresolved_tool_call(
    conversation: &[ChatMessage],
    start_idx: usize,
    end_idx: usize,
) -> Option<usize> {
    // Map from tool_call_id → message index of the assistant message
    let mut pending: HashMap<String, usize> = HashMap::new();

    for i in start_idx..end_idx {
        let msg = &conversation[i];

        // Track tool calls from assistant messages
        if msg.role == "assistant" {
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        pending.insert(id.to_string(), i);
                    }
                }
            }
        }

        // Resolve tool results
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                pending.remove(id);
            }
        }
    }

    // Return the earliest pending tool call index (if any)
    pending.values().copied().min()
}

/// Determine which messages should be summarized.
///
/// Preserves:
/// - The system prompt (index 0)
/// - The last `min_messages_to_keep` messages
/// - Incomplete tool-call blocks (never splits them)
///
/// Returns `None` if there aren't enough messages to summarize.
pub fn get_messages_to_summarize(
    conversation: &[ChatMessage],
    min_messages_to_keep: usize,
) -> Option<MessagesToSummarize> {
    if conversation.len() <= min_messages_to_keep {
        return None;
    }

    // Find first system message
    let first_system_index = conversation.iter().position(|m| m.role == "system");

    // Summary starts after the system prompt
    let summary_start = match first_system_index {
        Some(idx) => idx + 1,
        None => 0,
    };

    // Summary ends before the last N messages we want to keep
    let mut summary_end = conversation.len().saturating_sub(min_messages_to_keep);

    if summary_start >= summary_end {
        return None;
    }

    // Check for unresolved tool calls within the summary range ONLY.
    // A tool call in the summary range whose result falls outside (in the
    // "keep" zone) is unresolved from the summary's perspective — summarizing
    // the assistant+tool_calls message while leaving the tool result outside
    // would orphan it and break the OpenAI message format.
    if let Some(earliest_unresolved) =
        find_earliest_unresolved_tool_call(conversation, summary_start, summary_end)
    {
        if earliest_unresolved < summary_end {
            tracing::debug!(
                "[context_summarizer] Unresolved tool call at index {}, shrinking summary_end from {} to {}",
                earliest_unresolved,
                summary_end,
                earliest_unresolved,
            );
            summary_end = earliest_unresolved;
        }
    }

    if summary_start >= summary_end {
        return None;
    }

    let messages = conversation[summary_start..summary_end].to_vec();
    let last_summarized_index = summary_end - 1;

    Some(MessagesToSummarize {
        messages,
        last_summarized_index,
        snapshot_len: conversation.len(),
    })
}

// ── Transcript formatting ───────────────────────────────────────

/// Format messages as a human-readable transcript for the summarization LLM.
///
/// Produces lines like:
/// ```text
/// USER: Hello
/// ASSISTANT: Hi there
/// TOOL_CALL: get_time({})
/// TOOL_RESULT[call_123]: {"time": "10:30 AM"}
/// ```
pub fn format_messages_for_summary(messages: &[ChatMessage]) -> String {
    let mut parts = Vec::new();

    for msg in messages {
        // Primary content
        if let Some(ref content_val) = msg.content {
            let content = match content_val {
                serde_json::Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            if !content.is_empty() {
                if msg.role == "tool" {
                    // Tool results get special formatting
                    let call_id = msg.tool_call_id.as_deref().unwrap_or("unknown");
                    parts.push(format!("TOOL_RESULT[{}]: {}", call_id, content));
                } else {
                    let role_upper = msg.role.to_uppercase();
                    parts.push(format!("{}: {}", role_upper, content));
                }
            }
        }

        // Tool calls
        if let Some(ref tool_calls) = msg.tool_calls {
            for tc in tool_calls {
                if let Some(func) = tc.get("function") {
                    let name = func
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let args = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                    parts.push(format!("TOOL_CALL: {}({})", name, args));
                }
            }
        }
    }

    parts.join("\n\n")
}

// ── Summary application ─────────────────────────────────────────

/// Apply a generated summary to the conversation, replacing the
/// summarized messages with a user+assistant summary pair.
///
/// Reconstructs:
///   `[system_prompt] + [user: "[Context...] {summary}"] + [assistant: ack] + [kept_messages]`
///
/// This mirrors the Python builder's `compress_history` pattern:
/// the summary is framed as context flowing *to* the LLM (user role),
/// followed by a short assistant acknowledgment to close the turn cleanly.
/// Without the ack, kept_messages[0] being a user message would create
/// two consecutive user messages, which some providers reject.
///
/// Returns `true` if the summary was applied, `false` if the index
/// was stale (conversation shrank since the snapshot was taken) or if
/// fewer than `min_messages_after_summary` messages would be kept.
pub fn apply_summary(
    conversation: &mut Vec<ChatMessage>,
    summary: &str,
    last_summarized_index: usize,
    snapshot_len: usize,
    min_messages_after_summary: usize,
) -> bool {
    // Reject if the conversation shrank since we took the snapshot.
    // This means trim_history or a prior summarization ran concurrently —
    // our index is no longer meaningful.
    if conversation.len() < snapshot_len {
        tracing::warn!(
            "[context_summarizer] Stale summary: snapshot_len={} but conversation.len()={}, discarding",
            snapshot_len,
            conversation.len(),
        );
        return false;
    }

    // Validate the index is still within bounds
    if last_summarized_index >= conversation.len() {
        tracing::warn!(
            "[context_summarizer] Stale summary index {} (conversation len={}), discarding",
            last_summarized_index,
            conversation.len(),
        );
        return false;
    }

    // Validate that enough recent messages remain after the summarized range.
    // remaining = number of messages after last_summarized_index.
    let remaining = conversation.len().saturating_sub(last_summarized_index + 1);
    if remaining < min_messages_after_summary {
        tracing::warn!(
            "[context_summarizer] min_keep check failed: {} remaining < {} required, discarding",
            remaining,
            min_messages_after_summary,
        );
        return false;
    }

    // Find the system prompt (should be index 0)
    let system_msg = if !conversation.is_empty() && conversation[0].role == "system" {
        Some(conversation[0].clone())
    } else {
        None
    };

    // User message framing the summary as prior context (Python builder pattern)
    let summary_user_msg = ChatMessage {
        role: "user".to_string(),
        content: Some(serde_json::Value::String(format!(
            "[Context from earlier in our conversation]\n\n{}",
            summary
        ))),
        tool_calls: None,
        tool_call_id: None,
    };

    // Short assistant ack to close the summary turn cleanly.
    // Without this, kept_messages[0] being a user message would create
    // two consecutive user messages, which some providers reject.
    let summary_ack_msg = ChatMessage {
        role: "assistant".to_string(),
        content: Some(serde_json::Value::String(
            "Got it — I have the full context from our earlier conversation. Let's continue."
                .to_string(),
        )),
        tool_calls: None,
        tool_call_id: None,
    };

    // Collect messages after the summarized range
    let kept_messages: Vec<ChatMessage> = conversation[last_summarized_index + 1..].to_vec();

    // Reconstruct: [system] + [user summary] + [assistant ack] + [kept]
    conversation.clear();
    if let Some(sys) = system_msg {
        conversation.push(sys);
    }
    conversation.push(summary_user_msg);
    conversation.push(summary_ack_msg);
    conversation.extend(kept_messages);

    true
}

// ── Context Summarizer Task ─────────────────────────────────────

const CONTEXT_SUMMARY_PROMPT: &str = "\
You are summarizing a phone conversation between a user and a voice AI agent.\n\n\
Create a concise summary preserving:\n\
- Key facts, decisions, and agreements\n\
- User preferences and requirements\n\
- Unresolved questions or pending action items\n\
- Tool call results that are still relevant\n\n\
Omit greetings, small talk, and resolved tangents.\n\
Output ONLY the summary, no other text.";

pub struct ContextSummarizer {
    provider: Arc<dyn LlmProvider>,
    in_progress: bool,
    result_tx: mpsc::UnboundedSender<(String, usize, usize, usize)>,
    result_rx: mpsc::UnboundedReceiver<(String, usize, usize, usize)>,
}

impl ContextSummarizer {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        Self {
            provider,
            in_progress: false,
            result_tx,
            result_rx,
        }
    }

    /// Check if context summarization should trigger, and spawn the task if so.
    pub fn maybe_start(
        &mut self,
        conversation: &[ChatMessage],
        config: &ContextSummarizationConfig,
    ) {
        if self.in_progress {
            return;
        }

        if !should_summarize(conversation, config) {
            return;
        }

        let result =
            match get_messages_to_summarize(conversation, config.min_messages_after_summary) {
                Some(r) if !r.messages.is_empty() => r,
                _ => return,
            };

        self.in_progress = true;

        let transcript = format_messages_for_summary(&result.messages);
        let last_idx = result.last_summarized_index;
        let snapshot_len = result.snapshot_len;
        let min_keep = config.min_messages_after_summary;
        let max_tokens = config.target_summary_tokens;
        let provider = Arc::clone(&self.provider);
        let tx = self.result_tx.clone();

        info!(
            "[context_summarizer] Spawning background summarization: {} messages, last_idx={}",
            result.messages.len(),
            last_idx,
        );

        tokio::spawn(async move {
            let summary = match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                summarize_internal(&*provider, &transcript, max_tokens),
            )
            .await
            {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!("[context_summarizer] Summarization timed out after 120s");
                    String::new()
                }
            };
            let _ = tx.send((summary, last_idx, snapshot_len, min_keep));
        });
    }

    /// Try to apply a completed context summarization result (non-blocking).
    pub fn try_apply(&mut self, conversation: &mut Vec<ChatMessage>) {
        match self.result_rx.try_recv() {
            Ok((summary, last_idx, snapshot_len, min_keep)) => {
                self.in_progress = false;
                if summary.is_empty() {
                    warn!("[context_summarizer] Empty summary received, skipping");
                    return;
                }
                let before_len = conversation.len();
                if apply_summary(conversation, &summary, last_idx, snapshot_len, min_keep) {
                    info!(
                        "[context_summarizer] Applied summary: {} → {} messages",
                        before_len,
                        conversation.len(),
                    );
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.in_progress = false;
            }
        }
    }

    /// Reset summarization state after a turn is cancelled.
    ///
    /// Drains any result that arrived before `cancel()` was called. If the
    /// background task sends to the channel *after* the drain (but before the
    /// next `try_apply`), `try_apply` will consume it on the next turn. That
    /// stale result is harmless: `apply_summary` checks `snapshot_len` and will
    /// reject it if the conversation has diverged since the snapshot.
    ///
    /// Backends are not reused across sessions, so this race is moot in practice.
    pub fn cancel(&mut self) {
        self.in_progress = false;
        while self.result_rx.try_recv().is_ok() {}
    }
}

async fn summarize_internal(
    provider: &dyn LlmProvider,
    transcript: &str,
    max_tokens: u32,
) -> String {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(serde_json::Value::String(CONTEXT_SUMMARY_PROMPT.to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(serde_json::Value::String(format!(
                "Summarize this conversation transcript:\n\n{}",
                transcript
            ))),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let config = LlmCallConfig {
        temperature: 0.0,
        max_tokens,
        model: None,
    };
    match collect_text(provider, &messages, &config).await {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                warn!("[context_summarizer] Context summary empty");
                String::new()
            } else {
                info!(
                    "[context_summarizer] Context summarized: {} chars → {} chars",
                    transcript.len(),
                    trimmed.len()
                );
                trimmed
            }
        }
        Err(e) => {
            warn!("[context_summarizer] Context summarization failed: {}", e);
            String::new()
        }
    }
}

// ── Hard Trim (Safety Net) ──────────────────────────────────────

/// Trim conversation history to keep at most `MAX_HISTORY_TURNS`
/// user/assistant exchanges. The system prompt (index 0) is always
/// preserved.
///
/// Tool-call blocks (assistant with `tool_calls` → N `tool` results)
/// are treated as atomic units — we never cut in the middle of one,
/// which would leave orphaned `tool` messages and break the OpenAI API.
pub(crate) fn trim_history(conversation: &mut Vec<ChatMessage>) {
    let max_messages = 1 + MAX_HISTORY_TURNS * 2;
    if conversation.len() <= max_messages {
        return;
    }

    let target_remove = conversation.len() - max_messages;
    let mut removed = 0;
    let mut i = 1; // start after system prompt

    while removed < target_remove && i < conversation.len() {
        if conversation[i].role == "assistant" && conversation[i].tool_calls.is_some() {
            let block_start = i;
            i += 1;
            while i < conversation.len() && conversation[i].role == "tool" {
                i += 1;
            }
            let block_len = i - block_start;
            conversation.drain(block_start..i);
            removed += block_len;
            i = block_start;
        } else {
            conversation.remove(i);
            removed += 1;
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(serde_json::Value::String(content.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn tool_assistant(call_ids: &[&str]) -> ChatMessage {
        let tool_calls: Vec<serde_json::Value> = call_ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": "test_tool", "arguments": "{}" }
                })
            })
            .collect();
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    fn tool_result(call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(serde_json::Value::String(content.to_string())),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
        }
    }

    // ── Token estimation ────────────────────────────────────────

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens("Hello world"), 2); // 11 / 4 = 2
        assert_eq!(estimate_tokens("This is a test message"), 5); // 22 / 4 = 5
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_context_tokens_aggregates() {
        let conv = vec![
            msg("system", "You are helpful"), // ~3 tokens + 10 overhead
            msg("user", "Hello"),             // ~1 token + 10 overhead
            msg("assistant", "Hi there"),     // ~2 tokens + 10 overhead
        ];
        let total = estimate_context_tokens(&conv);
        assert!(total > 30, "Expected > 30, got {}", total);
        assert!(total < 50, "Expected < 50, got {}", total);
    }

    // ── should_summarize ────────────────────────────────────────

    #[test]
    fn should_summarize_message_threshold() {
        let config = ContextSummarizationConfig {
            max_unsummarized_messages: 5,
            max_context_tokens: 100_000, // won't trigger
            ..Default::default()
        };
        let mut conv = vec![msg("system", "sys")];
        for i in 0..4 {
            conv.push(msg("user", &format!("msg{}", i)));
        }
        assert!(!should_summarize(&conv, &config)); // len-1 = 4 < 5

        conv.push(msg("assistant", "resp"));
        assert!(should_summarize(&conv, &config)); // len-1 = 5 >= 5
    }

    #[test]
    fn should_summarize_token_threshold() {
        let config = ContextSummarizationConfig {
            max_context_tokens: 50,
            max_unsummarized_messages: 1000,
            ..Default::default()
        };
        let conv = vec![
            msg("system", "You are a helpful assistant with a very long system prompt that exceeds the token budget easily"),
            msg("user", "Tell me something interesting about the world and all of its wonders"),
            msg("assistant", "The world is full of amazing things that you would never believe existed"),
        ];
        assert!(should_summarize(&conv, &config));
    }

    #[test]
    fn should_summarize_neither() {
        let config = ContextSummarizationConfig {
            max_context_tokens: 100_000,
            max_unsummarized_messages: 100,
            ..Default::default()
        };
        let conv = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            msg("assistant", "hi"),
        ];
        assert!(!should_summarize(&conv, &config));
    }

    // ── get_messages_to_summarize ───────────────────────────────

    #[test]
    fn get_messages_preserves_system_and_recent() {
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "Message 1"),
            msg("assistant", "Response 1"),
            msg("user", "Message 2"),
            msg("assistant", "Response 2"),
            msg("user", "Message 3"),
            msg("assistant", "Response 3"),
        ];
        let result = get_messages_to_summarize(&conv, 2).unwrap();
        // Should summarize indices 1..5 (4 messages), keeping last 2
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.messages[0].content.as_ref().and_then(|v| v.as_str()), Some("Message 1"));
        assert_eq!(result.messages[3].content.as_ref().and_then(|v| v.as_str()), Some("Response 2"));
        assert_eq!(result.last_summarized_index, 4);
    }

    #[test]
    fn get_messages_insufficient() {
        let conv = vec![msg("user", "Message 1"), msg("assistant", "Response 1")];
        assert!(get_messages_to_summarize(&conv, 2).is_none());
    }

    #[test]
    fn get_messages_tool_boundary_complete() {
        // A complete tool-call block CAN be summarized
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "What time is it?"),
            tool_assistant(&["call_123"]),
            tool_result("call_123", r#"{"time": "10:30 AM"}"#),
            msg("assistant", "It's 10:30 AM"),
            msg("user", "Latest message"),
        ];
        let result = get_messages_to_summarize(&conv, 1).unwrap();
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.last_summarized_index, 4);
    }

    #[test]
    fn get_messages_tool_boundary_incomplete() {
        // An incomplete tool-call block must NOT be summarized
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "What time is it?"),
            tool_assistant(&["call_123"]),
            // No tool result — call is in progress
            msg("user", "Latest message"),
        ];
        let result = get_messages_to_summarize(&conv, 1).unwrap();
        // Should only summarize index 1 (the user message before the tool call)
        assert_eq!(result.messages.len(), 1);
        assert_eq!(
            result.messages[0].content.as_ref().and_then(|v| v.as_str()),
            Some("What time is it?")
        );
        assert_eq!(result.last_summarized_index, 1);
    }

    #[test]
    fn get_messages_tool_result_in_keep_zone() {
        // Tool call at index 2, result at index 3.
        // When keeping last 2, summary_end = 3. The tool call at index 2
        // has its result at index 3 which is OUTSIDE the summary range [1,3).
        // This means the tool call is "unresolved" within the summary range,
        // so we must shrink to before index 2.
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "Early message"),
            tool_assistant(&["call_x"]),
            tool_result("call_x", "result"),
            msg("user", "Latest message"),
        ];
        let result = get_messages_to_summarize(&conv, 2);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].content.as_ref().and_then(|v| v.as_str()), Some("Early message"));
        assert_eq!(result.last_summarized_index, 1);
    }

    #[test]
    fn get_messages_multi_tool_partial() {
        // Multiple tool calls in one assistant message, only some resolved
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "Message 1"),
            msg("assistant", "Response 1"),
            msg("user", "What's the time and date?"),
            tool_assistant(&["call_time", "call_date"]),
            tool_result("call_time", r#"{"time": "10:30 AM"}"#),
            // call_date NOT resolved
            msg("user", "Latest message"),
        ];
        let result = get_messages_to_summarize(&conv, 1).unwrap();
        // Should stop before index 4 (where the incomplete tool call block is)
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.last_summarized_index, 3);
    }

    #[test]
    fn get_messages_all_completed_multi_tool() {
        let conv = vec![
            msg("system", "System prompt"),
            msg("user", "What's the time and date?"),
            tool_assistant(&["call_time", "call_date"]),
            tool_result("call_time", r#"{"time": "10:30 AM"}"#),
            tool_result("call_date", r#"{"date": "January 1"}"#),
            msg("assistant", "It's 10:30 AM on January 1"),
            msg("user", "Latest"),
        ];
        let result = get_messages_to_summarize(&conv, 1).unwrap();
        assert_eq!(result.messages.len(), 5);
        assert_eq!(result.last_summarized_index, 5);
    }

    // ── format_messages_for_summary ─────────────────────────────

    #[test]
    fn format_transcript_basic() {
        let messages = vec![
            msg("user", "Hello"),
            msg("assistant", "Hi there"),
            msg("user", "How are you?"),
        ];
        let transcript = format_messages_for_summary(&messages);
        assert!(transcript.contains("USER: Hello"));
        assert!(transcript.contains("ASSISTANT: Hi there"));
        assert!(transcript.contains("USER: How are you?"));
    }

    #[test]
    fn format_transcript_with_tools() {
        let messages = vec![
            msg("user", "What time is it?"),
            tool_assistant(&["call_123"]),
            tool_result("call_123", r#"{"time": "10:30 AM"}"#),
            msg("assistant", "It's 10:30 AM"),
        ];
        let transcript = format_messages_for_summary(&messages);
        assert!(transcript.contains("TOOL_CALL: test_tool({})"));
        assert!(transcript.contains(r#"TOOL_RESULT[call_123]: {"time": "10:30 AM"}"#));
    }

    // ── apply_summary ───────────────────────────────────────────

    #[test]
    fn apply_summary_basic() {
        let mut conv = vec![
            msg("system", "System prompt"),
            msg("user", "Message 1"),
            msg("assistant", "Response 1"),
            msg("user", "Message 2"),
            msg("assistant", "Response 2"),
            msg("user", "Latest"),
        ];
        let snapshot_len = conv.len();
        let applied = apply_summary(&mut conv, "Summary of messages 1-2", 3, snapshot_len, 2);
        assert!(applied);
        // system + user_summary + ack + Response 2 + Latest = 5
        assert_eq!(conv.len(), 5);
        assert_eq!(conv[0].role, "system");
        assert_eq!(conv[1].role, "user");
        assert!(conv[1]
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("Summary of messages 1-2"));
        assert!(conv[1]
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("[Context from earlier"));
        assert_eq!(conv[2].role, "assistant"); // ack
        assert_eq!(conv[3].content.as_ref().and_then(|v| v.as_str()), Some("Response 2"));
        assert_eq!(conv[4].content.as_ref().and_then(|v| v.as_str()), Some("Latest"));
    }

    #[test]
    fn apply_summary_stale_index() {
        let mut conv = vec![msg("system", "sys"), msg("user", "hello")];
        // Stale index: beyond conversation length
        let applied = apply_summary(&mut conv, "summary", 10, 2, 1);
        assert!(!applied);
        assert_eq!(conv.len(), 2); // unchanged
    }

    #[test]
    fn apply_summary_stale_snapshot_len() {
        let mut conv = vec![msg("system", "sys"), msg("user", "hello")];
        // Stale snapshot: taken when conv had 5 messages, now it has 2 (shrank)
        let applied = apply_summary(&mut conv, "summary", 1, 5, 1);
        assert!(!applied);
        assert_eq!(conv.len(), 2); // unchanged
    }

    #[test]
    fn apply_summary_min_keep_violated() {
        let mut conv = vec![
            msg("system", "sys"),
            msg("user", "u1"),
            msg("assistant", "a1"),
            msg("user", "u2"),
        ];
        let snapshot_len = conv.len();
        // Summarize up to index 2, keeping only 1 message (u2).
        // min_messages_after_summary = 4 → should be rejected.
        let applied = apply_summary(&mut conv, "summary", 2, snapshot_len, 4);
        assert!(!applied);
        assert_eq!(conv.len(), 4); // unchanged
    }

    // ── trim_history ────────────────────────────────────────────

    #[test]
    fn trim_preserves_system_prompt() {
        let mut conv = vec![msg("system", "sys")];
        for i in 0..50 {
            conv.push(msg("user", &format!("u{}", i)));
            conv.push(msg("assistant", &format!("a{}", i)));
        }
        trim_history(&mut conv);
        assert_eq!(conv[0].role, "system");
        assert!(conv.len() <= 1 + MAX_HISTORY_TURNS * 2);
    }

    #[test]
    fn trim_preserves_tool_block_integrity() {
        let mut conv = vec![msg("system", "sys")];
        conv.push(msg("user", "u0"));
        conv.push(tool_assistant(&["call_0", "call_1"]));
        conv.push(tool_result("call_0", "result"));
        conv.push(tool_result("call_1", "result"));
        conv.push(msg("assistant", "response"));
        for i in 1..30 {
            conv.push(msg("user", &format!("u{}", i)));
            conv.push(msg("assistant", &format!("a{}", i)));
        }
        trim_history(&mut conv);

        for (j, m) in conv.iter().enumerate() {
            if m.role == "tool" {
                let prev = &conv[j - 1];
                assert!(
                    prev.role == "tool" || (prev.role == "assistant" && prev.tool_calls.is_some()),
                    "orphaned tool message at index {}",
                    j
                );
            }
        }
    }
}
