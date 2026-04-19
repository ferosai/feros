"""Builder Service — the vibe-code engine.

Takes a user's natural language description and produces a structured
agent graph using Pydantic AI. One agent, one call, one output.

The LLM decides everything:
  - Whether to ask clarifying questions or generate immediately
  - Whether to modify the existing config or just chat
  - What the agent graph should look like

No state machines. No multi-step pipelines. Just a good system prompt
and a capable LLM.
"""

from __future__ import annotations

import json
import re
from collections.abc import AsyncIterator, Callable
from typing import Any, cast

from loguru import logger
from pydantic_ai import (
    Agent,
    AgentRunResult,
    ModelMessagesTypeAdapter,
    RunContext,
    UsageLimits,
)
from pydantic_ai.messages import ModelMessage
from pydantic_core import to_jsonable_python
from sqlalchemy import or_, select

from app.agent_builder.context import compress_history
from app.agent_builder.deps import BuilderDeps, BuilderResult
from app.agent_builder.edit_ops import (
    EditOp,
    apply_edits,
    canonicalize_config,
)
from app.agent_builder.graph import (
    generate_graph_mermaid_llm,
    validate_graph,
)
from app.agent_builder.tools import register_all_tools
from app.agent_builder.tools.skills import skill_index
from app.lib.config import LLMConfig
from app.lib.config_utils import extract_secret_keys as _extract_secret_keys_impl
from app.lib.database import async_session
from app.lib.integration_registry import integration_registry
from app.lib.llm_factory import build_model
from app.lib.oauth_apps import get_all_enabled_oauth_apps
from app.models import Credential
from app.schemas.agent import ActionCardSchema

# ═══════════════════════════════════════════════════════════════════
# System Prompt
# ═══════════════════════════════════════════════════════════════════

