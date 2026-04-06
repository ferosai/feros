"""Edit operations DSL for continuous agent config editing.

Defines 7 typed edit operations that can be applied to an existing
v3_graph config. The LLM emits a list of these ops instead of
rewriting the entire config, and the server applies them
deterministically to produce a new complete snapshot.

Design principles:
- All-or-nothing: if any op fails, the entire edit is rejected.
- Eager validation: ops fail fast on invalid targets.
- Cascading cleanup: delete ops auto-clean dangling references.
- No rename: rename_node/rename_tool are intentionally unsupported.
"""

from __future__ import annotations

import copy
from collections.abc import Sequence
from typing import Annotated, Any, Literal

from pydantic import BaseModel, Field, model_validator

# ── Top-level field whitelist ────────────────────────────────────

_TOP_LEVEL_ALLOWED = {"language", "timezone", "voice_id"}

# ── Node field whitelist ─────────────────────────────────────────

_NODE_FIELD_ALLOWED = {"system_prompt", "greeting", "tools", "edges"}

# ── Tool field whitelist ─────────────────────────────────────────

_TOOL_FIELD_ALLOWED = {"description", "params", "script", "side_effect"}

# ── Canonical field ordering ─────────────────────────────────────

_TOP_LEVEL_ORDER = [
    "config_schema_version",
    "entry",
    "nodes",
    "tools",
    "language",
    "timezone",
    "voice_id",
]

_NODE_FIELD_ORDER = ["system_prompt", "greeting", "tools", "edges"]

_TOOL_FIELD_ORDER = ["description", "params", "script", "side_effect"]


def _validate_string_list(value: Any, field_name: str) -> None:
    """Validate that a value is a list of strings."""
    if not isinstance(value, list):
        raise ValueError(f"'{field_name}' must be a list, got {type(value).__name__}")
    for i, item in enumerate(value):
        if not isinstance(item, str):
            raise ValueError(
                f"'{field_name}[{i}]' must be a string, got {type(item).__name__}"
            )


def _validate_params_list(value: Any) -> None:
    """Validate that params is a list of objects."""
    if not isinstance(value, list):
        raise ValueError(f"'params' must be a list, got {type(value).__name__}")
    for i, item in enumerate(value):
        if not isinstance(item, dict):
            raise ValueError(
                f"'params[{i}]' must be an object, got {type(item).__name__}"
            )


def _validate_node_field_types(fields: dict[str, Any]) -> None:
    """Validate types of well-known node fields."""
    if "tools" in fields:
        _validate_string_list(fields["tools"], "tools")
    if "edges" in fields:
        _validate_string_list(fields["edges"], "edges")
    if "system_prompt" in fields and not isinstance(fields["system_prompt"], str):
        raise ValueError("'system_prompt' must be a string")
    if "greeting" in fields and not isinstance(fields["greeting"], str):
        raise ValueError("'greeting' must be a string")


# ═══════════════════════════════════════════════════════════════════
# Edit Op Models
# ═══════════════════════════════════════════════════════════════════


class SetTopLevel(BaseModel):
    """Set a top-level config field (language, timezone, voice_id)."""

    op: Literal["set_top_level"] = "set_top_level"
    key: str
    value: Any

    @model_validator(mode="after")
    def _validate_key(self) -> SetTopLevel:
        if self.key not in _TOP_LEVEL_ALLOWED:
            raise ValueError(
                f"set_top_level only allows keys: {sorted(_TOP_LEVEL_ALLOWED)}. "
                f"Got '{self.key}'."
            )
        return self


class SetEntry(BaseModel):
    """Change the entry node to a different existing node."""

    op: Literal["set_entry"] = "set_entry"
    node_id: str


class SetNodeFields(BaseModel):
    """Update fields on an existing node (partial update)."""

    op: Literal["set_node_fields"] = "set_node_fields"
    node_id: str
    fields: dict[str, Any]

    @model_validator(mode="after")
    def _validate_fields(self) -> SetNodeFields:
        bad = set(self.fields.keys()) - _NODE_FIELD_ALLOWED
        if bad:
            raise ValueError(
                f"set_node_fields only allows fields: {sorted(_NODE_FIELD_ALLOWED)}. "
                f"Got disallowed: {sorted(bad)}."
            )
        _validate_node_field_types(self.fields)
        return self


