//! Default (local) agent backend — wraps `LlmProvider` + optional `SwarmState` + `HttpExecutor`.
//!
//! Runs the full agentic loop internally:
//! LLM → tool calls → execute → feed result → next LLM round → ... → Finished.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use chrono_tz::Tz;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::agent_backends::ChatMessage;
use crate::agent_backends::{AgentBackend, AgentBackendConfig, AgentEvent, ToolInterceptor};
use crate::context_summarizer::{trim_history, ContextSummarizationConfig, ContextSummarizer};
use crate::micro_tasks;
use crate::providers::{LlmCallConfig, LlmProvider, LlmProviderError};
use crate::providers::{LlmEvent as InnerLlmEvent, ToolCallEvent};
use crate::swarm::{
    build_node_tool_schemas, make_artifact_tool_schemas, make_hang_up_tool_schema,
    make_on_hold_tool_schema, AgentGraphDef, EscalationDestination, SwarmState,
    ESCALATE_CALL_TOOL_NAME, HANG_UP_TOOL_NAME, ON_HOLD_TOOL_NAME,
};
use crate::tool_executor::{
    spawn_tool_task, ToolTaskResult,
};
use crate::ScriptEngine;

// ── Runtime system prompt suffix ────────────────────────────────
// Appended to every node's system prompt at runtime so it is never
// stored in the DB config JSON.  Covers turn-completion markers,
// output rules, and TTS normalization.

const SYSTEM_PROMPT_SUFFIX: &str = r#"

## Turn Completion

**Every response MUST begin with a turn completion indicator:**
- `✓` — user gave a complete answer; respond normally
- `○` — user was cut off mid-sentence (audible trailing-off); gently prompt them to continue
- `◐` — user is genuinely deliberating (said "hmm", "let me think", or went silent mid-thought); check in when ready

**Rules for choosing the marker — when in doubt, use `✓`:**
- Use `✓` when the user directly answered a question you asked, even if the answer
  is purely numeric (phone numbers, dates, ZIP codes, account IDs, amounts).
  Example: you asked "what's your phone number?" and they said "4086768103" → `✓`
- Use `○` only when the user's speech ended mid-word or mid-clause with no natural pause.
- Use `◐` only for genuine open-ended thinking: "hmm...", "let me see...", long silence
  after an open question. NEVER use `◐` for a direct factual answer.

## Output Rules

Your text is spoken aloud by a voice engine. ONLY output natural spoken language.
NEVER output:
- XML tags (e.g. `<thinking>`, `<internal>`, `<note>`)
- Markdown formatting (`**bold**`, `# heading`, `- list`)
- Internal notes, stage directions, or annotations
- Parenthetical asides like `(wait for response)` or `(internal: ...)`

Verbalize symbols and abbreviations.
Say "twenty-four dollars per month" instead of "$24/month".

## Tool Usage — CRITICAL

To perform any action (book, save, create, send, look up, check, etc.),
you MUST call the appropriate tool FIRST.
Only confirm the action to the user AFTER the tool has returned a result.

Correct workflow:
1. User requests an action → say a filler phrase (e.g. "Let me check...", "One moment...")
2. Call the tool
3. Tool returns a result → tell the user what happened

Wrong workflow:
- Calling a tool in silence without saying anything first.
- Telling the user "Done!" without calling any tool.

If no matching tool exists for what the user wants, say so honestly.

## Ending the Call — CRITICAL

When the conversation is complete or the user says goodbye, you MUST call the `hang_up` tool
immediately — WITHOUT WAITING FOR THE USER TO RESPOND AGAIN.

Correct order in the SAME response:
1. Say your farewell text (e.g., "Thanks for calling, goodbye!").
2. Call the `hang_up` tool immediately after.

WRONG (never do this):
- Call `hang_up` entirely in silence without saying a farewell text.
- Say "Goodbye!" → wait → then hang up  ← call stays open forever
- Say "Goodbye!" without calling `hang_up` at all  ← call stays open forever

Triggers — call `hang_up` immediately when:
- User says goodbye, thanks you, or signals they are done
- The task is fully completed (reservation made, question answered, etc.)
- You have said your farewell sentence

## Artifacts (Persistent Memory)

You have `save_artifact`, `read_artifact`, and `list_artifacts` tools.
Use them to persist critical information that must survive context compression:
- Caller details: name, phone number, email, account ID — save these immediately when collected
- Confirmed choices, preferences, booking details, reference numbers
- Any information the caller may ask about later in the call

Best practices:
- Save to a descriptive name like `caller_info.md` or `booking_summary.md`
- Call `list_artifacts` when context seems incomplete, then `read_artifact` to restore the data
- Overwrite an artifact with the same name to update it"#;

/// Format a datetime with minutes truncated to 10-min intervals.
fn format_truncated<Z: chrono::TimeZone>(dt: &chrono::DateTime<Z>) -> String
where
    Z::Offset: std::fmt::Display,
{
    use chrono::Timelike;
    let truncated_min = (dt.minute() / 10) * 10;
    format!(
        "{}:{:02} {}",
        dt.format("%A, %B %d, %Y %I"),
        truncated_min,
        dt.format("%p")
    )
}

