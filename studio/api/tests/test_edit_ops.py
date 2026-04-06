"""Tests for the edit ops DSL (apply_edits + canonicalize_config)."""

import copy

import pytest

from app.agent_builder.edit_ops import (
    AddNode,
    DeleteNode,
    DeleteTool,
    SetEntry,
    SetNodeFields,
    SetTopLevel,
    UpsertTool,
    apply_edits,
    canonicalize_config,
)

# ── Fixtures ─────────────────────────────────────────────────────


def _base_config() -> dict:
    """A minimal but complete v3_graph config for testing."""
    return {
        "entry": "main",
        "nodes": {
            "main": {
                "system_prompt": "You are a booking agent.",
                "greeting": "Hello!",
                "tools": ["check_rooms", "make_booking"],
                "edges": [],
            },
        },
        "tools": {
            "check_rooms": {
                "description": "Check room availability",
                "params": [{"name": "date", "type": "string", "required": True}],
                "script": 'let resp = http_get("https://api.example.com/rooms"); return resp.body;',
                "side_effect": False,
            },
            "make_booking": {
                "description": "Make a booking",
                "params": [
                    {"name": "guest", "type": "string", "required": True},
                    {"name": "date", "type": "string", "required": True},
                ],
                "script": 'let resp = http_post("https://api.example.com/book", {guest, date}); return resp.body;',
                "side_effect": True,
            },
        },
        "language": "en",
        "timezone": "",
        "voice_id": "alloy",
    }


def _dispatcher_config() -> dict:
    """A multi-node dispatcher star config."""
    return {
        "entry": "dispatcher",
        "nodes": {
            "dispatcher": {
                "system_prompt": "Route to the right specialist.",
                "tools": [],
                "edges": ["billing", "support"],
            },
            "billing": {
                "system_prompt": "Handle billing questions.",
                "tools": ["lookup_invoice"],
                "edges": ["dispatcher"],
            },
            "support": {
                "system_prompt": "Handle tech support.",
                "tools": [],
                "edges": ["dispatcher"],
            },
        },
        "tools": {
            "lookup_invoice": {
                "description": "Look up invoice",
                "params": [{"name": "invoice_id", "type": "string", "required": True}],
                "script": "return http_get(`/invoices/${invoice_id}`).body;",
                "side_effect": False,
            },
        },
        "language": "en",
    }


# ═══════════════════════════════════════════════════════════════════
# SetTopLevel
# ═══════════════════════════════════════════════════════════════════


class TestSetTopLevel:
    def test_set_language(self):
        result = apply_edits(_base_config(), [SetTopLevel(key="language", value="zh")])
        assert result["language"] == "zh"

    def test_set_timezone(self):
        result = apply_edits(
            _base_config(), [SetTopLevel(key="timezone", value="Asia/Shanghai")]
        )
        assert result["timezone"] == "Asia/Shanghai"

    def test_set_voice_id(self):
        result = apply_edits(
            _base_config(), [SetTopLevel(key="voice_id", value="nova")]
        )
        assert result["voice_id"] == "nova"

    def test_rejects_nodes_key(self):
        with pytest.raises(ValueError, match="set_top_level only allows"):
            SetTopLevel(key="nodes", value={})

    def test_rejects_tools_key(self):
        with pytest.raises(ValueError, match="set_top_level only allows"):
            SetTopLevel(key="tools", value={})

    def test_rejects_entry_key(self):
        with pytest.raises(ValueError, match="set_top_level only allows"):
            SetTopLevel(key="entry", value="x")


# ═══════════════════════════════════════════════════════════════════
# SetEntry
# ═══════════════════════════════════════════════════════════════════


class TestSetEntry:
    def test_set_entry(self):
        cfg = _dispatcher_config()
        result = apply_edits(cfg, [SetEntry(node_id="billing")])
        assert result["entry"] == "billing"

    def test_rejects_nonexistent_node(self):
        with pytest.raises(ValueError, match="does not exist"):
            apply_edits(_base_config(), [SetEntry(node_id="nonexistent")])


# ═══════════════════════════════════════════════════════════════════
# SetNodeFields
# ═══════════════════════════════════════════════════════════════════


