"""Tests for artifact tool helper functions (app.agent_builder.tools.artifacts).

Covers the pure helper functions that don't require DB or agent setup:
  - _slice_text: line-range slicing of text content
  - _search_text: keyword search with context
  - _format_read_result: formatting FileStore ReadResult objects
"""

from app.agent_builder.tools.artifacts import (
    _format_read_result,
    _search_text,
    _slice_text,
)
from app.lib.file_store import MAX_READ_LINES, MAX_SEARCH_HITS, ReadResult

# ══════════════════════════════════════════════════════════════════
# _slice_text
# ══════════════════════════════════════════════════════════════════


SAMPLE = "\n".join(f"Line {i}" for i in range(1, 21))  # 20 lines


class TestSliceText:
    def test_empty_content(self) -> None:
        result = _slice_text("", 0, 0, "empty.md")
        assert "empty (0 lines)" in result

    def test_default_returns_first_max_lines(self) -> None:
        big = "\n".join(f"Row {i}" for i in range(1, MAX_READ_LINES + 200))
        result = _slice_text(big, 0, 0, "big.md")
        assert f"lines 1–{MAX_READ_LINES}" in result
        assert "Row 1" in result
        assert f"Row {MAX_READ_LINES}" in result
        assert f"Row {MAX_READ_LINES + 1}" not in result
        assert "more lines" in result

    def test_default_short_doc_shows_all(self) -> None:
        result = _slice_text(SAMPLE, 0, 0, "short.md")
        assert "lines 1–20 of 20 total" in result
        assert "Line 1" in result
        assert "Line 20" in result
        # No "more lines" footer since all lines are shown
        assert "more lines" not in result

    def test_explicit_range(self) -> None:
        result = _slice_text(SAMPLE, 5, 10, "range.md")
        assert "lines 5–10 of 20 total" in result
        assert "Line 5" in result
        assert "Line 10" in result
        assert "Line 4" not in result
        assert "Line 11" not in result

    def test_range_clamps_start(self) -> None:
        result = _slice_text(SAMPLE, -5, 3, "clamp.md")
        assert "lines 1–3" in result

    def test_range_clamps_end(self) -> None:
        result = _slice_text(SAMPLE, 18, 999, "clamp.md")
        assert "lines 18–20" in result
        assert "more lines" not in result

    def test_range_caps_at_max_read_lines(self) -> None:
        big = "\n".join(f"R{i}" for i in range(1, 2000))
        result = _slice_text(big, 100, 1500, "cap.md")
        # Should be capped to MAX_READ_LINES from start
        expected_end = 100 + MAX_READ_LINES - 1
        assert f"lines 100–{expected_end}" in result

    def test_footer_includes_continue_hint(self) -> None:
        result = _slice_text(SAMPLE, 1, 5, "hint.md")
        assert "read_artifact('hint.md', start_line=6)" in result

    def test_only_start_line_specified(self) -> None:
        """When start_line > 0 but end_line <= 0, should default end."""
        result = _slice_text(SAMPLE, 10, 0, "partial.md")
        # end should default to min(start + MAX_READ_LINES - 1, total)
        assert "lines 10–20" in result


# ══════════════════════════════════════════════════════════════════
# _search_text
# ══════════════════════════════════════════════════════════════════


class TestSearchText:
    def test_empty_query(self) -> None:
        result = _search_text(SAMPLE, "test.md", "")
        assert "must not be empty" in result.lower()

    def test_whitespace_only_query(self) -> None:
        result = _search_text(SAMPLE, "test.md", "   ")
        assert "must not be empty" in result.lower()

    def test_no_matches(self) -> None:
        result = _search_text(SAMPLE, "test.md", "nonexistent_term_xyz")
        assert "No matches" in result
        assert "20 lines" in result

    def test_single_match(self) -> None:
        result = _search_text(SAMPLE, "test.md", "Line 15")
        assert "1 match" in result
        assert "Line 15" in result

    def test_case_insensitive(self) -> None:
        content = "Hello World\nhello world\nHELLO WORLD"
        result = _search_text(content, "case.md", "hello")
        assert "3 match" in result

    def test_context_lines_present(self) -> None:
        # Search for Line 10 — should have 3 context lines before and after
        result = _search_text(SAMPLE, "test.md", "Line 10")
        # Context before should include Line 7, 8, 9
        assert "Line 7" in result
        assert "Line 8" in result
        assert "Line 9" in result
        # Context after should include Line 11, 12, 13
        assert "Line 11" in result
        assert "Line 12" in result
        assert "Line 13" in result

    def test_context_clamps_at_start(self) -> None:
        result = _search_text(SAMPLE, "test.md", "Line 1\n")
        # "Line 1" is on line 1 — no context before, but we need exact match
        # Actually "Line 1\n" won't match since we search per-line.
        # Let's use a different approach
        content = "Target line\nSecond\nThird"
        result = _search_text(content, "test.md", "Target")
        # No context before the first line
        assert "--- Line 1 ---" in result

    def test_context_clamps_at_end(self) -> None:
        result = _search_text(SAMPLE, "test.md", "Line 20")
        assert "--- Line 20 ---" in result

    def test_many_matches_truncated(self) -> None:
        content = "\n".join(f"match row {i}" for i in range(50))
        result = _search_text(content, "test.md", "match")
        assert f"showing first {MAX_SEARCH_HITS}" in result

    def test_exactly_max_hits_not_truncated(self) -> None:
        content = "\n".join(f"match row {i}" for i in range(MAX_SEARCH_HITS))
        result = _search_text(content, "test.md", "match")
        # Should show all hits without truncation message
        assert "showing first" not in result
        assert f"{MAX_SEARCH_HITS} match" in result


# ══════════════════════════════════════════════════════════════════
# _format_read_result
# ══════════════════════════════════════════════════════════════════


class TestFormatReadResult:
    def test_empty_result(self) -> None:
        result = ReadResult(
            file_id="f1",
            filename="empty.txt",
            total_lines=0,
            start_line=0,
            end_line=0,
            content="",
        )
        formatted = _format_read_result(result)
        assert "empty (0 lines)" in formatted

    def test_full_file(self) -> None:
        result = ReadResult(
            file_id="f1",
            filename="small.txt",
            total_lines=5,
            start_line=1,
            end_line=5,
            content="a\nb\nc\nd\ne",
        )
        formatted = _format_read_result(result)
        assert "lines 1–5 of 5 total" in formatted
        assert "more lines" not in formatted

    def test_partial_with_more(self) -> None:
        result = ReadResult(
            file_id="f1",
            filename="big.txt",
            total_lines=1500,
            start_line=1,
            end_line=800,
            content="some content",
        )
        formatted = _format_read_result(result)
        assert "lines 1–800 of 1500 total" in formatted
        assert "700 more lines" in formatted
        assert "read_artifact('f1', start_line=801)" in formatted

    def test_middle_range(self) -> None:
        result = ReadResult(
            file_id="abc-123",
            filename="doc.md",
            total_lines=2000,
            start_line=500,
            end_line=700,
            content="middle content",
        )
        formatted = _format_read_result(result)
        assert "lines 500–700 of 2000 total" in formatted
        assert "read_artifact('abc-123', start_line=701)" in formatted