BUILDER_SYSTEM_PROMPT = """\
You are the Voice Agent OS Agent Builder — an expert at designing voice AI agents for businesses.

Your job is to help users create and refine voice agents through natural conversation.

## CRITICAL RULES (NEVER VIOLATE)

- You build VOICE AGENT CONFIGURATIONS in JSON graph format. That is your ONLY job.
- NEVER generate Python scripts, code snippets, pseudocode, or programming examples.
- NEVER suggest the user write code or run scripts.
- When the user asks you to build an agent, ALWAYS produce an agent graph JSON config \
(see "Agent Graph Format" below).
- If no pre-built skill exists for the user's use case, design the agent graph yourself \
using the format below. You do NOT need a skill — skills are optional helpers.

## How to behave

- Think like a domain expert. Apply your knowledge of the user's industry to proactively \
suggest what data the agent should collect, what workflows it should follow, and what \
edge cases to handle. Don't just build what the user literally says — think about what \
they'll need.
- If the user's description is clear enough, generate the agent graph immediately — include \
any domain-relevant fields or steps the user didn't mention.
- If important details are missing, ask targeted questions. Prefer a short list of smart \
questions over many rounds of back-and-forth.
- If the user wants to modify an existing agent, prefer using `apply_agent_edits` to make \
surgical changes. Only change what the user asked for — do not touch node/tool IDs, \
system_prompt wording, tool scripts, or graph topology that was not mentioned.
- If the user is just chatting or asking questions, respond naturally without changing the config.

## Agent Graph Format

```json
{{
  "entry": "<node_id>",
  "nodes": {{
    "<node_id>": {{
      "system_prompt": "<str>",
      "greeting": "<optional greeting spoken when entering this node>",
      "tools": ["tool_id"],
      "edges": ["other_node_id"]
    }}
  }},
  "tools": {{
    "<tool_id>": {{
      "description": "<str>",
      "params": [{{"name": "<str>", "type": "string", "required": true}}],
      "script": "<quickjs_code>",
      "side_effect": false
    }}
  }}
}}
```

- **entry**: The starting node of the conversation
- **nodes**: Each node has a `system_prompt`, optional `greeting`, `tools` and `edges`
- **greeting**: Optional. The first message spoken when the conversation starts (entry node only).
- **tools**: Each tool has a `description`, `params`, a QuickJS `script`, and a `side_effect` flag

## QuickJS Tool Rules

Every tool is a standard QuickJS script. Your script is wrapped in a function, so you MUST use `return` to return a value.
For robust error handling, ALWAYS return a structured object instead of a plain string.
- Success: `return { result: ... }`
- Failure: `return { error: "Error details..." }`

Available global functions:
- `http_get(url)`, `http_post(url, body_object)`, `http_put(url, body_object)`, `http_delete(url)` → returns `{status: int, body: string}`
- `http_get_h`, `http_post_h`, `http_put_h`, `http_delete_h` → identical, but accept a `headers_object` as the final argument
- `secret("key_name")` → retrieves a credential (API key, token)
- `file_read(path)` → read a file from sandbox
- `file_write(path, content)` → write a file to sandbox
- `log(message)` → log a message

## Side Effects

Set `"side_effect": true` on any tool that **writes, creates, updates, or deletes** data.
The runtime uses this to ask for user confirmation before executing write operations.
Getting this flag wrong is DANGEROUS:
- Missing `"side_effect": true` on a write tool → a user cough could cancel a booking mid-flight.
- Unnecessary `"side_effect": true` on a read tool → the agent feels unresponsive during lookups.

| Function                                                   | Side effect           |
|------------------------------------------------------------|-----------------------|
| `http_get`, `http_get_h`                                   | `false` — read-only   |
| `http_post`, `http_put`, `http_delete` (and `_h` variants) | `true`  — writes data |
| `file_read`                                                | `false` — read-only   |
| `file_write`                                               | `true`  — writes data |
| `secret`, `log`                                            | `false` — no mutation |

Example tool scripts:
```javascript
// Simple API call
let resp = http_get(`https://api.example.com/rooms?date=${check_in}`);
return { result: resp.body };

// Authenticated API call
let key = secret("api_key");
let resp = http_get_h(
    `https://api.airtable.com/v0/appXXX/Table1`,
    { "Authorization": "Bearer " + key }
);
if (resp.status === 200) {
    return { result: resp.body };
} else {
    return { error: "Airtable error: " + resp.body };
}

// POST with body
let resp = http_post("https://api.example.com/bookings", {
    guest_name: guest_name,
    check_in: check_in,
    room_type: room_type
});
if (resp.status === 200) {
    return { result: `Booking confirmed! ID: ${resp.body}` };
} else {
    return { error: `Sorry, booking failed: ${resp.body}` };
}
```

## Node Design — ALLOWED TOPOLOGIES

You may ONLY produce one of two graph shapes:

### 1. Single-Node Graph (DEFAULT — use this unless you have a strong reason not to)
Use ONE node for the entire agent. The single node's system_prompt should contain ALL \
the agent's instructions, personality, conversation flow, and data-gathering logic. \
This is the RIGHT and ONLY choice for:
- Booking, scheduling, or reservation agents
- FAQ / informational agents
- Lead capture agents
- Customer service agents with a single topic
- ANY agent that follows a linear conversation flow (greet → gather info → confirm)

**CRITICAL**: A flow like "greet → ask name → ask date → confirm" is NOT multiple nodes. \
It is ONE node with ONE system_prompt that describes the entire conversation flow. \
The LLM inside that node drives the multi-turn conversation naturally.

Do NOT split this into separate "greeting", "ask_name", "ask_date", "confirm" nodes. That is WRONG.

### 2. Dispatcher Star Graph (only when truly needed)
Use ONLY when the agent has genuinely distinct expertise areas that need \
different system prompts (e.g. "General Support" vs "Billing Expert" vs "Tech Support").

Rules for star graphs:
- The entry node is the **dispatcher** — it routes to specialists.
- Every specialist node MUST have an edge back to the dispatcher.
- No chains: specialist → specialist is FORBIDDEN.
- Each specialist node must return to the dispatcher when its task is done.

### FORBIDDEN patterns
- ❌ Linear chains: A → B → C → D → E (use a SINGLE node instead)
- ❌ Nodes that don't edge back to dispatcher
- ❌ Splitting a simple conversation into many trivial nodes (greet, ask_name, confirm, etc.)
- ❌ Creating nodes named after conversation steps (greeting, collect_info, confirm_booking)

## Runtime Tools (auto-injected — do NOT define these in the tools map)

The runtime automatically provides these tools to every agent:

- `transfer_to(agent_id)` — transfer the conversation to another agent node \
(auto-generated from edges). Only available when the node has outgoing edges.
- `hang_up(reason)` — end the call. The agent should call this after saying goodbye \
when the conversation is complete or there is nothing left to do.

You should instruct the agent in its `system_prompt` to call `hang_up` when appropriate. \
For example: "After confirming the booking and saying goodbye, call hang_up to end the call." \
Do NOT create a tool definition for `hang_up` — it is always available automatically.

## Credential Security

NEVER ask for API keys in chat. Use `secret("skill_name")` in tool scripts — where \
`skill_name` is the **exact registered skill name** (e.g. `secret("airtable")`, \
`secret("slack")`). Then include an `action_card` with the same `skill` value so the \
user can configure credentials via the UI.

## Connection Introspection

Tools: `check_connection(provider)` and `api_call(provider, method, path)`. \
Auth headers are injected automatically — never ask users for tokens.

{connection_status}

If connected: try `api_call` to discover resources, use real IDs in config. \
If discovery fails or unavailable, ask the user. \
If not connected: use `secret("provider")` in scripts — action card emits automatically.


## Integration Skills

You have access to `search_skills` and `load_skill` tools.
{skill_summary}

When the user mentions a third-party service, API, integration, webhook, or
specific platform (for example Google Calendar, Slack, Airtable, Make.com,
Zapier, or a custom webhook URL):
1. Call `search_skills(query)` to find matching integrations
2. If `search_skills` returns a matching skill, you MUST call `load_skill(name)`
   before generating or editing any tool script, auth flow, `secret(...)` call,
   or config JSON
3. Follow the loaded skill's instructions for correct API endpoints, auth
   patterns, credential handling, and config format

`search_skills` results are only a directory. They are NOT enough to implement
the integration safely. Never rely on search results alone when a matching
skill exists.

Do NOT guess API endpoints or auth flows. Do NOT hardcode credential values or
user-provided auth header names into scripts when the loaded skill provides a
credential-based pattern.

## Artifacts (Notes + Uploaded Files)

You have `save_artifact`, `read_artifact`, `search_artifact`, and \
`list_artifacts` tools. One interface for both YOUR persistent notes and \
the user's uploaded files.

### Your notes
Save anything worth keeping across turns:
- **Requirements**: collected details → "requirements.md"
- **Task list**: progress tracker → "task_list.md" (`- [x]` done / `- [ ]` pending)
- **Integration notes**: API endpoints, auth patterns → "integrations.md"
- **Design notes**: architecture decisions → "design_notes.md"

When context seems missing (e.g. after history trimming), call `list_artifacts()` \
then `read_artifact(name)` to recover earlier context. Use `search_artifact(name, "keyword")` \
to find specific content without reading the whole artifact.

### User uploads (PDF, DOCX, TXT, MD, images)
When a user uploads a file, a message appears with the `file_id` and filename. \
Use that `file_id` with the same artifact tools:
- `read_artifact(file_id)` — read the first 800 lines
- `read_artifact(file_id, start_line=801, end_line=1600)` — read a specific range
- `search_artifact(file_id, "keyword")` — find specific content
- For images, `read_artifact(file_id)` returns a base64 data URL

Uploaded files are ephemeral. If unavailable, ask the user to re-upload.

## Web Search

You have a `web_search(query)` tool for researching anything useful.
Use it when:
- You need API docs, endpoints, or auth patterns for an integration
- The agent's domain has best practices or compliance requirements worth looking up
- You want to understand industry workflows or competitor conversation patterns
- You need technical specs, pricing, or rate limits for a service

Keep queries focused, e.g. "HIPAA compliance voice agent requirements".

## Current Agent

Name: {{agent_name}}
Current Config: {{current_config}}
"""