/// Append the runtime suffix to a system prompt (idempotent).
/// If `timezone` is provided, also appends the current date/time.
fn with_suffix(prompt: &str, timezone: Option<&str>) -> String {
    // Idempotent: detect both legacy and current suffix formats.
    if prompt.contains("## Turn Completion")
        || prompt.contains("CRITICAL INSTRUCTION: Every response MUST begin with a turn completion")
    {
        return prompt.to_string();
    }

    let mut result = format!("{}{}", prompt, SYSTEM_PROMPT_SUFFIX);

    // Append current date + time (truncated to 10-min intervals for prompt cache stability)
    let now = Utc::now();
    let (formatted, tz_label) = match timezone.filter(|s| !s.is_empty()) {
        Some(tz_str) => match tz_str.parse::<Tz>() {
            Ok(tz) => (
                format_truncated(&now.with_timezone(&tz)),
                tz_str.to_string(),
            ),
            Err(_) => (format!("{} UTC", format_truncated(&now)), "UTC".to_string()),
        },
        None => (format!("{} UTC", format_truncated(&now)), "UTC".to_string()),
    };

    result.push_str(&format!(
        "\n\n## Current Date & Time\n\nIt is currently {} ({}). Note: time is approximate and may differ by up to 10 minutes.",
        formatted, tz_label
    ));

    result
}

// ── Internal types ──────────────────────────────────────────────
// Use `tool_pipeline` for execution definitions.

/// State machine phase for `recv()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No active turn.
    Idle,
    /// Streaming tokens/tool calls from the LLM.
    Streaming,
    /// Waiting for spawned tool tasks to complete.
    WaitingForTools,
}

/// A `hang_up` tool call that was seen during LLM streaming but deferred
/// until all sibling tool tasks complete.  Populated in
/// `handle_streaming_phase` and consumed in `handle_waiting_for_tools_phase`
/// (or at stream-end when `tools_remaining == 0`).
struct PendingHangUp {
    reason: String,
    /// Farewell text emitted by the LLM; filled in once the stream closes
    /// and `push_assistant_message()` is called.
    content: Option<String>,
}

/// An `escalate_call` tool call that was deferred during LLM streaming.
/// Emitted as `AgentEvent::EscalateCall` once all sibling tools complete.
struct PendingEscalateCall {
    /// Resolved phone number (looked up from destination_name).
    destination: String,
    reason: String,
}

// ── Observability ────────────────────────────────────────────────

/// Per-LLM-call observability context.
///
/// Captured at the start of each `stream_completion` call and consumed
/// by `emit_llm_complete` to build the `AgentEvent::LlmComplete` payload.
/// Stored as `Option<LlmCallObs>` on the backend — `None` between calls.
struct LlmCallObs {
    start: std::time::Instant,
    first_token: Option<std::time::Instant>,
    model: String,
    provider: String,
    input_json: String,
    tools_json: Option<String>,
    temperature: f64,
    max_tokens: u32,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: Option<u32>,
}

// ── DefaultAgentBackend ─────────────────────────────────────────

/// Production implementation of [`AgentBackend`].
///
/// Wraps an `LlmProvider` + optional `SwarmState` + `ScriptEngine`.
/// Drives the full agentic loop (LLM ↔ tools) internally, emitting
/// lifecycle events via `recv()`.
pub struct DefaultAgentBackend {
    provider: Arc<dyn LlmProvider>,

    /// Script engine for graph-defined tools.
    script_engine: Option<Arc<ScriptEngine>>,

    /// Optional interceptor for intercepting tool calls (testing, observability).
    interceptor: Option<Arc<dyn ToolInterceptor>>,

    // ── Context summarization ──
    /// Optional context summarizer for background conversation compression.
    context_summarizer: Option<ContextSummarizer>,
    /// Configuration for context summarization thresholds.
    context_summarization_config: ContextSummarizationConfig,

    // ── Tool Filler ──
    /// A running task generating filler speech, if any.
    filler_task: Option<tokio::task::JoinHandle<Option<String>>>,

    // ── LLM stream ──
    llm_event_rx: Option<mpsc::Receiver<InnerLlmEvent>>,

    // ── Swarm ──
    swarm: Option<SwarmState>,

    // ── Base config ──
    config: AgentBackendConfig,

    // ── Conversation history (owned) ──
    conversation: Vec<ChatMessage>,

    // ── Agentic loop state ──
    phase: Phase,
    tool_rounds: u32,
    tools_remaining: usize,
    /// (call_id, name) for pending tool tasks — used by cancel() for placeholders.
    pending_tool_info: HashMap<String, String>,
    /// Internal channel for tool execution results.
    tool_result_tx: mpsc::UnboundedSender<ToolTaskResult>,
    tool_result_rx: mpsc::UnboundedReceiver<ToolTaskResult>,
    /// Buffered events to yield before polling.
    event_buffer: VecDeque<AgentEvent>,

