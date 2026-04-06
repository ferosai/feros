"""Tests for the agent graph schema validation."""

from app.agent_builder.graph import (
    _pin_entry_to_top,
    _strip_entry_anchor_block,
    generate_graph_mermaid,
    validate_graph,
)


def _make_minimal_graph(**overrides: object) -> dict:
    """Return a minimal but valid agent graph."""
    graph: dict = {
        "entry": "main",
        "nodes": {
            "main": {
                "system_prompt": "You are a helpful assistant.",
                "tools": [],
                "edges": [],
            }
        },
        "tools": {},
    }
    graph.update(overrides)
    return graph


class TestValidateGraph:
    """Tests for validate_graph()."""

    def test_valid_minimal_graph(self) -> None:
        errors = validate_graph(_make_minimal_graph())
        assert errors == []

    def test_missing_entry(self) -> None:
        graph = _make_minimal_graph()
        del graph["entry"]
        errors = validate_graph(graph)
        assert any("entry" in e.lower() for e in errors)

    def test_entry_references_invalid_node(self) -> None:
        graph = _make_minimal_graph(entry="nonexistent")
        errors = validate_graph(graph)
        assert any("nonexistent" in e for e in errors)

    def test_empty_nodes(self) -> None:
        graph = _make_minimal_graph(nodes={})
        errors = validate_graph(graph)
        assert any("non-empty" in e for e in errors)

    def test_missing_system_prompt(self) -> None:
        graph = _make_minimal_graph()
        graph["nodes"]["main"]["system_prompt"] = ""
        errors = validate_graph(graph)
        assert any("system_prompt" in e for e in errors)

    def test_invalid_edge_reference(self) -> None:
        graph = _make_minimal_graph()
        graph["nodes"]["main"]["edges"] = ["nonexistent"]
        errors = validate_graph(graph)
        assert any("nonexistent" in e for e in errors)

    def test_invalid_tool_reference(self) -> None:
        graph = _make_minimal_graph()
        graph["nodes"]["main"]["tools"] = ["missing_tool"]
        errors = validate_graph(graph)
        assert any("missing_tool" in e for e in errors)

    def test_tool_missing_description(self) -> None:
        graph = _make_minimal_graph()
        graph["tools"]["my_tool"] = {"script": "42"}
        errors = validate_graph(graph)
        assert any("description" in e for e in errors)

    def test_tool_missing_script(self) -> None:
        graph = _make_minimal_graph()
        graph["tools"]["my_tool"] = {"description": "A tool"}
        errors = validate_graph(graph)
        assert any("script" in e for e in errors)

    def test_tool_param_missing_name(self) -> None:
        graph = _make_minimal_graph()
        graph["tools"]["my_tool"] = {
            "description": "A tool",
            "script": "42",
            "params": [{"type": "string"}],
        }
        errors = validate_graph(graph)
        assert any("name" in e for e in errors)

    def test_unreachable_node(self) -> None:
        graph = _make_minimal_graph()
        graph["nodes"]["orphan"] = {
            "system_prompt": "I am unreachable",
            "tools": [],
            "edges": [],
        }
        errors = validate_graph(graph)
        assert any("unreachable" in e.lower() for e in errors)

    def test_multi_node_valid_graph(self) -> None:
        graph = {
            "entry": "receptionist",
            "nodes": {
                "receptionist": {
                    "system_prompt": "You are the receptionist.",
                    "tools": ["lookup"],
                    "edges": ["specialist"],
                },
                "specialist": {
                    "system_prompt": "You are the specialist.",
                    "tools": [],
                    "edges": ["receptionist"],
                },
            },
            "tools": {
                "lookup": {
                    "description": "Look up info",
                    "script": 'http_get("https://api.example.com")',
                    "params": [{"name": "query", "type": "string", "required": True}],
                }
            },
        }
        errors = validate_graph(graph)
        assert errors == []