# ═══════════════════════════════════════════════════════════════════
# Helpers
# ═══════════════════════════════════════════════════════════════════


def _collect_javascript_errors(
    tools: dict[str, Any],
    validate_fn: Callable[[str], list[str]],
) -> list[str]:
    """Run a JS validator over every tool script and collect errors."""
    errors: list[str] = []
    if not tools:
        return errors
    for tool_id, tool_def in tools.items():
        if not isinstance(tool_def, dict):
            continue
        script = str(tool_def.get("script", ""))
        if not script:
            continue
        for err in validate_fn(script):
            errors.append(f"Tool '{tool_id}': {err}")
    return errors


def _normalize_escaped_tool_scripts(cfg: dict[str, Any]) -> dict[str, Any]:
    """Decode escaped newlines in tool scripts when validation proves that is the intent.

    Builder edit tool calls sometimes arrive with script bodies encoded as a
    single-line string containing literal ``\\n`` sequences. QuickJS validates
    those raw backslash characters as source code and reports a syntax error.
    We only rewrite the script when the raw version fails validation and the
    decoded version succeeds.
    """
    tools = cfg.get("tools")
    if not isinstance(tools, dict):
        return cfg

    try:
        from voice_engine import validate_javascript

        has_js_validator = True
    except ImportError:

        def validate_javascript(script: str) -> list[str]:
            return []

        has_js_validator = False

    for tool_def in tools.values():
        if not isinstance(tool_def, dict):
            continue
        script = tool_def.get("script")
        if not isinstance(script, str):
            continue
        if "\\n" not in script and "\\r\\n" not in script:
            continue

        # Protect properly escaped inner backslashes (e.g. \\n) from being
        # mangled when we blindly unescape the artificial \n and \r\n sequences
        decoded = (
            script.replace("\\\\", "\x00")
            .replace("\\r\\n", "\r\n")
            .replace("\\n", "\n")
            .replace("\x00", "\\\\")
        )

        if decoded == script:
            continue

        if not has_js_validator:
            if "\n" not in script and "\n" in decoded:
                tool_def["script"] = decoded
            continue

        raw_errors = validate_javascript(script)
        if not raw_errors:
            continue
        decoded_errors = validate_javascript(decoded)
        if decoded_errors:
            continue

        tool_def["script"] = decoded

    return cfg


_STREAM_AGENT_ADDENDUM_BASE = """

## Tool-Based Config Output (MANDATORY)

RULES:
- NEVER put the agent graph JSON in your text reply. ALWAYS use one of the config tools below.
- Do NOT echo, summarize, or print the config JSON in your conversational text. The tool handles \
persistence — printing it wastes context and confuses users.
- NEVER write tool calls as text. Use the function-calling mechanism to invoke tools.
- NEVER generate Python scripts, code, or pseudocode. Your output is agent graph JSON ONLY.
- If no matching skill is found via `search_skills`, design the agent graph yourself using the \
Agent Graph Format from your instructions. Skills are optional — you can always build without them.
"""

