//! Agent graph — serializable multi-agent definitions with inline tools.
//!
//! The graph language is the single contract between the builder LLM
//! and the Rust runtime.  Rust doesn't know about "scenes", "slots",
//! or "booking" — it just executes nodes, edges, and tools.
//!
//! Graph JSON is stored as JSONB in `agent_versions.config_json`.
//! The builder LLM generates it; the engine loads and runs it.
//!
//! # Tool types
//!
//! - `"javascript"` — evaluates a JavaScript script in a QuickJS sandbox.
//!
//! JS scripts can call `http_get`, `http_post`, `http_put` Rust-side
//! functions registered on the engine, so they can do API orchestration
//! without an external workflow tool.

use std::collections::HashMap;

use serde_json::json;

// ── Graph Definition ────────────────────────────────────────────

/// A complete agent graph.  One JSON blob = one complete agent config.
///
/// ```json
/// {
///   "entry": "receptionist",
///   "nodes": {
///     "receptionist": {
///       "system_prompt": "You are a hotel receptionist...",
///       "tools": ["check_availability"],
///       "edges": ["booking_agent", "concierge"]
///     }
///   },
///   "tools": {
///     "check_availability": {
///       "type": "http",
///       "description": "Check room availability",
///       "method": "GET",
///       "endpoint": "https://api.hotel.com/availability",
///       "parameters": [...]
///     }
///   }
/// }
/// ```
// Re-export canonical recording type definitions from common.
pub use common::{AudioFormat, AudioLayout, RecordingConfig};

pub use proto::agent::{AgentGraphDef, NodeDef, ParamDef, ToolDef, ToolResultMode};

// ── Runtime State ───────────────────────────────────────────────

/// Runtime state tracking the active node in the graph.
#[derive(Debug, Clone)]
pub struct SwarmState {
    /// The currently active node ID.
    pub active_node: String,
    /// The parsed graph definition.
    pub graph: AgentGraphDef,
}

impl SwarmState {
    /// Create a new swarm state, starting at the graph's entry node.
    pub fn new(graph: AgentGraphDef) -> Self {
        let entry = graph.entry.clone();
        Self {
            active_node: entry,
            graph,
        }
    }

    /// Get the currently active node definition.
    pub fn active_def(&self) -> Option<&NodeDef> {
        self.graph.nodes.get(&self.active_node)
    }

    /// Transfer to another node.  Returns true if the transition
    /// is valid (target exists AND is a declared edge from the active node).
    pub fn transfer_to(&mut self, target: &str) -> bool {
        // Target must exist in graph
        if !self.graph.nodes.contains_key(target) {
            return false;
        }
        // Current node must list target in its edges
        if let Some(current) = self.active_def() {
            if !current.edges.contains(&target.to_string()) {
                return false;
            }
        } else {
            return false;
        }
        self.active_node = target.to_string();
        true
    }
}

// ── Transfer Tool Schema Generation ─────────────────────────────

/// Name of the synthetic transfer tool.
pub const TRANSFER_TOOL_NAME: &str = "transfer_to";

/// Name of the synthetic hang-up tool.
pub const HANG_UP_TOOL_NAME: &str = "hang_up";

/// Name of the synthetic on-hold tool.
pub const ON_HOLD_TOOL_NAME: &str = "on_hold";

// Re-export artifact tool names so callers don't need to depend on artifact_store directly.
pub use crate::artifact_store::{LIST_ARTIFACTS_TOOL, READ_ARTIFACT_TOOL, SAVE_ARTIFACT_TOOL};

/// Generate the three artifact tool schemas (save / read / list).
pub fn make_artifact_tool_schemas() -> Vec<serde_json::Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": SAVE_ARTIFACT_TOOL,
                "description": "Save a named artifact (your persistent note) for later reference. \
                    Use for: caller name, phone number, email, account ID, confirmed booking \
                    details, or any information that must survive context compression. \
                    Use descriptive names like 'caller_info.md' or 'booking_details.md'. \
                    Overwriting an artifact with the same name replaces its content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Artifact name, e.g. 'caller_info.md'"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to persist"
                        }
                    },
                    "required": ["name", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": READ_ARTIFACT_TOOL,
                "description": "Read a previously saved artifact by name. \
                    Call list_artifacts first if you are unsure what is available.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Name of the artifact to read"
                        }
                    },
                    "required": ["name"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": LIST_ARTIFACTS_TOOL,
                "description": "List all saved artifacts with their names and sizes. \
                    Call this when context seems incomplete to discover what you have saved.",
                "parameters": {
                    "type": "object",
                    "properties": {}
                }
            }
        }),
    ]
}