class TestGenerateMermaid:
    """Tests for generate_graph_mermaid()."""

    def test_mermaid_has_entry_node(self) -> None:
        graph = _make_minimal_graph()
        mermaid = generate_graph_mermaid(graph)
        assert "main" in mermaid

    def test_mermaid_has_edges(self) -> None:
        graph = {
            "entry": "a",
            "nodes": {
                "a": {"system_prompt": "A", "tools": [], "edges": ["b"]},
                "b": {"system_prompt": "B", "tools": [], "edges": []},
            },
            "tools": {},
        }
        mermaid = generate_graph_mermaid(graph)
        assert "transfer" in mermaid

    def test_mermaid_pins_entry_node_to_top(self) -> None:
        graph = _make_minimal_graph()
        mermaid = generate_graph_mermaid(graph)
        assert "__entry_anchor --- main" in mermaid
        assert "linkStyle 0 stroke:none,stroke-width:0px" in mermaid


class TestPinEntryToTop:
    """Tests for Mermaid layout pinning."""

    def test_pins_first_step_inside_entry_subgraph(self) -> None:
        mermaid = """graph TD
  subgraph appointment_scheduler["Appointment Scheduler"]
    as1[Greeting]
    as2[Information Gathering]
    as1 --> as2
  end
  check_availability["check_availability"]
  as2 -.-> check_availability"""
        pinned = _pin_entry_to_top(mermaid, "appointment_scheduler")
        # Anchor targets first step, not the subgraph itself
        assert "__entry_anchor --- as1" in pinned
        assert "__entry_anchor --- appointment_scheduler" not in pinned
        # Anchor is OUTSIDE the subgraph (after "end")
        end_pos = pinned.index("\n  end\n") + len("\n  end\n")
        anchor_pos = pinned.index("__entry_anchor(( ))")
        assert anchor_pos >= end_pos
        # Target label is defined before anchor references it
        assert pinned.index("as1[Greeting]") < pinned.index("__entry_anchor --- as1")

    def test_anchor_is_inserted_after_subgraph_end(self) -> None:
        mermaid = """graph TD
  subgraph scheduler["Scheduler"]
    s1[Greet & ask details] --> s2[Check availability]
    s2 --> s3{Slot available?}
  end"""
        pinned = _pin_entry_to_top(mermaid, "scheduler")
        assert pinned.index(
            "s1[Greet & ask details] --> s2[Check availability]"
        ) < pinned.index("__entry_anchor --- s1")
        # 2 edges inside subgraph (-->, -->), so anchor's hidden edge is index 2
        assert "linkStyle 2 stroke:none,stroke-width:0px" in pinned

    def test_regression_real_v1_mermaid(self) -> None:
        """Regression: agent 4f0c6ea1 v1 mermaid — anchor must be outside subgraph."""
        mermaid = """graph TD
  subgraph reservation_handler["Reservation Handler"]
    rh1[Greet and ask details] --> rh2[Check availability]
    rh2 -- Available --> rh3[Make reservation]
    rh3 --> rh4[Confirm and end]
    rh2 -- Unavailable --> rh5[Suggest alternatives]
    rh5 --> rh1
  end
  check_availability["check_availability"]
  make_reservation["make_reservation"]
  reservation_handler -.-> check_availability
  reservation_handler -.-> make_reservation"""
        pinned = _pin_entry_to_top(mermaid, "reservation_handler")
        # Target label defined before anchor reference
        assert pinned.index("rh1[Greet and ask details]") < pinned.index(
            "__entry_anchor --- rh1"
        )
        # Anchor after subgraph end, not inside
        assert pinned.index("end\n") < pinned.index("__entry_anchor(( ))")
        # 5 edges inside subgraph: -->, -->, -->, -->, -->
        # linkStyle 5 hides the anchor edge, not any business edge
        assert "linkStyle 5 stroke:none,stroke-width:0px" in pinned

    def test_fallback_no_subgraph_deterministic(self) -> None:
        """Deterministic diagram (no subgraph) — anchor after target definition."""
        mermaid = """graph TD
  main["Main"]
  tool_a["tool_a"]
  main -.-> tool_a"""
        pinned = _pin_entry_to_top(mermaid, "main")
        # main["Main"] defined before anchor references it (no bare ID)
        assert pinned.index('main["Main"]') < pinned.index("__entry_anchor --- main")
        # 0 edges before and including main["Main"], so linkStyle 0
        assert "linkStyle 0 stroke:none,stroke-width:0px" in pinned
        # The business edge main -.-> tool_a is NOT hidden (it's linkStyle 1)
        assert "linkStyle 1" not in pinned

    def test_wrong_anchor_is_stripped_and_repinned(self) -> None:
        """LLM-copied anchor with wrong linkStyle is stripped and re-injected correctly."""
        mermaid = """graph TD
  subgraph reservation_node["Reservation Node"]
    res1[Greet and collect info] --> res_time[Check 11:00-21:00 range]
    res_time -- Out of range --> res1
    res_time -- In range --> res2[Check availability]
    res2 -- Available --> res3[Make reservation]
    res2 -- Unavailable --> res4[Suggest other times]
    res4 --> res1
    res3 --> res5[Ask for special requests]
    res5 --> res6[Confirm and goodbye]
  end
  __entry_anchor(( ))
  style __entry_anchor fill:none,stroke:none
  __entry_anchor --- res1
  linkStyle 7 stroke:none,stroke-width:0px
  check_availability["check_availability"]
  make_reservation["make_reservation"]
  reservation_node -.-> check_availability
  reservation_node -.-> make_reservation"""
        pinned = _pin_entry_to_top(mermaid, "reservation_node")
        assert pinned.count("__entry_anchor(( ))") == 1
        # 8 edges inside subgraph → anchor edge is index 8, not the LLM's wrong 7
        assert "linkStyle 8 stroke:none,stroke-width:0px" in pinned
        assert "linkStyle 7 stroke:none,stroke-width:0px" not in pinned

    def test_existing_correct_anchor_is_repinned(self) -> None:
        """A correct anchor is stripped and re-injected — still results in exactly one correct block."""
        mermaid = """graph TD
  subgraph reservation_node["Reservation Node"]
    rh1[Greet and ask details] --> rh2[Check availability]
    rh2 -- Available --> rh3[Make reservation]
    rh3 --> rh4[Confirm and end]
    rh2 -- Unavailable --> rh5[Suggest alternatives]
    rh5 --> rh1
  end
  __entry_anchor(( ))
  style __entry_anchor fill:none,stroke:none
  __entry_anchor --- rh1
  linkStyle 5 stroke:none,stroke-width:0px
  check_availability["check_availability"]
  make_reservation["make_reservation"]
  reservation_node -.-> check_availability
  reservation_node -.-> make_reservation"""
        pinned = _pin_entry_to_top(mermaid, "reservation_node")
        assert pinned.count("__entry_anchor(( ))") == 1
        assert "__entry_anchor --- rh1" in pinned
        assert "linkStyle 5 stroke:none,stroke-width:0px" in pinned