class UpsertTool(BaseModel):
    """Create a new tool or shallow-merge fields into an existing tool."""

    op: Literal["upsert_tool"] = "upsert_tool"
    tool_id: str
    fields: dict[str, Any]

    @model_validator(mode="after")
    def _validate_fields(self) -> UpsertTool:
        bad = set(self.fields.keys()) - _TOOL_FIELD_ALLOWED
        if bad:
            raise ValueError(
                f"upsert_tool only allows fields: {sorted(_TOOL_FIELD_ALLOWED)}. "
                f"Got disallowed: {sorted(bad)}."
            )
        if "params" in self.fields:
            _validate_params_list(self.fields["params"])
        if "script" in self.fields and not isinstance(self.fields["script"], str):
            raise ValueError("'script' must be a string")
        if "description" in self.fields and not isinstance(
            self.fields["description"], str
        ):
            raise ValueError("'description' must be a string")
        if "side_effect" in self.fields and not isinstance(
            self.fields["side_effect"], bool
        ):
            raise ValueError("'side_effect' must be a boolean")
        return self


class DeleteTool(BaseModel):
    """Delete a tool and remove all node references to it."""

    op: Literal["delete_tool"] = "delete_tool"
    tool_id: str


class AddNode(BaseModel):
    """Add a new node. system_prompt is required; tools/edges default to []."""

    op: Literal["add_node"] = "add_node"
    node_id: str
    fields: dict[str, Any]

    @model_validator(mode="after")
    def _validate_fields(self) -> AddNode:
        bad = set(self.fields.keys()) - _NODE_FIELD_ALLOWED
        if bad:
            raise ValueError(
                f"add_node only allows fields: {sorted(_NODE_FIELD_ALLOWED)}. "
                f"Got disallowed: {sorted(bad)}."
            )
        _validate_node_field_types(self.fields)
        if "system_prompt" not in self.fields:
            raise ValueError("add_node requires 'system_prompt' in fields.")
        if not self.fields["system_prompt"].strip():
            raise ValueError("system_prompt must be non-empty.")
        return self


class DeleteNode(BaseModel):
    """Delete a node and clean up edge references. Cannot delete the entry node."""

    op: Literal["delete_node"] = "delete_node"
    node_id: str


EditOp = Annotated[
    SetTopLevel
    | SetEntry
    | SetNodeFields
    | UpsertTool
    | DeleteTool
    | AddNode
    | DeleteNode,
    Field(discriminator="op"),
]


# ═══════════════════════════════════════════════════════════════════
# Apply Logic
# ═══════════════════════════════════════════════════════════════════


def apply_edits(
    base_config: dict[str, Any],
    edits: Sequence[EditOp],
) -> dict[str, Any]:
    """Apply a sequence of edit ops to a base config.

    Returns a new config dict. The base_config is not mutated.
    Raises ValueError if any op is invalid (all-or-nothing).
    """
    cfg = copy.deepcopy(base_config)
    nodes: dict[str, Any] = cfg.setdefault("nodes", {})
    tools: dict[str, Any] = cfg.setdefault("tools", {})

    for i, edit in enumerate(edits):
        try:
            _apply_one(cfg, nodes, tools, edit)
        except (KeyError, ValueError, TypeError) as e:
            raise ValueError(f"Edit #{i} ({edit.op}): {e}") from e

    return cfg