/// Generate the `hang_up` tool schema.
pub fn make_hang_up_tool_schema() -> serde_json::Value {
    json!({
        "type": "function",
        "function": {
            "name": HANG_UP_TOOL_NAME,
            "description": r#"Ends the current phone call and disconnects immediately.

Call when:
- The user clearly indicates they are done (e.g. "that's all, bye", "thank you, goodbye").
- The task is fully completed and there is nothing left to discuss.
- The agent has delivered its farewell.

Do not call when:
- The user asks to hold, transfer, or pause.
- The intent to end is unclear.

This is the final action. Once called, no further interaction is possible."#,
            "parameters": {
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Brief reason for ending the call (e.g. 'user_goodbye', 'task_complete')"
                    }
                },
                "required": ["reason"]
            }
        }
    })
}

/// Generate the `on_hold` tool schema.
///
/// Called when the user asks to hold, pause, or indicates they'll be back.
/// Signals the reactor to suppress idle-timeout nudges/shutdown temporarily.
pub fn make_on_hold_tool_schema() -> serde_json::Value {
    json!({
        "type": "function",
        "function": {
            "name": ON_HOLD_TOOL_NAME,
            "description": r#"Signal that the user is placing the call on hold or will be away briefly.
Call this when the user says things like "hold on", "one second", "brb", "let me check",
or otherwise indicates they need a moment before continuing.
This prevents the call from being ended due to silence while the user is away.
Do NOT call this if the user is ending the call — use hang_up instead."#,
            "parameters": {
                "type": "object",
                "properties": {
                    "duration_mins": {
                        "type": "integer",
                        "description": "Approximate hold duration in minutes (e.g. 5, 2, 10). Estimate based on context, default to 3 if unknown."
                    }
                },
                "required": ["duration_mins"]
            }
        }
    })
}

pub fn make_transfer_tool_schema(edges: &[String]) -> serde_json::Value {
    if edges.is_empty() {
        return json!(null);
    }
    json!({
        "type": "function",
        "function": {
            "name": TRANSFER_TOOL_NAME,
            "description": format!(
                "Transfer the conversation to another agent. Available targets: {}",
                edges.join(", ")
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to transfer to",
                        "enum": edges
                    }
                },
                "required": ["agent_id"]
            }
        }
    })
}

/// Build the complete tool schemas for a node: its own tools + transfer_to.
///
/// Tool schemas are generated from the node's `tools` list, resolved
/// against the graph's `tools` map. Unknown tool names are logged as warnings
/// and skipped.
pub fn build_node_tool_schemas(
    node: &NodeDef,
    graph_tools: &HashMap<String, ToolDef>,
) -> Vec<serde_json::Value> {
    let mut schemas: Vec<serde_json::Value> = node
        .tools
        .iter()
        .filter_map(|name| match graph_tools.get(name) {
            Some(def) => Some(tool_def_to_schema(name, def)),
            None => {
                tracing::warn!(
                    "[swarm] Tool '{}' referenced in node but not defined in graph tools map",
                    name
                );
                None
            }
        })
        .collect();

    // Add transfer_to tool if this node has outgoing edges
    if !node.edges.is_empty() {
        schemas.push(make_transfer_tool_schema(&node.edges));
    }

    // Always add hang_up tool so the agent can end the call
    schemas.push(make_hang_up_tool_schema());

    // Always add on_hold tool so the agent can pause idle detection
    schemas.push(make_on_hold_tool_schema());

    // Always add artifact tools so the agent can persist important information
    schemas.extend(make_artifact_tool_schemas());

    schemas
}