    // ── Per-turn accumulation ──
    pending_tokens: Vec<String>,
    pending_tool_calls: Vec<ToolCallEvent>,

    // ── Deferred hang_up ──
    /// Set when a `hang_up` tool call is seen during LLM streaming while
    /// other tools are still in-flight.  Cleared at turn start / cancel.
    pending_hang_up: Option<PendingHangUp>,

    // ── Deferred escalate_call ──
    /// Set when an `escalate_call` tool call is seen during LLM streaming.
    /// Emitted as `AgentEvent::EscalateCall` after TTS drains.
    pending_escalate_call: Option<PendingEscalateCall>,

    // ── Telephony/Escalation context ──
    /// True when this session is over a telephony transport (Twilio/Telnyx).
    /// Controls whether `escalate_call` tool is injected.
    is_telephony: bool,
    /// Pre-configured escalation targets for this agent.
    escalation_destinations: Vec<EscalationDestination>,

    // ── Observability (per LLM call) ──
    /// Present while an LLM stream is active; consumed by `emit_llm_complete`.
    obs: Option<LlmCallObs>,
}

impl DefaultAgentBackend {
    /// Create a new backend.
    ///
    /// * `provider` — the LLM provider (rig-backed).
    /// * `agent_graph` — optional swarm graph for multi-agent routing.
    /// * `config` — base generation config.
    /// * `executor` — tool runner for executing tool calls.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        agent_graph: Option<AgentGraphDef>,
        config: AgentBackendConfig,
    ) -> Self {
        // Build the Script engine from graph tools (if any).
        // Secrets are injected so QuickJS scripts can call secret("provider").
        let script_engine = agent_graph.as_ref().and_then(|g| {
            if g.tools.is_empty() {
                None
            } else {
                Some(Arc::new(ScriptEngine::with_tools_and_sandbox(
                    g.tools.clone(),
                    std::env::temp_dir(),
                    config.secrets.clone(),
                )))
            }
        });

        let swarm = agent_graph.map(SwarmState::new);
        let (tool_result_tx, tool_result_rx) = mpsc::unbounded_channel();

        // ── Wire tasks from config ───────────────────────────────────────────
        //
        // All tasks share the same provider as the main agent loop.
        // Flags are set by the calling binary (voice-engine reads env vars via
        // envy and populates AgentBackendConfig directly).

        let ctx_summarizer: Option<ContextSummarizer> = if config.context_summarizer {
            Some(ContextSummarizer::new(Arc::clone(&provider)))
        } else {
            None
        };

        Self {
            provider,
            script_engine,
            interceptor: None,
            context_summarizer: ctx_summarizer,
            context_summarization_config: ContextSummarizationConfig::default(),
            filler_task: None,
            llm_event_rx: None,
            swarm,
            config,
            conversation: Vec::new(),
            phase: Phase::Idle,
            tool_rounds: 0,
            tools_remaining: 0,
            pending_tool_info: HashMap::new(),
            tool_result_tx,
            tool_result_rx,
            event_buffer: VecDeque::new(),
            pending_tokens: Vec::new(),
            pending_tool_calls: Vec::new(),
            pending_hang_up: None,
            pending_escalate_call: None,
            is_telephony: false,
            escalation_destinations: Vec::new(),
            obs: None,
        }
    }

    /// Get the agent-wide timezone from the graph (if set).
    fn timezone(&self) -> Option<String> {
        self.swarm.as_ref().and_then(|s| s.graph.timezone.clone())
    }

    /// Configure telephony context for this session.
    ///
    /// When `is_telephony` is `true` and `destinations` is non-empty,
    /// the `escalate_call` tool is injected into the LLM tool schemas.
    pub fn with_telephony_escalation(
        mut self,
        is_telephony: bool,
        destinations: Vec<EscalationDestination>,
    ) -> Self {
        self.is_telephony = is_telephony;
        self.escalation_destinations = destinations;
        self
    }

    /// Set a tool interceptor for intercepting tool execution.
    ///
    /// Used by [`ArtifactInterceptor`] in production and optionally by callers for
    /// testing (stub or override tool results).  Last call wins — if you need
    /// to compose multiple interceptors, wrap them externally before passing in.
    pub fn with_interceptor(mut self, interceptor: Arc<dyn ToolInterceptor>) -> Self {
        self.interceptor = Some(interceptor);
        self
    }

    /// Resolve tool schemas, temperature, max_tokens, and model override for the current LLM call.
    ///
    /// When a swarm is active, the active node's settings take precedence over base config.
    /// Falls back to base config + runtime built-in tools (hang_up, artifacts) otherwise.
    fn resolve_llm_call_params(
        &self,
    ) -> Result<(Vec<serde_json::Value>, f64, u32, Option<String>), String> {
        if let Some(swarm) = &self.swarm {
            if let Some(node) = swarm.active_def() {
                let schemas = build_node_tool_schemas(
                    node,
                    &swarm.graph.tools,
                    self.is_telephony,
                    &self.escalation_destinations,
                );
                let temp = node.temperature.unwrap_or(self.config.temperature);
                let mt = node.max_tokens.unwrap_or(self.config.max_tokens);
                let model = node.model.clone();
                return Ok((schemas, temp, mt, model));
            } else {
                return Err(format!(
                    "Active node '{}' not found in agent graph",
                    swarm.active_node
                ));
            }
        }

        // No active swarm node — return the standard built-in tool set.
        // In practice all production agents use a graph, so this path is only
        // exercised by tests that construct the backend without a graph.
        Ok((
            vec![make_hang_up_tool_schema(), make_on_hold_tool_schema()]
                .into_iter()
                .chain(make_artifact_tool_schemas())
                .collect(),
            self.config.temperature,
            self.config.max_tokens,
            None,
        ))
    }

    /// Start an LLM stream using current conversation state (internal helper).
    async fn start_llm_stream(&mut self) -> Result<(), LlmProviderError> {
        // Resolve per-node config (swarm overrides base config).
        let (tool_schemas, temperature, max_tokens, model_override) = self
            .resolve_llm_call_params()
            .map_err(LlmProviderError::Provider)?;

        let call_config = LlmCallConfig {
            temperature,
            max_tokens,
            model: model_override,
        };

        let rx = self
            .provider
            .stream_completion(
                &self.conversation,
                if tool_schemas.is_empty() {
                    None
                } else {
                    Some(&tool_schemas)
                },
                &call_config,
            )
            .await?;

        self.obs = Some(LlmCallObs {
            start: std::time::Instant::now(),
            first_token: None,
            model: call_config
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            provider: self.provider.provider_name().to_string(),
            input_json: serde_json::to_string(&self.conversation).unwrap_or_default(),
            tools_json: if tool_schemas.is_empty() {
                None
            } else {
                serde_json::to_string(&tool_schemas).ok()
            },
            temperature: call_config.temperature,
            max_tokens: call_config.max_tokens,
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_read_tokens: None,
        });

        self.llm_event_rx = Some(rx);
        self.pending_tokens.clear();
        self.pending_tool_calls.clear();
        Ok(())
    }

    /// Push the accumulated assistant message (text + tool calls) to conversation.
    fn push_assistant_message(&mut self) -> Option<String> {
        let full_text = self.pending_tokens.join("");
        let has_text = !full_text.is_empty();
        let has_tools = !self.pending_tool_calls.is_empty();

        if !has_text && !has_tools {
            self.pending_tokens.clear();
            self.pending_tool_calls.clear();
            return None;
        }

        let tool_calls_json: Option<Vec<serde_json::Value>> = if has_tools {
            Some(
                self.pending_tool_calls
                    .iter()
                    .map(|tc| {
                        let mut v = serde_json::json!({
                            "id": tc.tool_call_id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        });
                        if let Some(sig) = &tc.signature {
                            v["signature"] = serde_json::json!(sig);
                        }
                        v
                    })
                    .collect(),
            )
        } else {
            None
        };

        self.conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: if has_text {
                Some(serde_json::Value::String(full_text.clone()))
            } else {
                None
            },
            tool_calls: tool_calls_json,
            tool_call_id: None,
        });

        self.pending_tokens.clear();
        self.pending_tool_calls.clear();

        if has_text {
            Some(full_text)
        } else {
            None
        }
    }

    /// Build an `AgentEvent::LlmComplete` from captured observability context and buffer it.
    ///
    /// No-ops if called when no stream is active (i.e., `obs` is `None`).
    /// Safe to call multiple times — idempotent after the first call per stream.
    fn emit_llm_complete(&mut self) {
        let Some(obs) = self.obs.take() else {
            return;
        };

        let text = self.pending_tokens.join("");
        let output_json = if self.pending_tool_calls.is_empty() {
            serde_json::json!({ "content": text }).to_string()
        } else {
            let tool_calls: Vec<serde_json::Value> = self
                .pending_tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "name": tc.name,
                        "arguments": tc.arguments,
                    })
                })
                .collect();
            serde_json::json!({ "content": text, "tool_calls": tool_calls }).to_string()
        };

        let span_label = if !self.pending_tool_calls.is_empty() {
            "llm_tool_req"
        } else if self.tool_rounds > 0 {
            "llm_tool_resp"
        } else {
            "llm"
        };

        self.event_buffer.push_back(AgentEvent::LlmComplete {
            provider: obs.provider,
            model: obs.model,
            input_json: obs.input_json,
            output_json,
            tools_json: obs.tools_json,
            temperature: obs.temperature,
            max_tokens: obs.max_tokens,
            duration_ms: obs.start.elapsed().as_secs_f64() * 1000.0,
            ttfb_ms: obs
                .first_token
                .map(|ft| ft.duration_since(obs.start).as_secs_f64() * 1000.0),
            prompt_tokens: obs.prompt_tokens,
            completion_tokens: obs.completion_tokens,
            cache_read_tokens: obs.cache_read_tokens,
            span_label: span_label.to_string(),
        });
    }

    /// Spawn a tool execution task.
    ///
    /// If a `ToolInterceptor` is set, it is consulted before and after execution:
    /// - `before_tool_call` can return `Stub(result)` to skip execution entirely.
    /// - `after_tool_call` can return `Override(result)` to replace the real result.
    fn spawn_tool(
        &mut self,
        call_id: String,
        name: String,
        args: String,
        side_effect: bool,
        summarize: bool,
    ) {
        // Some streaming providers can emit duplicate tool-call events for the same
        // call_id (e.g. retry/delta edge cases). Guard against double-counting, which
        // would leave `tools_remaining` stuck > 0 forever.
        if self.pending_tool_info.contains_key(&call_id) {
            warn!(
                "[agent_backend] duplicate tool call id ignored: {} ({})",
                call_id, name
            );
            return;
        }

        // Spawn filler generator for side-effecting tools only when enabled.
        // We ensure only one filler task runs per wait-batch by checking `.is_none()`.
        if side_effect && self.config.tool_filler && self.filler_task.is_none() {
            let provider = Arc::clone(&self.provider);
            let tool_name = name.clone();
            let user_msg = self
                .conversation
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .and_then(|m| m.content.as_ref())
                .map(|val| match val {
                    serde_json::Value::String(s) => s.clone(),
                    v => v.to_string(),
                })
                .unwrap_or_default();

            self.filler_task = Some(tokio::spawn(async move {
                micro_tasks::generate_tool_filler(&*provider, &tool_name, &user_msg).await
            }));
        }

        let before = self.tools_remaining;
        self.pending_tool_info.insert(call_id.clone(), name.clone());
        self.tools_remaining += 1;
        info!(
            "[agent_backend] spawn_tool: id={} name={} tools_remaining {}->{} pending_info={}",
            call_id,
            name,
            before,
            self.tools_remaining,
            self.pending_tool_info.len()
        );

        spawn_tool_task(
            call_id,
            name,
            args,
            side_effect,
            summarize,
            self.script_engine.clone(),
            self.interceptor.clone(),
            Some(Arc::clone(&self.provider)),
            self.tool_result_tx.clone(),
        );
    }

    /// Record a `hang_up` tool call as a deferred marker.
    ///
    /// We do **not** close the LLM stream or call `push_assistant_message` here.
    /// Instead we let the stream run to `None` so that any tool calls and tokens
    /// that appear *after* `hang_up` in the same response are still processed.
    /// The actual `AgentEvent::HangUp` is emitted once the stream ends and all
    /// sibling tool tasks have completed — see `handle_streaming_phase` (stream-end
    /// branch) and `handle_waiting_for_tools_phase`.
    fn handle_hang_up(&mut self, tc: &ToolCallEvent) {
        if self.pending_hang_up.is_some() {
            warn!("[agent_backend] duplicate hang_up in same response — ignoring");
            return;
        }
        let reason = serde_json::from_str::<serde_json::Value>(&tc.arguments)
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
            .unwrap_or_else(|| "agent_initiated".to_string());

        info!(
            "[agent_backend] hang_up deferred (tools_remaining={}): {}",
            self.tools_remaining, reason
        );
        self.pending_hang_up = Some(PendingHangUp {
            reason,
            content: None,
        });
        // Do NOT touch llm_event_rx, pending_tokens, or phase here.
        // The stream continues; hang_up is resolved at stream-end.
    }

    /// Record an `escalate_call` tool call as a deferred marker.
    ///
    /// Resolves the `destination_name` argument to its actual phone number
    /// using the pre-configured `escalation_destinations` list.
    /// Emitted as `AgentEvent::EscalateCall` once the stream ends and all
    /// sibling tool tasks complete.
    fn handle_escalate_call(&mut self, tc: &ToolCallEvent) {
        if self.pending_escalate_call.is_some() {
            warn!("[agent_backend] duplicate escalate_call in same response — ignoring");
            return;
        }
        let args: serde_json::Value = serde_json::from_str(&tc.arguments).unwrap_or_default();
        let destination_name = args
            .get("destination_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("user_request")
            .to_string();

        // Resolve destination name → phone number.
        // Hard-reject unknown destinations: passing a human-readable label to
        // the Telnyx/Twilio API would cause the REST call to fail silently.
        let Some(destination) = self
            .escalation_destinations
            .iter()
            .find(|d| d.name == destination_name)
            .map(|d| d.phone_number.clone())
        else {
            warn!(
                "[agent_backend] escalate_call: unknown destination '{}' — rejecting (not in configured list)",
                destination_name
            );
            return;
        };

        info!(
            "[agent_backend] escalate_call deferred: {} → {} (reason={})",
            destination_name, destination, reason
        );
        self.pending_escalate_call = Some(PendingEscalateCall { destination, reason });
    }

    fn handle_on_hold(&mut self, tc: &ToolCallEvent) {
        let duration_mins = serde_json::from_str::<serde_json::Value>(&tc.arguments)
            .ok()
            .and_then(|v| v.get("duration_mins").and_then(|d| d.as_u64()))
            .unwrap_or(3) as u32;

        info!(
            "[agent_backend] on_hold intercepted: mins={}",
            duration_mins
        );

        // Cap at 10 minutes to prevent abuse
        let duration_secs = (duration_mins * 60).clamp(60, 600);

        // We drop the tool call from conversation history to prevent "unmatched tool result"
        // errors from the OpenAI API. The side effect (AgentEvent::OnHold) is enough,
        // and the LLM's acknowledgement text ("Sure, I'll wait") will be saved normally.
        self.event_buffer
            .push_back(AgentEvent::OnHold { duration_secs });
    }

    async fn handle_streaming_phase(&mut self) -> Option<AgentEvent> {
        loop {
            let rx = self.llm_event_rx.as_mut()?;
            match rx.recv().await {
                Some(InnerLlmEvent::Token(t)) => {
                    // Capture TTFB on first token
                    if let Some(obs) = self.obs.as_mut() {
                        if obs.first_token.is_none() {
                            obs.first_token = Some(std::time::Instant::now());
                        }
                    }
                    self.pending_tokens.push(t.clone());
                    return Some(AgentEvent::Token(t));
                }
                Some(InnerLlmEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    cached_input_tokens,
                    ..
                }) => {
                    if let Some(obs) = self.obs.as_mut() {
                        obs.prompt_tokens = prompt_tokens;
                        obs.completion_tokens = completion_tokens;
                        obs.cache_read_tokens =
                            (cached_input_tokens > 0).then_some(cached_input_tokens);
                    }
                    // Usage is metadata-only — continue polling
                    continue;
                }
                Some(InnerLlmEvent::Error(msg)) => {
                    // Provider signalled a mid-stream failure.
                    // We must fully cancel() the turn to orphan any in-flight tools
                    // and sanitize the conversation history before going Idle.
                    let complete_event = self.cancel();
                    if let Some(ev) = complete_event {
                        self.event_buffer.push_back(ev); // Defer LlmComplete
                    }
                    return Some(AgentEvent::Error(msg));
                }
                Some(InnerLlmEvent::ToolCall(tc)) => {
                    // ── Intercept hang_up (synthetic runtime tool) ──
                    if tc.name == HANG_UP_TOOL_NAME {
                        self.handle_hang_up(&tc);
                        continue;
                    }

                    // ── Intercept on_hold (synthetic runtime tool) ──
                    if tc.name == ON_HOLD_TOOL_NAME {
                        self.handle_on_hold(&tc);
                        continue;
                    }

                    // ── Intercept escalate_call (synthetic telephony tool) ──
                    // Defer: let the LLM finish its farewell text, then emit EscalateCall.
                    if tc.name == ESCALATE_CALL_TOOL_NAME {
                        self.handle_escalate_call(&tc);
                        continue;
                    }

                    let id = tc
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
                    self.pending_tool_calls.push(ToolCallEvent {
                        tool_call_id: Some(id.clone()),
                        ..tc.clone()
                    });

                    let (side_effect, summarize) = self
                        .swarm
                        .as_ref()
                        .and_then(|s| s.graph.tools.get(&tc.name))
                        .map(|t| (t.side_effect, t.summarize_result))
                        .unwrap_or((false, false));

                    if side_effect {
                        tracing::debug!("[agent_backend] Tool '{}' marked as side-effect", tc.name);
                    }

                    // Spawn after resolving side_effect so the
                    // task can log it if the session dies first.
                    self.spawn_tool(
                        id.clone(),
                        tc.name.clone(),
                        tc.arguments.clone(),
                        side_effect,
                        summarize,
                    );

                    return Some(AgentEvent::ToolCallStarted {
                        id,
                        name: tc.name,
                        side_effect,
                    });
                }
                None => {
                    // LLM stream closed — seal this LLM call.
                    // `emit_llm_complete` must run before `push_assistant_message`
                    // because it reads `pending_tokens` / `pending_tool_calls`
                    // before they are drained.
                    self.emit_llm_complete();
                    self.llm_event_rx = None;

                    // push_assistant_message drains pending_tokens / pending_tool_calls
                    // into the conversation history and returns the text content.
                    let content = self.push_assistant_message();

                    // If hang_up was deferred, record the farewell content now
                    // that the stream is fully closed and the message is sealed.
                    if let Some(ref mut ph) = self.pending_hang_up {
                        ph.content = content.clone();
                    }

                    if self.tools_remaining == 0 {
                        self.phase = Phase::Idle;
                        // EscalateCall takes highest priority (overrides HangUp)
                        if let Some(pe) = self.pending_escalate_call.take() {
                            info!("[agent_backend] escalate_call resolved at stream-end");
                            // Clear any concurrent hang_up so the reactor only gets one shutdown signal.
                            self.pending_hang_up = None;
                            return Some(AgentEvent::EscalateCall {
                                destination: pe.destination,
                                reason: pe.reason,
                            });
                        }
                        // Pending hang_up takes priority over Finished.
                        if let Some(ph) = self.pending_hang_up.take() {
                            info!(
                                "[agent_backend] hang_up resolved at stream-end (no pending tools)"
                            );
                            return Some(AgentEvent::HangUp {
                                reason: ph.reason,
                                content: ph.content,
                            });
                        }
                        // Normal turn completion.
                        if let Some(ctx) = self.context_summarizer.as_mut() {
                            ctx.maybe_start(&self.conversation, &self.context_summarization_config);
                        }
                        return Some(AgentEvent::Finished { content });
                    }

                    // Tool calls pending — wait for them to drain.
                    // pending_hang_up (if set) will be consumed once the last
                    // tool completes in handle_waiting_for_tools_phase.
                    self.phase = Phase::WaitingForTools;
                    return None;
                }
            }
        }
    }

    async fn handle_waiting_for_tools_phase(&mut self) -> Option<AgentEvent> {
        tracing::debug!(
            "[agent_backend] WaitingForTools: pending={} waiting for next tool result",
            self.tools_remaining
        );
        let rx = &mut self.tool_result_rx;
        let result = if let Some(mut filler_task) = self.filler_task.take() {
            tokio::select! {
                // Tool finished first: cancel the active filler task
                // so a stale phrase doesn't leak into the next turn.
                res = rx.recv() => {
                    filler_task.abort();
                    res?
                }
                filler_res = &mut filler_task => {
                    // Task finished before the tool! Only yield if we got text.
                    if let Ok(Some(filler)) = filler_res {
                        return Some(AgentEvent::Token(filler));
                    }
                    // If it failed or yielded None, just wait for the tool.
                    rx.recv().await?
                }
            }
        } else {
            rx.recv().await?
        };

        info!(
            "[agent_backend] WaitingForTools: received tool result id={} name={} success={}",
            result.call_id, result.name, result.success
        );

        // Remove from pending info (best-effort).
        let removed = self.pending_tool_info.remove(&result.call_id).is_some();
        if !removed {
            warn!(
                "[agent_backend] received tool result for unknown call_id={} (name={})",
                result.call_id, result.name
            );
        }

        // IMPORTANT:
        // Do not await additional async work (e.g. tool summarizer) here.
        // This phase runs inside a recv loop that can be cancelled/repolled by
        // the reactor select loop; keeping this path await-free ensures
        // `tools_remaining` accounting is atomic once a result is received.
        let content = result.result.clone();

        let error_msg = (!result.success).then(|| content.clone());

        // Add tool result to conversation
        self.conversation.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(serde_json::Value::String(content)),
            tool_calls: None,
            tool_call_id: Some(result.call_id.clone()),
        });

        let before = self.tools_remaining;
        if self.tools_remaining == 0 {
            warn!(
                "[agent_backend] tools_remaining already zero when result arrived: id={} name={}",
                result.call_id, result.name
            );
        } else {
            self.tools_remaining -= 1;
        }
        info!(
            "[agent_backend] tool_result_accounting: id={} removed={} tools_remaining {}->{} pending_info={}",
            result.call_id,
            removed,
            before,
            self.tools_remaining,
            self.pending_tool_info.len()
        );

        let event = AgentEvent::ToolCallCompleted {
            id: result.call_id,
            name: result.name,
            success: result.success,
            error_message: error_msg,
        };

        if self.tools_remaining == 0 {
            // All tools done — check for deferred escalate/hang-up first.
            self.tool_rounds += 1;

            if let Some(pe) = self.pending_escalate_call.take() {
                // escalate_call takes priority over hang_up.
                info!("[agent_backend] escalate_call resolved after all tools completed");
                self.pending_hang_up = None;
                self.phase = Phase::Idle;
                self.event_buffer.push_back(AgentEvent::EscalateCall {
                    destination: pe.destination,
                    reason: pe.reason,
                });
            } else if let Some(ph) = self.pending_hang_up.take() {
                // hang_up was deferred while tools were in-flight.
                // Now that every tool has completed we can end the session.
                // Buffer HangUp so it is emitted *after* this ToolCallCompleted.
                info!("[agent_backend] hang_up resolved after all tools completed");
                self.phase = Phase::Idle;
                self.event_buffer.push_back(AgentEvent::HangUp {
                    reason: ph.reason,
                    content: ph.content,
                });
            } else if self.tool_rounds >= self.config.max_tool_rounds {
                warn!(
                    "[agent_backend] Max tool rounds ({}) reached",
                    self.config.max_tool_rounds
                );
                self.phase = Phase::Idle;
                self.event_buffer
                    .push_back(AgentEvent::Error("Max tool rounds reached".into()));
            } else {
                // Trigger context summarization before the next LLM call.
                // This handles tool-heavy sessions that never produce a
                // text-only turn (which is the only other trigger point).
                if let Some(ctx) = self.context_summarizer.as_mut() {
                    ctx.maybe_start(&self.conversation, &self.context_summarization_config);
                }
                // Start next LLM turn with tool results
                match self.start_llm_stream().await {
                    Ok(()) => {
                        self.phase = Phase::Streaming;
                    }
                    Err(e) => {
                        self.phase = Phase::Idle;
                        self.event_buffer
                            .push_back(AgentEvent::Error(e.to_string()));
                    }
                }
            }
        }

        Some(event)
    }
}