class TestSetNodeFields:
    def test_update_greeting(self):
        result = apply_edits(
            _base_config(),
            [SetNodeFields(node_id="main", fields={"greeting": "您好！"})],
        )
        assert result["nodes"]["main"]["greeting"] == "您好！"
        # Other fields unchanged
        assert result["nodes"]["main"]["system_prompt"] == "You are a booking agent."
        assert result["nodes"]["main"]["tools"] == ["check_rooms", "make_booking"]

    def test_update_system_prompt(self):
        result = apply_edits(
            _base_config(),
            [
                SetNodeFields(
                    node_id="main",
                    fields={"system_prompt": "You are a friendly booking agent."},
                )
            ],
        )
        assert (
            result["nodes"]["main"]["system_prompt"]
            == "You are a friendly booking agent."
        )
        assert result["nodes"]["main"]["greeting"] == "Hello!"

    def test_replace_tools_list(self):
        result = apply_edits(
            _base_config(),
            [SetNodeFields(node_id="main", fields={"tools": ["check_rooms"]})],
        )
        assert result["nodes"]["main"]["tools"] == ["check_rooms"]

    def test_replace_edges_list(self):
        cfg = _dispatcher_config()
        result = apply_edits(
            cfg,
            [SetNodeFields(node_id="dispatcher", fields={"edges": ["billing"]})],
        )
        assert result["nodes"]["dispatcher"]["edges"] == ["billing"]

    def test_rejects_nonexistent_node(self):
        with pytest.raises(ValueError, match="does not exist"):
            apply_edits(
                _base_config(),
                [SetNodeFields(node_id="ghost", fields={"greeting": "hi"})],
            )

    def test_rejects_disallowed_field(self):
        with pytest.raises(ValueError, match="set_node_fields only allows"):
            SetNodeFields(node_id="main", fields={"description": "bad"})

    def test_rejects_tools_as_string(self):
        with pytest.raises(ValueError, match="'tools' must be a list"):
            SetNodeFields(node_id="main", fields={"tools": "check_rooms"})

    def test_rejects_edges_as_string(self):
        with pytest.raises(ValueError, match="'edges' must be a list"):
            SetNodeFields(node_id="main", fields={"edges": "main"})

    def test_rejects_tools_with_non_string_items(self):
        with pytest.raises(ValueError, match="must be a string"):
            SetNodeFields(node_id="main", fields={"tools": [123]})


# ═══════════════════════════════════════════════════════════════════
# UpsertTool
# ═══════════════════════════════════════════════════════════════════


class TestUpsertTool:
    def test_create_new_tool(self):
        result = apply_edits(
            _base_config(),
            [
                UpsertTool(
                    tool_id="get_weather",
                    fields={
                        "description": "Check weather",
                        "params": [
                            {"name": "city", "type": "string", "required": True}
                        ],
                        "script": "return http_get(`/weather/${city}`).body;",
                        "side_effect": False,
                    },
                )
            ],
        )
        assert "get_weather" in result["tools"]
        assert result["tools"]["get_weather"]["description"] == "Check weather"

    def test_shallow_merge_existing_tool(self):
        result = apply_edits(
            _base_config(),
            [
                UpsertTool(
                    tool_id="check_rooms",
                    fields={"script": "return 'new script';"},
                )
            ],
        )
        tool = result["tools"]["check_rooms"]
        assert tool["script"] == "return 'new script';"
        # Other fields preserved
        assert tool["description"] == "Check room availability"
        assert tool["side_effect"] is False
        assert len(tool["params"]) == 1

    def test_shallow_merge_replaces_params_list(self):
        new_params = [
            {"name": "city", "type": "string", "required": True},
            {"name": "date", "type": "string", "required": True},
        ]
        result = apply_edits(
            _base_config(),
            [UpsertTool(tool_id="check_rooms", fields={"params": new_params})],
        )
        assert result["tools"]["check_rooms"]["params"] == new_params

    def test_rejects_disallowed_field(self):
        with pytest.raises(ValueError, match="upsert_tool only allows"):
            UpsertTool(tool_id="x", fields={"descripton": "typo"})

    def test_rejects_params_as_string(self):
        with pytest.raises(ValueError, match="'params' must be a list"):
            UpsertTool(tool_id="x", fields={"params": "oops"})

    def test_rejects_params_with_non_dict_items(self):
        with pytest.raises(ValueError, match="must be an object"):
            UpsertTool(tool_id="x", fields={"params": ["not_a_dict"]})

    def test_rejects_script_as_non_string(self):
        with pytest.raises(ValueError, match="'script' must be a string"):
            UpsertTool(tool_id="x", fields={"script": 123})

    def test_rejects_side_effect_as_non_bool(self):
        with pytest.raises(ValueError, match="'side_effect' must be a boolean"):
            UpsertTool(tool_id="x", fields={"side_effect": "yes"})


