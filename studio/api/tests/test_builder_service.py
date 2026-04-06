"""Tests for the builder service helper functions."""

from typing import Any

from app.agent_builder.service import _normalize_escaped_tool_scripts


def _cfg_with_script(script: str) -> dict[str, Any]:
    """Build a minimal config dict wrapping a single tool script."""
    return {"tools": {"t": {"script": script}}}


def _script_out(cfg: dict[str, Any]) -> str:
    """Extract the script string from the normalized config."""
    return cfg["tools"]["t"]["script"]


def _make_validator(valid_scripts: set[str] | None = None):
    """Create a fake validate_javascript that treats listed scripts as valid.

    If *valid_scripts* is ``None`` every script is considered valid (no errors).
    """

    def fake_validate(script: str) -> list[str]:
        if valid_scripts is None:
            return []
        if script in valid_scripts:
            return []
        return ["syntax error"]

    return fake_validate


# ---------------------------------------------------------------------------
# Helper to run the function with a controlled validator
# ---------------------------------------------------------------------------


def _normalize_with_validator(
    cfg: dict[str, Any],
    validator: Any = None,
) -> dict[str, Any]:
    """Run _normalize_escaped_tool_scripts with a controlled import environment.

    *validator*:
      - callable → used as validate_javascript (has_js_validator=True)
      - None     → voice_engine import fails   (has_js_validator=False)
    """
    if validator is not None:
        # Make voice_engine importable with our fake validator
        import sys
        import types

        fake_mod = types.ModuleType("voice_engine")
        fake_mod.validate_javascript = validator  # type: ignore[attr-defined]
        saved = sys.modules.get("voice_engine")
        sys.modules["voice_engine"] = fake_mod
        try:
            return _normalize_escaped_tool_scripts(cfg)
        finally:
            if saved is not None:
                sys.modules["voice_engine"] = saved
            else:
                sys.modules.pop("voice_engine", None)
    else:
        # Block voice_engine import
        import sys

        saved = sys.modules.get("voice_engine")
        sys.modules["voice_engine"] = None  # type: ignore[assignment]
        try:
            return _normalize_escaped_tool_scripts(cfg)
        finally:
            if saved is not None:
                sys.modules["voice_engine"] = saved
            else:
                sys.modules.pop("voice_engine", None)


# ===================================================================
# Tests: no-op / skip paths
# ===================================================================


class TestNormalizeSkipPaths:
    """Cases where the function should not touch the config."""

    def test_no_tools_key(self) -> None:
        cfg: dict[str, Any] = {"nodes": {}}
        assert _normalize_escaped_tool_scripts(cfg) is cfg

    def test_tools_not_a_dict(self) -> None:
        cfg: dict[str, Any] = {"tools": ["a", "b"]}
        assert _normalize_escaped_tool_scripts(cfg) is cfg

    def test_tool_value_not_a_dict(self) -> None:
        cfg: dict[str, Any] = {"tools": {"t": "just a string"}}
        assert _normalize_escaped_tool_scripts(cfg) == cfg

    def test_script_not_a_string(self) -> None:
        cfg: dict[str, Any] = {"tools": {"t": {"script": 42}}}
        assert _normalize_escaped_tool_scripts(cfg) == cfg

    def test_script_without_escaped_newlines(self) -> None:
        cfg = _cfg_with_script("return 1;")
        assert _script_out(_normalize_escaped_tool_scripts(cfg)) == "return 1;"

    def test_script_with_real_newlines_only(self) -> None:
        """A script that already has real newlines and no escaped ones is untouched."""
        script = "return 1;\nconsole.log(2);"
        cfg = _cfg_with_script(script)
        assert _script_out(_normalize_escaped_tool_scripts(cfg)) == script


# ===================================================================
# Tests: basic unescaping (no validator dependency)
# ===================================================================


class TestNormalizeUnescaping:
    r"""Core unescaping: lone \n → LF, \r\n → CRLF, \\n preserved."""

    def test_unescape_lone_backslash_n(self) -> None:
        r"""A lone \n (backslash + n) becomes a real newline."""
        script = "return 1;\\nreturn 2;"
        cfg = _cfg_with_script(script)
        res = _normalize_escaped_tool_scripts(cfg)
        assert _script_out(res) == "return 1;\nreturn 2;"

    def test_unescape_backslash_r_backslash_n(self) -> None:
        r"""A \r\n sequence becomes a real CRLF, not just LF."""
        script = "return 1;\\r\\nreturn 2;"
        cfg = _cfg_with_script(script)
        res = _normalize_escaped_tool_scripts(cfg)
        assert _script_out(res) == "return 1;\r\nreturn 2;"

    def test_preserve_double_backslash_n(self) -> None:
        r"""\\n (two backslashes + n) must NOT be unescaped — it's a JS literal."""
        script = "return '\\\\n';"
        cfg = _cfg_with_script(script)
        res = _normalize_escaped_tool_scripts(cfg)
        assert _script_out(res) == script

    def test_mixed_double_and_single_backslash(self) -> None:
        r"""\\n is preserved while adjacent \n is decoded."""
        script = "return '\\\\n';\\nconsole.log(1);"
        cfg = _cfg_with_script(script)
        res = _normalize_escaped_tool_scripts(cfg)
        expected = "return '\\\\n';\nconsole.log(1);"
        assert _script_out(res) == expected

    def test_multiple_escaped_newlines(self) -> None:
        r"""Multiple \n sequences are all decoded."""
        script = "a();\\nb();\\nc();"
        cfg = _cfg_with_script(script)
        res = _normalize_escaped_tool_scripts(cfg)
        assert _script_out(res) == "a();\nb();\nc();"