class TestStripEntryAnchorBlock:
    """Tests for _strip_entry_anchor_block()."""

    def test_strips_full_block(self) -> None:
        mermaid = """graph TD
  main["Main"]
  __entry_anchor(( ))
  style __entry_anchor fill:none,stroke:none
  __entry_anchor --- main
  linkStyle 0 stroke:none,stroke-width:0px
  tool_a["tool_a"]"""
        stripped = _strip_entry_anchor_block(mermaid)
        assert "__entry_anchor" not in stripped
        assert "linkStyle 0 stroke:none,stroke-width:0px" not in stripped
        assert 'main["Main"]' in stripped
        assert 'tool_a["tool_a"]' in stripped

    def test_no_anchor_passthrough(self) -> None:
        mermaid = """graph TD
  main["Main"]
  tool_a["tool_a"]
  main -.-> tool_a"""
        assert _strip_entry_anchor_block(mermaid) == mermaid


class TestMermaidPromptConstraints:
    """Ensure critical continuity rules exist in the system prompt."""

    def test_prompt_has_continuity_rules(self) -> None:
        from app.agent_builder.graph import _MERMAID_SYSTEM_PROMPT

        assert "MUST start from the previous diagram" in _MERMAID_SYSTEM_PROMPT
        assert "MINIMAL modifications" in _MERMAID_SYSTEM_PROMPT
        assert "Use the change_summary as a hint" in _MERMAID_SYSTEM_PROMPT
        assert "prefer annotating or locally refining" in _MERMAID_SYSTEM_PROMPT