/// Convert a `ToolDef` to an OpenAI-compatible function tool schema.
fn tool_def_to_schema(name: &str, def: &ToolDef) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in &def.params {
        let mut prop = serde_json::Map::new();
        prop.insert("type".to_string(), json!(param.r#type));
        if !param.description.is_empty() {
            prop.insert("description".to_string(), json!(param.description));
        }
        if !param.options.is_empty() {
            prop.insert("enum".to_string(), json!(param.options));
        }
        properties.insert(param.name.clone(), serde_json::Value::Object(prop));
        if param.required {
            required.push(json!(param.name));
        }
    }

    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": def.description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        }
    })
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> AgentGraphDef {
        serde_json::from_str(
            r#"{
            "entry": "receptionist",
            "nodes": {
                "receptionist": {
                    "system_prompt": "You are a receptionist.",
                    "tools": ["check_availability"],
                    "edges": ["booking", "concierge"]
                },
                "booking": {
                    "model": "gpt-4o",
                    "system_prompt": "You are a booking agent.",
                    "tools": ["create_reservation"],
                    "edges": ["receptionist"]
                },
                "concierge": {
                    "system_prompt": "You are a concierge.",
                    "tools": [],
                    "edges": ["receptionist"]
                }
            },
            "tools": {
                "check_availability": {
                    "description": "Check room availability",
                    "params": [
                        {"name": "check_in", "type": "string", "required": true},
                        {"name": "check_out", "type": "string", "required": true}
                    ],
                    "script": "const resp = http_get(`https://api.hotel.com/availability?check_in=${check_in}&check_out=${check_out}`); return resp;"
                },
                "create_reservation": {
                    "description": "Create a reservation",
                    "params": [
                        {"name": "guest_name", "type": "string", "required": true},
                        {"name": "check_in", "type": "string", "required": true}
                    ],
                    "script": "const result = `Booked for ${guest_name} on ${check_in}`; return result;"
                }
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn parse_graph_from_json() {
        let graph = sample_graph();
        assert_eq!(graph.entry, "receptionist");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(
            graph.nodes["receptionist"].edges,
            vec!["booking", "concierge"]
        );
        assert_eq!(graph.nodes["booking"].model, Some("gpt-4o".to_string()));
        assert!(graph.nodes["receptionist"].model.is_none());
        assert_eq!(graph.tools.len(), 2);
    }

    #[test]
    fn swarm_state_transfer() {
        let graph = sample_graph();
        let mut state = SwarmState::new(graph);

        assert_eq!(state.active_node, "receptionist");

        // Valid transfer
        assert!(state.transfer_to("booking"));
        assert_eq!(state.active_node, "booking");

        // Valid transfer back
        assert!(state.transfer_to("receptionist"));
        assert_eq!(state.active_node, "receptionist");

        // Invalid: non-existent node
        assert!(!state.transfer_to("nonexistent"));
        assert_eq!(state.active_node, "receptionist");
    }

    #[test]
    fn transfer_respects_edges() {
        let graph = sample_graph();
        let mut state = SwarmState::new(graph);

        state.transfer_to("booking");

        // Booking can only go to receptionist, not concierge
        assert!(!state.transfer_to("concierge"));
        assert_eq!(state.active_node, "booking");

        assert!(state.transfer_to("receptionist"));
        assert_eq!(state.active_node, "receptionist");
    }

    #[test]
    fn tool_schema_from_http_def() {
        let graph = sample_graph();
        let schema = tool_def_to_schema("check_availability", &graph.tools["check_availability"]);

        assert_eq!(schema["type"], "function");
        assert_eq!(schema["function"]["name"], "check_availability");
        let params = &schema["function"]["parameters"];
        assert!(params["properties"]["check_in"]["type"] == "string");
        assert!(params["required"]
            .as_array()
            .unwrap()
            .contains(&json!("check_in")));
    }

    #[test]
    fn tool_schema_from_js_def() {
        let graph = sample_graph();
        let schema = tool_def_to_schema("create_reservation", &graph.tools["create_reservation"]);

        assert_eq!(schema["function"]["name"], "create_reservation");
        assert!(schema["function"]["parameters"]["properties"]["guest_name"]["type"] == "string");
    }

    #[test]
    fn node_tool_schemas_include_transfer() {
        let graph = sample_graph();
        let schemas = build_node_tool_schemas(&graph.nodes["receptionist"], &graph.tools);

        // Base tools + transfer_to + hang_up + on_hold + artifacts
        assert!(
            schemas.len() >= 3,
            "Should have at least base tool + transfer + hang_up"
        );

        let names: Vec<&str> = schemas
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"check_availability"));
        assert!(names.contains(&"transfer_to"));
        assert!(names.contains(&"hang_up"));
    }

    #[test]
    fn node_without_edges_has_no_transfer() {
        let graph: AgentGraphDef = serde_json::from_str(
            r#"{
            "entry": "solo",
            "nodes": {
                "solo": {
                    "system_prompt": "You are a solo agent.",
                    "tools": [],
                    "edges": []
                }
            }
        }"#,
        )
        .unwrap();
        let schemas = build_node_tool_schemas(&graph.nodes["solo"], &graph.tools);
        // hang_up + on_hold + artifacts (no transfer_to)
        assert!(schemas.len() >= 1, "Should have at least hang_up");
        assert_eq!(schemas[0]["function"]["name"].as_str().unwrap(), "hang_up");
    }

    #[test]
    fn transfer_tool_schema_enum() {
        let schema = make_transfer_tool_schema(&["booking".to_string(), "concierge".to_string()]);

        let func = &schema["function"];
        assert_eq!(func["name"], TRANSFER_TOOL_NAME);
        let agent_enum = &func["parameters"]["properties"]["agent_id"]["enum"];
        assert_eq!(agent_enum[0], "booking");
        assert_eq!(agent_enum[1], "concierge");
    }

    #[test]
    fn transfer_tool_schema_empty_edges() {
        let schema = make_transfer_tool_schema(&[]);
        assert!(schema.is_null());
    }
}