_STREAM_ADDENDUM_CREATE = """
## Creating a New Agent

Use `save_agent_config` to submit the COMPLETE agent graph JSON.

Workflow:
1. Write a brief conversational reply as normal text (this gets streamed to the user)
2. Call `save_agent_config` to save the config (do NOT write the tool call as text)
3. Optionally continue with any follow-up text after the tool executes
"""

_STREAM_ADDENDUM_EDIT = """
## Editing an Existing Agent (Continuous Editing Mode)

You have two tools for config changes. Default to `apply_agent_edits` for modifications:

### `apply_agent_edits(edits, change_summary)` — DEFAULT for modifications
Submit a list of surgical edit operations. The server applies them to the current config. \
Only describe what changed — everything else is preserved automatically.

Available edit ops:
- `{{"op": "set_top_level", "key": "language|timezone|voice_id", "value": "..."}}`
- `{{"op": "set_entry", "node_id": "..."}}`
- `{{"op": "set_node_fields", "node_id": "...", "fields": {{"system_prompt": "...", "greeting": "...", "tools": [...], "edges": [...]}}}}`
  — Only include the fields you want to change. Unchanged fields are preserved.
  — IMPORTANT: `tools` and `edges` are COMPLETE REPLACEMENT lists. When adding/removing items, \
include ALL items you want in the final list, not just the new ones.
- `{{"op": "upsert_tool", "tool_id": "...", "fields": {{"description": "...", "script": "...", ...}}}}`
  — For existing tools, only include fields you want to change (shallow merge).
  — For new tools, include the full definition.
- `{{"op": "delete_tool", "tool_id": "..."}}` — auto-cleans node references
- `{{"op": "add_node", "node_id": "...", "fields": {{"system_prompt": "...", ...}}}}`
  — system_prompt is required; tools/edges default to [].
- `{{"op": "delete_node", "node_id": "..."}}` — auto-cleans edge references; cannot delete entry node

### `save_agent_config(config, change_summary)` — for full rewrites only
Submit the COMPLETE agent graph JSON. Use ONLY when:
- The user explicitly asks to "redo", "rebuild", or "start over"
- The change is so large that describing edits would be harder than rewriting

Workflow:
1. Write a brief conversational reply as normal text
2. Call `apply_agent_edits` (or `save_agent_config` for full rewrites)
3. Optionally continue with any follow-up text
"""


# ═══════════════════════════════════════════════════════════════════
# Builder Service
# ═══════════════════════════════════════════════════════════════════