# ===================================================================
# Tests: with JS validator (controls the validator-dependent logic)
# ===================================================================


class TestNormalizeWithValidator:
    """Tests that exercise the validator-gated replacement path."""

    def test_valid_raw_script_not_replaced(self) -> None:
        r"""If the raw script is valid JS, it is NOT replaced even if it
        contains \n that could be decoded.

        Example: return '\n'; is valid JS (backslash-n escape in a string).
        Decoding it to a real newline would BREAK the string literal.
        """
        script = "return '\\n';"
        # Mark the raw script as valid — validator says "no errors"
        validator = _make_validator(valid_scripts={script})
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=validator)
        assert _script_out(res) == script

    def test_invalid_raw_valid_decoded_is_replaced(self) -> None:
        r"""If raw fails validation but decoded passes, the script is replaced."""
        script = "let x = 1;\\nreturn x;"
        decoded = "let x = 1;\nreturn x;"
        # Raw fails, decoded passes
        validator = _make_validator(valid_scripts={decoded})
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=validator)
        assert _script_out(res) == decoded

    def test_both_invalid_not_replaced(self) -> None:
        r"""If both raw and decoded fail validation, no replacement is made."""
        script = "console.log(\\n"
        # Neither raw nor decoded is valid
        validator = _make_validator(valid_scripts=set())
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=validator)
        assert _script_out(res) == script

    def test_complex_mixed_escaping_with_validator(self) -> None:
        r"""A script mixing regex escapes (\w) and artificial \n newlines."""
        script = "const regex = /\\w+/;\\nconsole.log('string with \\\\n inside');"
        decoded = "const regex = /\\w+/;\nconsole.log('string with \\\\n inside');"
        # Raw fails, decoded passes
        validator = _make_validator(valid_scripts={decoded})
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=validator)
        assert _script_out(res) == decoded


# ===================================================================
# Tests: without JS validator (heuristic fallback path)
# ===================================================================


class TestNormalizeWithoutValidator:
    """Tests that exercise the heuristic fallback when voice_engine is unavailable."""

    def test_heuristic_replaces_when_no_real_newlines_in_original(self) -> None:
        r"""Without a validator, replace if original has no real newlines but decoded does."""
        script = "return 1;\\nreturn 2;"
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=None)
        assert _script_out(res) == "return 1;\nreturn 2;"

    def test_heuristic_skips_when_original_has_real_newlines(self) -> None:
        r"""If original already has real newlines, the heuristic does NOT replace."""
        script = "return 1;\nconsole.log('\\n');"
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=None)
        assert _script_out(res) == script

    def test_heuristic_does_replace_valid_js_backslash_n(self) -> None:
        r"""return '\n'; has no real newlines, so the heuristic WILL replace it.

        This is a known limitation of the heuristic path — without a validator,
        valid JS string escapes can be incorrectly decoded. The validator path
        exists to prevent this.
        """
        script = "return '\\n';"
        cfg = _cfg_with_script(script)
        res = _normalize_with_validator(cfg, validator=None)
        # Heuristic fires: no real newlines in original, real newline in decoded.
        # This is "wrong" but expected without the validator.
        assert _script_out(res) == "return '\n';"


# ===================================================================
# Tests: multiple tools in one config
# ===================================================================


class TestNormalizeMultipleTools:
    """Ensure each tool is processed independently."""

    def test_multiple_tools_processed_independently(self) -> None:
        cfg: dict[str, Any] = {
            "tools": {
                "clean": {"script": "return 1;"},
                "escaped": {"script": "a();\\nb();"},
                "already_multiline": {"script": "x();\ny();"},
            }
        }
        res = _normalize_escaped_tool_scripts(cfg)
        assert res["tools"]["clean"]["script"] == "return 1;"
        assert res["tools"]["escaped"]["script"] == "a();\nb();"
        assert res["tools"]["already_multiline"]["script"] == "x();\ny();"