# ═══════════════════════════════════════════════════════════════════
# DeleteTool
# ═══════════════════════════════════════════════════════════════════


class TestDeleteTool:
    def test_delete_and_cascade(self):
        result = apply_edits(_base_config(), [DeleteTool(tool_id="make_booking")])
        assert "make_booking" not in result["tools"]
        # Cascaded: removed from node.tools
        assert "make_booking" not in result["nodes"]["main"]["tools"]
        assert "check_rooms" in result["nodes"]["main"]["tools"]

    def test_cascade_multiple_nodes(self):
        cfg = _dispatcher_config()
        result = apply_edits(cfg, [DeleteTool(tool_id="lookup_invoice")])
        assert "lookup_invoice" not in result["tools"]
        assert "lookup_invoice" not in result["nodes"]["billing"]["tools"]

    def test_cascade_removes_all_duplicates(self):
        """list.remove only removes the first occurrence — we need all gone."""
        cfg = _base_config()
        cfg["nodes"]["main"]["tools"] = ["check_rooms", "make_booking", "make_booking"]
        result = apply_edits(cfg, [DeleteTool(tool_id="make_booking")])
        assert "make_booking" not in result["nodes"]["main"]["tools"]

    def test_rejects_nonexistent_tool(self):
        with pytest.raises(ValueError, match="does not exist"):
            apply_edits(_base_config(), [DeleteTool(tool_id="ghost")])


# ═══════════════════════════════════════════════════════════════════
# AddNode
# ═══════════════════════════════════════════════════════════════════


class TestAddNode:
    def test_add_node_with_defaults(self):
        result = apply_edits(
            _base_config(),
            [
                AddNode(
                    node_id="faq",
                    fields={"system_prompt": "Answer FAQs."},
                )
            ],
        )
        node = result["nodes"]["faq"]
        assert node["system_prompt"] == "Answer FAQs."
        assert node["tools"] == []
        assert node["edges"] == []

    def test_add_node_with_full_fields(self):
        result = apply_edits(
            _base_config(),
            [
                AddNode(
                    node_id="billing",
                    fields={
                        "system_prompt": "Handle billing.",
                        "greeting": "Billing department.",
                        "tools": ["check_rooms"],
                        "edges": ["main"],
                    },
                )
            ],
        )
        node = result["nodes"]["billing"]
        assert node["greeting"] == "Billing department."
        assert node["tools"] == ["check_rooms"]
        assert node["edges"] == ["main"]

    def test_rejects_existing_node(self):
        with pytest.raises(ValueError, match="already exists"):
            apply_edits(
                _base_config(),
                [AddNode(node_id="main", fields={"system_prompt": "dup"})],
            )

    def test_rejects_missing_system_prompt(self):
        with pytest.raises(ValueError, match="requires 'system_prompt'"):
            AddNode(node_id="bad", fields={"greeting": "hi"})

    def test_rejects_empty_system_prompt(self):
        with pytest.raises(ValueError, match="non-empty"):
            AddNode(node_id="bad", fields={"system_prompt": "  "})

    def test_rejects_disallowed_field(self):
        with pytest.raises(ValueError, match="add_node only allows"):
            AddNode(node_id="x", fields={"system_prompt": "ok", "descripton": "typo"})


# ═══════════════════════════════════════════════════════════════════
# DeleteNode
# ═══════════════════════════════════════════════════════════════════


class TestDeleteNode:
    def test_delete_and_cascade_edges(self):
        cfg = _dispatcher_config()
        result = apply_edits(cfg, [DeleteNode(node_id="support")])
        assert "support" not in result["nodes"]
        # Cascaded: removed from dispatcher's edges
        assert "support" not in result["nodes"]["dispatcher"]["edges"]
        assert "billing" in result["nodes"]["dispatcher"]["edges"]

    def test_cascade_removes_all_duplicate_edges(self):
        cfg = _dispatcher_config()
        cfg["nodes"]["dispatcher"]["edges"] = ["billing", "support", "support"]
        result = apply_edits(cfg, [DeleteNode(node_id="support")])
        assert "support" not in result["nodes"]["dispatcher"]["edges"]

    def test_rejects_deleting_entry_node(self):
        with pytest.raises(ValueError, match="Cannot delete entry node"):
            apply_edits(_dispatcher_config(), [DeleteNode(node_id="dispatcher")])

    def test_delete_entry_after_set_entry(self):
        """Can delete the old entry if a new entry is set first."""
        cfg = _dispatcher_config()
        result = apply_edits(
            cfg,
            [
                SetEntry(node_id="billing"),
                DeleteNode(node_id="dispatcher"),
            ],
        )
        assert result["entry"] == "billing"
        assert "dispatcher" not in result["nodes"]

    def test_rejects_nonexistent_node(self):
        with pytest.raises(ValueError, match="does not exist"):
            apply_edits(_base_config(), [DeleteNode(node_id="ghost")])