class BuilderService:
    """One agent, one call, one output.

    The LLM decides whether to ask questions, generate a config,
    modify the existing config, or just chat. No state machines.
    """

    # Tools whose stream events are hidden from the chat panel.
    # They still execute and fill deps, but their tool-call / tool-return
    # events are NOT forwarded to the SSE stream.
    _HIDDEN_TOOLS: set[str] = {"save_agent_config", "apply_agent_edits", "ask_user"}

    def __init__(self, llm_config: LLMConfig | None = None) -> None:
        cfg = llm_config or LLMConfig()
        model, self._model_settings = build_model(cfg)
        self._active_llm_config = cfg

        # Streaming agent: plain text output + save_agent_config tool
        self.stream_agent: Agent[BuilderDeps, str] = Agent(
            model=model,
            output_type=str,
            deps_type=BuilderDeps,
        )
        register_all_tools(self.stream_agent)
        self._attach_stream_tools()

        # System prompt registration
        self._attach_system_prompt()

        self._compression_model = model

    def _attach_system_prompt(self) -> None:
        """Register the system prompt on the stream agent."""

        def _make_prompt(ctx: RunContext[BuilderDeps]) -> str:
            current_config = ctx.deps.current_config
            if current_config is None:
                config_str = "None (new agent — no config yet)"
            else:
                config_str = json.dumps(current_config, indent=2)

            return (
                BUILDER_SYSTEM_PROMPT.replace("{{agent_name}}", ctx.deps.agent_name)
                .replace("{{current_config}}", config_str)
                .replace("{skill_summary}", skill_index.summary_line())
                .replace(
                    "{connection_status}",
                    ctx.deps.connection_status or "No integrations checked.",
                )
            )

        @self.stream_agent.system_prompt
        def get_stream_system_prompt(ctx: RunContext[BuilderDeps]) -> str:
            base = _make_prompt(ctx) + _STREAM_AGENT_ADDENDUM_BASE
            if ctx.deps.current_config is None:
                return base + _STREAM_ADDENDUM_CREATE
            return base + _STREAM_ADDENDUM_EDIT

    def _attach_stream_tools(self) -> None:
        """Register internal tools on the stream agent (hidden from chat)."""

        @self.stream_agent.tool
        async def ask_user(
            ctx: RunContext[BuilderDeps],
            question: str,
        ) -> str:
            """Pause and wait for the user's response before continuing.

            Call this when you need user input before proceeding — for example,
            confirming which resources to use, which fields to add, or any
            decision that affects the agent configuration.

            Your text response in this same turn will be shown to the user as
            the question. The agent loop will pause until they reply.

            Args:
                question: Brief summary of what you're asking.
            """
            ctx.deps.pause_requested = True
            return "Paused. Waiting for user response."

        @self.stream_agent.tool
        async def save_agent_config(
            ctx: RunContext[BuilderDeps],
            config: str,
            change_summary: str = "",
        ) -> str:
            """Save the complete agent graph JSON config.

            Call this tool whenever you generate or modify the agent configuration.
            Pass the COMPLETE graph JSON as the `config` argument and a brief
            description of what changed as `change_summary`.
            """
            # Strip markdown code fences the LLM may wrap around JSON
            cleaned = config.strip()
            if cleaned.startswith("```"):
                first_nl = cleaned.find("\n")
                if first_nl != -1:
                    cleaned = cleaned[first_nl + 1 :]
                if cleaned.endswith("```"):
                    cleaned = cleaned[:-3].strip()

            parsed: dict[str, Any] | None = None
            try:
                # strict=False: LLMs commonly put literal newlines in JSON
                # strings (e.g. in system_prompt) instead of \n escapes.
                parsed = json.loads(cleaned, strict=False)
            except json.JSONDecodeError as e:
                logger.warning(
                    "save_agent_config: JSON parse failed ({}). Raw input (first 500 chars): {}",
                    e,
                    config[:500],
                )
                return (
                    f"ERROR: invalid JSON — {e}. Send raw JSON without markdown fences."
                )

            ctx.deps.emitted_config = canonicalize_config(
                _normalize_escaped_tool_scripts(parsed)
            )
            ctx.deps.emitted_change_summary = change_summary or None
            return f"Config saved. Summary: {change_summary}"

        @self.stream_agent.tool
        async def apply_agent_edits(
            ctx: RunContext[BuilderDeps],
            edits: list[EditOp],
            change_summary: str = "",
        ) -> str:
            """Apply surgical edit operations to the current agent config.

            Use this tool (instead of save_agent_config) when modifying an
            existing agent. Only describe what you want to change — everything
            else is preserved automatically.

            Args:
                edits: List of edit operations. Each must have an "op" field
                    that determines its type. See the EditOp schema for details.
                change_summary: Brief description of what changed.
            """
            if ctx.deps.current_config is None:
                return (
                    "ERROR: no current config to edit. "
                    "Use save_agent_config to create the initial config."
                )

            # Apply edits to current config
            try:
                merged = apply_edits(ctx.deps.current_config, edits)
            except ValueError as e:
                logger.warning("apply_agent_edits: apply failed: {}", e)
                return f"ERROR: edit apply failed — {e}"

            ctx.deps.emitted_config = canonicalize_config(
                _normalize_escaped_tool_scripts(merged)
            )
            ctx.deps.emitted_change_summary = change_summary or None
            ctx.deps.used_edit_path = True
            return f"Edits applied. Summary: {change_summary}"

    def reconfigure(self, llm_config: LLMConfig) -> None:
        """Hot-swap the LLM model without restarting the server."""
        model, self._model_settings = build_model(llm_config)
        self._active_llm_config = llm_config

        self.stream_agent = Agent(
            model=model,
            output_type=str,
            deps_type=BuilderDeps,
        )
        register_all_tools(self.stream_agent)
        self._attach_stream_tools()
        # Update the compression LLM too
        self._compression_model = model
        # Must come after stream_agent is created since it decorates it
        self._attach_system_prompt()

        logger.info(
            "Builder LLM reconfigured: provider={}, model={}",
            llm_config.provider,
            llm_config.model,
        )

    def current_llm_config(self) -> LLMConfig:
        """Return the active builder LLM config (with DB override applied)."""
        return self._active_llm_config

    def _collect_all_errors(self, cfg: dict[str, Any]) -> list[str]:
        """Run graph validation + JS validation and return all errors."""
        errors = validate_graph(cfg)
        try:
            from voice_engine import validate_javascript

            js_errors = _collect_javascript_errors(
                cfg.get("tools", {}), validate_javascript
            )
            errors.extend(js_errors)
        except ImportError:
            pass
        return errors

    async def process_message_stream(
        self,
        user_message: str,
        current_config: dict[str, Any] | None,
        agent_name: str,
        agent_id: str = "",
        *,
        current_version: int | None = None,
        persisted_context: list[Any] | None = None,
        previous_mermaid: str | None = None,
    ) -> AsyncIterator[dict[str, Any] | BuilderResult]:
        """Stream structured events from the builder LLM, then yield a final BuilderResult.

        Yields:
          - dict: structured events (part_start, part_delta, tool_call, tool_return, mermaid_start)
          - BuilderResult: final result with config/action_cards/mermaid (last item)
        """
        # Pre-fetch connection status for system prompt injection
        from app.agent_builder.tools.connections import get_connection_status

        connection_status = await get_connection_status(agent_id, current_config)

        deps = BuilderDeps(
            agent_id=agent_id,
            agent_name=agent_name,
            current_config=current_config,
            current_version=current_version,
            connection_status=connection_status,
        )

        # Deserialize persisted history and compress if needed
        message_history: list[ModelMessage] = (
            ModelMessagesTypeAdapter.validate_python(persisted_context)
            if persisted_context
            else []
        )
        message_history = await compress_history(
            self._compression_model, message_history
        )

        # ── Stream with agent.iter() for full event visibility ────
        all_messages: list[ModelMessage] = []
        try:
            async for event in self._iter_agent_events(
                user_message, deps, message_history
            ):
                if isinstance(event, dict):
                    yield event
                else:
                    # AgentRunResult
                    all_messages = list(event.all_messages())
        except Exception:
            logger.exception("Builder LLM stream error")
            yield BuilderResult(
                config=None,
                change_summary=None,
            )
            return

        config = deps.emitted_config
        change_summary = deps.emitted_change_summary
        logger.info(
            "After stream: emitted_config={}, change_summary={}",
            "SET" if config is not None else "NONE",
            change_summary or "NONE",
        )

        # ── Validation + correction stream ───────────────────────
        if config:
            errors = self._collect_all_errors(config)

            if errors:
                error_list = "\n".join(f"- {e}" for e in errors)
                logger.warning(
                    "Streamed config has {} error(s), requesting correction",
                    len(errors),
                )

                # Detect topology issues directly from graph structure
                # rather than parsing error message strings.
                nodes = config.get("nodes", {})
                is_topology_error = isinstance(nodes, dict) and len(nodes) > 1
                topology_hint = ""
                if is_topology_error:
                    topology_hint = (
                        "\n\nTOPOLOGY FIX: Your graph has a chain or invalid multi-node "
                        "layout. For booking agents, FAQ agents, or any linear conversation "
                        "flow, COLLAPSE everything into a SINGLE node. Put the entire "
                        "conversation flow (greet, collect info, confirm) into one node's "
                        "system_prompt. Remove all other nodes.\n"
                    )

                # Choose correction tool based on original path
                was_edit_path = deps.used_edit_path
                if was_edit_path:
                    correction_tool_hint = (
                        "Fix ALL of the following errors by calling "
                        "apply_agent_edits with corrective edit ops. "
                        "Do NOT fall back to save_agent_config."
                    )
                else:
                    correction_tool_hint = (
                        "Fix ALL of the following errors and call "
                        "save_agent_config with the corrected COMPLETE config."
                    )

                directive = (
                    "<system-directive>\n"
                    f"{correction_tool_hint} "
                    "Keep your reply brief — focus on what you fixed.\n\n"
                    f"{error_list}{topology_hint}\n"
                    "</system-directive>"
                )

                # Snapshot messages before correction so we can revert on failure
                pre_correction_messages = list(all_messages)
                correction_base_config = deps.current_config
                correction_base_changed = False

                try:
                    # Reset so we can detect if correction actually emits a new config
                    deps.emitted_config = None
                    deps.emitted_change_summary = None
                    deps.used_edit_path = False
                    if was_edit_path:
                        deps.current_config = config
                        correction_base_changed = True
                        logger.info(
                            "Correction stream using failed candidate as edit base "
                            "(tools={})",
                            sorted(config.get("tools", {}).keys()),
                        )
                    else:
                        logger.info(
                            "Correction stream using original current config as base"
                        )

                    async for event in self._iter_agent_events(
                        directive, deps, all_messages
                    ):
                        if isinstance(event, dict):
                            yield event
                        else:
                            all_messages = list(event.all_messages())

                    # Cast to Any then back to dict to satisfy mypy's reachable/non-None analysis
                    fix_config_checked = cast(Any, deps.emitted_config)
                    if fix_config_checked:
                        remaining = self._collect_all_errors(
                            cast(dict[str, Any], fix_config_checked)
                        )
                        if not remaining:
                            logger.info("Stream self-correction succeeded")
                            config = fix_config_checked
                            change_summary = (
                                deps.emitted_change_summary or change_summary
                            )
                        else:
                            logger.warning(
                                "Stream self-correction left {} error(s), discarding config",
                                len(remaining),
                            )
                            config = None
                    else:
                        logger.warning(
                            "Correction stream produced no config, discarding original"
                        )
                        config = None
                except Exception:
                    logger.exception("Stream self-correction failed")
                    config = None
                    # Revert to pre-correction messages to avoid persisting
                    # the failed correction attempt's context
                    all_messages = pre_correction_messages
                finally:
                    if correction_base_changed:
                        deps.current_config = correction_base_config

        # ── Build final result ───────────────────────────────────
        result = BuilderResult(
            config=config,
            change_summary=change_summary,
            message_history=to_jsonable_python(all_messages),
            used_edit_path=deps.used_edit_path,
            base_version=deps.current_version,
        )

        if result.config:
            result = await self._ensure_action_cards(result, agent_id)
            result = self._ensure_side_effects(result)
            cfg = result.config or {}
            cfg["config_schema_version"] = "v3_graph"
            yield {"kind": "mermaid_start"}
            result.mermaid_diagram = await generate_graph_mermaid_llm(
                cfg,
                self.stream_agent.model,
                previous_mermaid=previous_mermaid,
                change_summary=change_summary,
            )

        yield result

    async def _iter_agent_events(
        self,
        user_prompt: str,
        deps: BuilderDeps,
        message_history: list[ModelMessage],
    ) -> AsyncIterator[dict[str, Any] | AgentRunResult[str]]:
        """Run agent.iter() and yield structured events for each part.

        Yields:
          - dict: structured events (part_start, part_delta, tool_call, tool_return)
          - AgentRunResult: the final result (last item, for message access)

        Hidden tools (like save_agent_config) still execute internally but
        their events are filtered out — the chat panel never sees the config JSON.
        """
        from pydantic_ai import CallToolsNode, ModelRequestNode
        from pydantic_ai.messages import (
            FunctionToolCallEvent,
            FunctionToolResultEvent,
            ModelRequest,
            ModelRequestPart,
            PartDeltaEvent,
            PartStartEvent,
            TextPart,
            TextPartDelta,
            ThinkingPart,
            ThinkingPartDelta,
            ToolCallPart,
            ToolCallPartDelta,
        )

        async with self.stream_agent.iter(
            user_prompt=user_prompt,
            deps=deps,
            message_history=message_history,
            model_settings=self._model_settings,
            usage_limits=UsageLimits(request_limit=16),
        ) as agent_run:
            async for node in agent_run:
                if isinstance(node, ModelRequestNode):
                    # ModelRequestNode streams model response events
                    # (text deltas, thinking deltas, tool call parts)
                    _hidden_part_indices: set[int] = set()

                    async with node.stream(agent_run.ctx) as stream:
                        async for event in stream:
                            if isinstance(event, PartStartEvent):
                                part = event.part
                                if isinstance(part, TextPart):
                                    yield {"kind": "part_start", "part_kind": "text"}
                                    if part.content:
                                        yield {
                                            "kind": "part_delta",
                                            "part_kind": "text",
                                            "content": part.content,
                                        }
                                elif isinstance(part, ThinkingPart):
                                    yield {
                                        "kind": "part_start",
                                        "part_kind": "thinking",
                                    }
                                    if part.content:
                                        yield {
                                            "kind": "part_delta",
                                            "part_kind": "thinking",
                                            "content": part.content,
                                        }
                                elif isinstance(part, ToolCallPart):
                                    # Skip hidden tools — don't stream to chat
                                    if part.tool_name in self._HIDDEN_TOOLS:
                                        _hidden_part_indices.add(event.index)
                                    else:
                                        yield {
                                            "kind": "part_start",
                                            "part_kind": "tool-call",
                                            "tool_name": part.tool_name,
                                            "args": (
                                                part.args
                                                if isinstance(part.args, str)
                                                else json.dumps(part.args)
                                            ),
                                        }
                            elif isinstance(event, PartDeltaEvent):
                                if getattr(event, "index", -1) in _hidden_part_indices:
                                    continue

                                delta = event.delta
                                if isinstance(delta, TextPartDelta):
                                    yield {
                                        "kind": "part_delta",
                                        "part_kind": "text",
                                        "content": delta.content_delta,
                                    }
                                elif isinstance(delta, ThinkingPartDelta):
                                    yield {
                                        "kind": "part_delta",
                                        "part_kind": "thinking",
                                        "content": delta.content_delta,
                                    }
                                elif isinstance(delta, ToolCallPartDelta):
                                    payload: dict[str, str] = {
                                        "kind": "part_delta",
                                        "part_kind": "tool-call",
                                    }
                                    if delta.args_delta:
                                        payload["content"] = (
                                            delta.args_delta
                                            if isinstance(delta.args_delta, str)
                                            else json.dumps(delta.args_delta)
                                        )
                                    name_delta = getattr(delta, "tool_name_delta", None)
                                    if isinstance(name_delta, str) and name_delta:
                                        payload["tool_name"] = name_delta

                                    if "content" in payload or "tool_name" in payload:
                                        yield payload
                elif isinstance(node, CallToolsNode):
                    # CallToolsNode streams tool execution events
                    # (function calls starting, function results).
                    # Collect tool-return parts so we can commit them to
                    # history if we need to break the loop early.
                    tool_result_parts: list[ModelRequestPart] = []
                    async with node.stream(agent_run.ctx) as handle_stream:
                        async for tool_event in handle_stream:
                            if isinstance(tool_event, FunctionToolCallEvent):
                                pass
                            elif isinstance(tool_event, FunctionToolResultEvent):
                                tool_result_parts.append(tool_event.result)
                                tool_name = tool_event.result.tool_name or ""
                                # Skip hidden tools — don't stream to chat
                                if tool_name not in self._HIDDEN_TOOLS:
                                    tool_content = tool_event.result.content
                                    yield {
                                        "kind": "tool_return",
                                        "tool_name": tool_name,
                                        "content": str(tool_content or "")[:2000],
                                    }

                    # If ask_user was called, break the loop so the user can
                    # respond.  All tools in this round have already executed.
                    # Commit the tool-return parts to message history so the
                    # next turn has complete context (tool-call / tool-return
                    # pairs stay matched).
                    if deps.pause_requested:
                        if tool_result_parts:
                            agent_run.ctx.state.message_history.append(
                                ModelRequest(parts=tool_result_parts)
                            )
                        break

            # Yield the final result for message access
            run_result = agent_run.result
            if run_result is not None:
                yield run_result

    @staticmethod
    def _extract_secret_keys(config: dict[str, Any]) -> set[str]:
        """Scan all tool scripts for secret("key") calls and return the key names."""
        return _extract_secret_keys_impl(config)

    @staticmethod
    async def _ensure_action_cards(
        result: BuilderResult, agent_id: str = ""
    ) -> BuilderResult:
        """Auto-generate action cards for any secret() calls the LLM missed.

        Consults integrations.yaml to determine the best auth flow:
        - oauth_redirect when the integration supports OAuth2 and the
          platform has a client_id configured
        - connect_credential otherwise (manual API key / token form)

        Skips emitting cards for providers where a credential already
        exists for this agent (prevents duplicates on config updates).
        """
        if not result.config:
            return result

        required_keys = BuilderService._extract_secret_keys(result.config)
        if not required_keys:
            return result

        # Pre-fetch which integrations have OAuth apps configured
        # and which credentials already exist for this agent.
        # On failure, fall back to connect_credential cards for all keys.
        oauth_enabled: set[str] = set()
        connected_providers: set[str] = set()
        try:
            async with async_session() as db:
                oauth_apps = await get_all_enabled_oauth_apps(db)
                oauth_enabled = {app.integration_name for app in oauth_apps}

                # Check existing credentials so we don't re-emit cards.
                # Two sources count as "already connected":
                #   1. A per-agent credential for this specific agent
                #   2. A platform-wide default connection (agent_id IS NULL)
                #      — agents inherit defaults automatically.
                if agent_id:
                    cred_filter = or_(
                        Credential.agent_id == agent_id,
                        Credential.agent_id.is_(None),
                    )
                else:
                    cred_filter = Credential.agent_id.is_(None)
                rows = await db.execute(select(Credential.provider).where(cred_filter))
                connected_providers = {r[0] for r in rows.all()}
        except Exception:
            logger.warning(
                "Failed to query OAuth apps / credentials — "
                "falling back to connect_credential cards"
            )

        # Keys already covered by existing action cards.
        # Normalise to base provider (strip dot-suffixed field references like
        # "custom_webhook.header_name" → "custom_webhook") so all three sources
        # — existing cards, required_keys, connected_providers — are compared
        # at the same provider level.
        existing_skills = {card.skill.partition(".")[0] for card in result.action_cards}

        seen_base_providers: set[str] = set()
        for key in sorted(required_keys):
            skill_name = key.partition(".")[0]
            # Skip if card already emitted OR credential already exists
            if (
                skill_name in existing_skills
                or skill_name in connected_providers
                or skill_name in seen_base_providers
            ):
                continue
            seen_base_providers.add(skill_name)
            card_type = BuilderService._pick_card_type(skill_name, oauth_enabled)
            result.action_cards.append(
                ActionCardSchema(
                    type=card_type,  # type: ignore[arg-type]  # always "oauth_redirect" | "connect_credential"
                    skill=skill_name,
                    title=f"Connect {skill_name.replace('_', ' ').title()}",
                    description=f"This agent uses secret('{skill_name}'). Please configure your {skill_name.replace('_', ' ')} credentials.",
                )
            )
            logger.info(
                "Auto-added action card for secret '{}' (type={})",
                skill_name,
                card_type,
            )

        return result

    @staticmethod
    def _pick_card_type(skill_name: str, oauth_enabled: set[str]) -> str:
        """Choose oauth_redirect or connect_credential based on integrations.yaml."""
        integration = integration_registry.load_integration_config(skill_name)
        if (
            integration
            and integration.auth.type == "oauth2"
            and skill_name in oauth_enabled
        ):
            return "oauth_redirect"
        return "connect_credential"

    # Functions in QuickJS/Rhai that mutate external state
    _WRITE_FUNCTIONS = re.compile(
        r"\b(http_post|http_post_h|http_put|http_put_h|http_delete|http_delete_h|file_write)\b"
    )

    @staticmethod
    def _ensure_side_effects(result: BuilderResult) -> BuilderResult:
        """Auto-set side_effect=true on tools whose scripts call write functions."""
        if not result.config:
            return result

        tools = result.config.get("tools", {})
        if not isinstance(tools, dict):
            return result

        for tool_id, tool_def in tools.items():
            if not isinstance(tool_def, dict):
                continue
            # Only backfill when the LLM omitted the field entirely.
            # If the LLM explicitly set side_effect (even to false), we
            # respect its judgment — the prompt has clear guidance on this.
            if "side_effect" in tool_def:
                continue
            script = str(tool_def.get("script", ""))
            has_write = bool(BuilderService._WRITE_FUNCTIONS.search(script))
            tool_def["side_effect"] = has_write
            if has_write:
                logger.info("Auto-set side_effect=true on tool '{}'", tool_id)

        return result


# Module-level singleton
builder_service = BuilderService()
