//! Internal LLM micro-call tasks managed directly by `DefaultAgentBackend`.
//!
//! These tasks run in the background to augment agentic logic:
//! bridging silence with filler words and summarizing tool output.
//! They are fully internal — no public traits, no external customization points.

use std::time::Duration;
use tracing::{info, warn};

use crate::agent_backends::ChatMessage;
use crate::providers::{collect_text, LlmCallConfig, LlmProvider};

const TOOL_SUMMARY_PROMPT: &str = "\
You are a tool result summarizer for a voice assistant. Condense \
the following tool output into a brief, essential summary that \
captures the key information the voice assistant needs to respond \
to the user. Keep only the facts that matter for the conversation.\n\n\
Output ONLY the summary. No explanation, no formatting.";

const TOOL_SUMMARY_TIMEOUT: Duration = Duration::from_secs(8);
const TOOL_FILLER_TIMEOUT: Duration = Duration::from_secs(4);

pub(super) async fn summarize_tool_result(
    provider: &dyn LlmProvider,
    tool_name: &str,
    raw_result: &str,
    summary_min_length: usize,
) -> String {
    if raw_result.len() < summary_min_length {
        return raw_result.to_string();
    }

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(serde_json::Value::String(TOOL_SUMMARY_PROMPT.to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(serde_json::Value::String(format!(
                "Tool: {}\n\nRaw output:\n{}",
                tool_name, raw_result
            ))),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let config = LlmCallConfig {
        temperature: 0.0,
        max_tokens: 200,
        model: None,
    };

    match tokio::time::timeout(
        TOOL_SUMMARY_TIMEOUT,
        collect_text(provider, &messages, &config),
    )
    .await
    {
        Ok(Ok(text)) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                raw_result.to_string()
            } else {
                info!(
                    "[agent_backend::helpers] Tool result summarized ({}): {} -> {} chars",
                    tool_name,
                    raw_result.len(),
                    trimmed.len()
                );
                trimmed
            }
        }
        Ok(Err(e)) => {
            warn!(
                "[agent_backend::helpers] Tool {} summarization failed: {} - using raw result",
                tool_name, e
            );
            raw_result.to_string()
        }
        Err(_) => {
            warn!(
                "[agent_backend::helpers] Tool {} summarization timed out after {:?} - using raw result",
                tool_name, TOOL_SUMMARY_TIMEOUT
            );
            raw_result.to_string()
        }
    }
}

// ── Tool Filler ─────────────────────────────────────────────────────

const TOOL_FILLER_PROMPT: &str = "\
You are a voice assistant on a live phone call. The user just asked \
something that requires calling a tool. Generate a very brief, \
natural acknowledgment (1 sentence max, under 15 words) to fill \
the silence while the tool runs.\n\n\
Output ONLY the spoken sentence. No markers, no formatting, no \
explanation. Be natural and conversational.";

pub(super) async fn generate_tool_filler(
    provider: &dyn LlmProvider,
    tool_name: &str,
    user_msg: &str,
) -> Option<String> {
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(serde_json::Value::String(TOOL_FILLER_PROMPT.to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(serde_json::Value::String(format!(
                "Tool being called: {}\nUser said: {}",
                tool_name, user_msg
            ))),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let config = LlmCallConfig {
        temperature: 0.7,
        max_tokens: 30,
        model: None,
    };
    match tokio::time::timeout(
        TOOL_FILLER_TIMEOUT,
        collect_text(provider, &messages, &config),
    )
    .await
    {
        Ok(Ok(text)) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                info!(
                    "[agent_backend::helpers] Tool filler generated: {:?}",
                    trimmed
                );
                Some(trimmed)
            }
        }
        Ok(Err(e)) => {
            warn!("[agent_backend::helpers] Tool filler failed: {}", e);
            None
        }
        Err(_) => {
            warn!(
                "[agent_backend::helpers] Tool filler timed out after {:?}",
                TOOL_FILLER_TIMEOUT
            );
            None
        }
    }
}