# ═══════════════════════════════════════════════════════════════════
# Multi-op sequences
# ═══════════════════════════════════════════════════════════════════


class TestMultiOp:
    def test_add_tool_and_attach_to_node(self):
        result = apply_edits(
            _base_config(),
            [
                UpsertTool(
                    tool_id="get_weather",
                    fields={
                        "description": "Check weather",
                        "script": "return 'sunny';",
                    },
                ),
                SetNodeFields(
                    node_id="main",
                    fields={"tools": ["check_rooms", "make_booking", "get_weather"]},
                ),
            ],
        )
        assert "get_weather" in result["tools"]
        assert "get_weather" in result["nodes"]["main"]["tools"]

    def test_delete_tool_and_update_prompt(self):
        result = apply_edits(
            _base_config(),
            [
                DeleteTool(tool_id="make_booking"),
                SetNodeFields(
                    node_id="main",
                    fields={"system_prompt": "You are an info agent. No bookings."},
                ),
            ],
        )
        assert "make_booking" not in result["tools"]
        assert "No bookings" in result["nodes"]["main"]["system_prompt"]

    def test_base_config_not_mutated(self):
        base = _base_config()
        original = copy.deepcopy(base)
        apply_edits(base, [SetTopLevel(key="language", value="zh")])
        assert base == original

    def test_all_or_nothing(self):
        """If one op fails, the base config is unchanged."""
        base = _base_config()
        original = copy.deepcopy(base)
        with pytest.raises(ValueError):
            apply_edits(
                base,
                [
                    SetTopLevel(key="language", value="zh"),  # valid
                    SetNodeFields(
                        node_id="ghost", fields={"greeting": "hi"}
                    ),  # invalid
                ],
            )
        # Base not mutated
        assert base == original


# ═══════════════════════════════════════════════════════════════════
# Canonicalization
# ═══════════════════════════════════════════════════════════════════


class TestCanonicalize:
    def test_top_level_key_order(self):
        cfg = {
            "voice_id": "alloy",
            "tools": {},
            "entry": "main",
            "language": "en",
            "nodes": {"main": {"system_prompt": "hi", "tools": [], "edges": []}},
        }
        result = canonicalize_config(cfg)
        keys = list(result.keys())
        assert keys.index("entry") < keys.index("nodes")
        assert keys.index("nodes") < keys.index("tools")
        assert keys.index("tools") < keys.index("language")

    def test_nodes_sorted_by_key(self):
        cfg = _dispatcher_config()
        result = canonicalize_config(cfg)
        assert list(result["nodes"].keys()) == ["billing", "dispatcher", "support"]

    def test_tools_sorted_by_key(self):
        cfg = _base_config()
        result = canonicalize_config(cfg)
        assert list(result["tools"].keys()) == ["check_rooms", "make_booking"]

    def test_node_field_order(self):
        cfg = _base_config()
        result = canonicalize_config(cfg)
        node_keys = list(result["nodes"]["main"].keys())
        assert node_keys.index("system_prompt") < node_keys.index("greeting")
        assert node_keys.index("greeting") < node_keys.index("tools")
        assert node_keys.index("tools") < node_keys.index("edges")

    def test_tool_field_order(self):
        cfg = _base_config()
        result = canonicalize_config(cfg)
        tool_keys = list(result["tools"]["check_rooms"].keys())
        assert tool_keys.index("description") < tool_keys.index("params")
        assert tool_keys.index("params") < tool_keys.index("script")
        assert tool_keys.index("script") < tool_keys.index("side_effect")

    def test_dedup_preserves_order(self):
        cfg = _base_config()
        cfg["nodes"]["main"]["tools"] = ["make_booking", "check_rooms", "make_booking"]
        result = canonicalize_config(cfg)
        assert result["nodes"]["main"]["tools"] == ["make_booking", "check_rooms"]

    def test_idempotent(self):
        cfg = _base_config()
        once = canonicalize_config(cfg)
        twice = canonicalize_config(once)
        assert once == twice