def _apply_one(
    cfg: dict[str, Any],
    nodes: dict[str, Any],
    tools: dict[str, Any],
    edit: EditOp,
) -> None:
    """Apply a single edit op (mutates cfg in place)."""
    match edit:
        case SetTopLevel(key=key, value=value):
            cfg[key] = value

        case SetEntry(node_id=node_id):
            if node_id not in nodes:
                raise ValueError(
                    f"Cannot set entry to '{node_id}': node does not exist. "
                    f"Available: {sorted(nodes.keys())}"
                )
            cfg["entry"] = node_id

        case SetNodeFields(node_id=node_id, fields=fields):
            if node_id not in nodes:
                raise ValueError(
                    f"Node '{node_id}' does not exist. Use add_node to create it first."
                )
            nodes[node_id].update(fields)

        case UpsertTool(tool_id=tool_id, fields=fields):
            if tool_id in tools:
                # Shallow merge: only overwrite fields that appear in the patch
                tools[tool_id].update(fields)
            else:
                # New tool — fields is the complete definition
                tools[tool_id] = fields

        case DeleteTool(tool_id=tool_id):
            if tool_id not in tools:
                raise ValueError(
                    f"Tool '{tool_id}' does not exist. "
                    f"Available: {sorted(tools.keys())}"
                )
            del tools[tool_id]
            # Cascade: remove ALL occurrences from all node.tools lists
            for node_def in nodes.values():
                if isinstance(node_def, dict):
                    node_tools = node_def.get("tools", [])
                    if isinstance(node_tools, list):
                        node_def["tools"] = [t for t in node_tools if t != tool_id]

        case AddNode(node_id=node_id, fields=fields):
            if node_id in nodes:
                raise ValueError(
                    f"Node '{node_id}' already exists. "
                    f"Use set_node_fields to modify it."
                )
            node_def = dict(fields)
            node_def.setdefault("tools", [])
            node_def.setdefault("edges", [])
            nodes[node_id] = node_def

        case DeleteNode(node_id=node_id):
            if node_id not in nodes:
                raise ValueError(
                    f"Node '{node_id}' does not exist. "
                    f"Available: {sorted(nodes.keys())}"
                )
            if cfg.get("entry") == node_id:
                raise ValueError(
                    f"Cannot delete entry node '{node_id}'. "
                    f"Call set_entry(...) first to change the entry."
                )
            del nodes[node_id]
            # Cascade: remove ALL occurrences from all other nodes' edges
            for other_def in nodes.values():
                if isinstance(other_def, dict):
                    edges = other_def.get("edges", [])
                    if isinstance(edges, list):
                        other_def["edges"] = [e for e in edges if e != node_id]

        case _:
            raise ValueError(f"Unknown edit op: {edit}")


# ═══════════════════════════════════════════════════════════════════
# Canonicalization
# ═══════════════════════════════════════════════════════════════════


def _ordered_dict(d: dict[str, Any], key_order: list[str]) -> dict[str, Any]:
    """Return a new dict with keys in the specified order, then remaining keys sorted."""
    result: dict[str, Any] = {}
    for k in key_order:
        if k in d:
            result[k] = d[k]
    for k in sorted(d.keys()):
        if k not in result:
            result[k] = d[k]
    return result


def _dedup_preserve_order(lst: list[str]) -> list[str]:
    """Remove duplicates from a list while preserving insertion order."""
    seen: set[str] = set()
    result: list[str] = []
    for item in lst:
        if item not in seen:
            seen.add(item)
            result.append(item)
    return result


def canonicalize_config(config: dict[str, Any]) -> dict[str, Any]:
    """Produce a canonical form of an agent config.

    - Top-level keys in fixed order
    - nodes/tools dicts sorted by key
    - Node/tool internal fields in fixed order
    - tools/edges lists de-duped but order preserved
    """
    result = _ordered_dict(config, _TOP_LEVEL_ORDER)

    # Canonicalize nodes
    nodes = result.get("nodes")
    if isinstance(nodes, dict):
        canon_nodes: dict[str, Any] = {}
        for nid in sorted(nodes.keys()):
            node = nodes[nid]
            if isinstance(node, dict):
                cn = _ordered_dict(node, _NODE_FIELD_ORDER)
                if isinstance(cn.get("tools"), list):
                    cn["tools"] = _dedup_preserve_order(cn["tools"])
                if isinstance(cn.get("edges"), list):
                    cn["edges"] = _dedup_preserve_order(cn["edges"])
                canon_nodes[nid] = cn
            else:
                canon_nodes[nid] = node
        result["nodes"] = canon_nodes

    # Canonicalize tools
    tools = result.get("tools")
    if isinstance(tools, dict):
        canon_tools: dict[str, Any] = {}
        for tid in sorted(tools.keys()):
            tool = tools[tid]
            if isinstance(tool, dict):
                canon_tools[tid] = _ordered_dict(tool, _TOOL_FIELD_ORDER)
            else:
                canon_tools[tid] = tool
        result["tools"] = canon_tools

    return result