#[async_trait]
impl AgentBackend for DefaultAgentBackend {
    fn set_system_prompt(&mut self, prompt: String) {
        let tz = self.timezone();
        self.conversation = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(serde_json::Value::String(with_suffix(
                &prompt,
                tz.as_deref(),
            ))),
            tool_calls: None,
            tool_call_id: None,
        }];
    }

    fn add_user_message(&mut self, text: String) {
        self.conversation.push(ChatMessage {
            role: "user".to_string(),
            content: Some(serde_json::Value::String(text)),
            tool_calls: None,
            tool_call_id: None,
        });
        trim_history(&mut self.conversation);
    }

    fn add_assistant_message(&mut self, text: String) {
        self.conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(serde_json::Value::String(text)),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    async fn start_turn(&mut self) -> Result<(), LlmProviderError> {
        info!(
            "[agent_backend] start_turn: reset counters (prev tools_remaining={} pending_info={})",
            self.tools_remaining,
            self.pending_tool_info.len()
        );
        self.tool_rounds = 0;
        self.tools_remaining = 0;
        self.pending_tool_info.clear();
        self.event_buffer.clear();
        self.pending_hang_up = None;
        // Apply any summary that finished during the user's speaking window,
        // then check if a new summarization should start. Both happen before
        // the LLM stream begins.
        if let Some(ctx) = self.context_summarizer.as_mut() {
            ctx.try_apply(&mut self.conversation);
            ctx.maybe_start(&self.conversation, &self.context_summarization_config);
        }
        self.start_llm_stream().await?;
        self.phase = Phase::Streaming;
        Ok(())
    }

    async fn recv(&mut self) -> Option<AgentEvent> {
        // Drain buffered events first
        if let Some(event) = self.event_buffer.pop_front() {
            return Some(event);
        }

        loop {
            match self.phase {
                Phase::Idle => return None,
                Phase::Streaming => {
                    if let Some(event) = self.handle_streaming_phase().await {
                        return Some(event);
                    }
                }
                Phase::WaitingForTools => {
                    if let Some(event) = self.handle_waiting_for_tools_phase().await {
                        return Some(event);
                    }
                }
            }
        }
    }

    fn cancel(&mut self) -> Option<AgentEvent> {
        // Emit a partial LlmComplete for observability (pushed to the back of the buffer),
        // then immediately extract it — the buffer won't be drained after cancel.
        self.emit_llm_complete();
        let llm_complete = self.event_buffer.pop_back();

        self.llm_event_rx = None;

        // Reset the tool result channel so orphaned task results don't pollute the next turn.
        let (tx, rx) = mpsc::unbounded_channel();
        self.tool_result_tx = tx;
        self.tool_result_rx = rx;

        // Inject placeholder tool results so the conversation remains well-formed.
        for (call_id, _name) in std::mem::take(&mut self.pending_tool_info) {
            self.conversation.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(serde_json::Value::String(
                    "Tool execution was interrupted by the user.".to_string(),
                )),
                tool_calls: None,
                tool_call_id: Some(call_id),
            });
        }

        self.pending_tokens.clear();
        self.pending_tool_calls.clear();
        self.tools_remaining = 0;
        self.tool_rounds = 0;
        self.event_buffer.clear();
        self.pending_hang_up = None;
        self.phase = Phase::Idle;

        if let Some(ctx) = self.context_summarizer.as_mut() {
            ctx.cancel();
        }

        info!("[agent_backend] Cancelled");
        llm_complete
    }

    fn handle_transfer(&mut self, target_agent: &str) -> bool {
        if let Some(swarm) = &mut self.swarm {
            let ok = swarm.transfer_to(target_agent);
            if ok {
                info!("[agent_backend] Swarm transfer → '{}'", target_agent);

                // Swap the system prompt to the new node's prompt
                if let Some(node) = swarm.active_def() {
                    if let Some(first_msg) = self.conversation.first_mut() {
                        if first_msg.role == "system" {
                            let tz = swarm.graph.timezone.as_deref();
                            first_msg.content = Some(serde_json::Value::String(with_suffix(
                                &node.system_prompt,
                                tz,
                            )));
                        }
                    }
                }
            } else {
                warn!(
                    "[agent_backend] Swarm transfer to '{}' denied",
                    target_agent
                );
            }
            ok
        } else {
            false
        }
    }

    fn is_active(&self) -> bool {
        // Also true when buffered observability events (e.g. LlmComplete)
        // haven't been drained yet — ensures the reactor polls recv()
        // until all events are consumed.
        self.phase != Phase::Idle || !self.event_buffer.is_empty()
    }

    fn recent_messages(&self, n: usize) -> Vec<ChatMessage> {
        let mut msgs: Vec<_> = self
            .conversation
            .iter()
            .rev()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .take(n)
            .cloned()
            .collect();
        msgs.reverse();
        msgs
    }
}
