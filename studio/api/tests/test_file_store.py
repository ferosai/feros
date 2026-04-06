"""Tests for the file store (app.lib.file_store).

Covers FileStore operations: add, read, read_range, search,
list, remove, TTL eviction, and disk cleanup.
Also covers extract_text for supported formats.
"""

import json
import time

import pytest

from app.lib.file_store import (
    FileStore,
    extract_text,
)

# ── Fixtures ─────────────────────────────────────────────────────

SAMPLE_TEXT = """\
Line 1: Introduction
Line 2: This is a sample document.
Line 3: It contains multiple lines.
Line 4: Some lines mention PYTHON programming.
Line 5: Others mention JavaScript.
Line 6: And some mention both Python and javascript together.
Line 7: This line is about testing.
Line 8: Unit tests are important.
Line 9: Integration tests too.
Line 10: End of document."""

LONG_TEXT = "\n".join(f"Line {i}: content row {i}" for i in range(1, 1501))


@pytest.fixture
def store(tmp_path: object) -> FileStore:
    """Return a fresh FileStore backed by a temp directory."""
    return FileStore(base_dir=str(tmp_path), ttl=300)


@pytest.fixture
def populated_store(store: FileStore) -> FileStore:
    """Return a FileStore with one sample document already loaded."""
    store.add_text_file("agent-1", "doc-1", "sample.txt", SAMPLE_TEXT)
    return store


# ══════════════════════════════════════════════════════════════════
# FileStore.add_document
# ══════════════════════════════════════════════════════════════════


class TestAddDocument:
    def test_basic_add(self, store: FileStore) -> None:
        meta = store.add_text_file("agent-1", "doc-1", "test.txt", SAMPLE_TEXT)
        assert meta.file_id == "doc-1"
        assert meta.filename == "test.txt"
        assert meta.total_lines == 10

    def test_empty_text(self, store: FileStore) -> None:
        meta = store.add_text_file("agent-1", "doc-1", "empty.txt", "")
        assert meta.total_lines == 0

    def test_single_line_no_newline(self, store: FileStore) -> None:
        meta = store.add_text_file("agent-1", "doc-1", "one.txt", "hello world")
        assert meta.total_lines == 1

    def test_trailing_newline(self, store: FileStore) -> None:
        meta = store.add_text_file("agent-1", "doc-1", "trail.txt", "a\nb\n")
        assert meta.total_lines == 2

    def test_multiple_docs_same_agent(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "doc-1", "a.txt", "one")
        store.add_text_file("agent-1", "doc-2", "b.txt", "two")
        assert len(store.list_files("agent-1")) == 2

    def test_multiple_agents_isolated(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "doc-1", "a.txt", "one")
        store.add_text_file("agent-2", "doc-1", "b.txt", "two")
        assert len(store.list_files("agent-1")) == 1
        assert len(store.list_files("agent-2")) == 1

    def test_writes_to_disk(self, store: FileStore, tmp_path: object) -> None:
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello")
        from pathlib import Path

        content_path = Path(str(tmp_path)) / "agent-1" / "doc-1" / "content.txt"
        meta_path = Path(str(tmp_path)) / "agent-1" / "doc-1" / "meta.json"
        assert content_path.exists()
        assert meta_path.exists()
        assert content_path.read_text() == "hello"


# ══════════════════════════════════════════════════════════════════
# FileStore.list_documents
# ══════════════════════════════════════════════════════════════════


class TestListDocuments:
    def test_list_empty(self, store: FileStore) -> None:
        assert store.list_files("nonexistent") == []

    def test_list_returns_metadata(self, populated_store: FileStore) -> None:
        docs = populated_store.list_files("agent-1")
        assert len(docs) == 1
        doc = docs[0]
        assert doc["file_id"] == "doc-1"
        assert doc["filename"] == "sample.txt"
        assert doc["total_lines"] == 10

    def test_list_reads_from_disk(self, store: FileStore) -> None:
        """list_documents should work even if cache is empty."""
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello\nworld")
        # Manually clear the in-memory cache
        store._cache.clear()
        docs = store.list_files("agent-1")
        assert len(docs) == 1
        assert docs[0]["total_lines"] == 2


# ══════════════════════════════════════════════════════════════════
# FileStore.read_doc
# ══════════════════════════════════════════════════════════════════


class TestReadDoc:
    def test_read_full_short_doc(self, populated_store: FileStore) -> None:
        result = populated_store.read_text("agent-1", "doc-1")
        assert result is not None
        assert result.total_lines == 10
        assert result.start_line == 1
        assert result.end_line == 10
        assert "Line 1: Introduction" in result.content
        assert "Line 10: End of document." in result.content

    def test_read_caps_at_800_lines(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "big", "big.txt", LONG_TEXT)
        result = store.read_text("agent-1", "big")
        assert result is not None
        assert result.total_lines == 1500
        assert result.start_line == 1
        assert result.end_line == 800
        assert "Line 1:" in result.content
        assert "Line 800:" in result.content
        assert "Line 801:" not in result.content

    def test_read_custom_max_lines(self, populated_store: FileStore) -> None:
        result = populated_store.read_text("agent-1", "doc-1", max_lines=3)
        assert result is not None
        assert result.end_line == 3
        assert "Line 4:" not in result.content

    def test_read_nonexistent_doc(self, populated_store: FileStore) -> None:
        assert populated_store.read_text("agent-1", "no-such-doc") is None

    def test_read_nonexistent_agent(self, populated_store: FileStore) -> None:
        assert populated_store.read_text("no-agent", "doc-1") is None

    def test_read_loads_from_disk_after_cache_eviction(self, store: FileStore) -> None:
        """After cache is cleared, read_doc should reload from disk."""
        store.add_text_file("agent-1", "doc-1", "test.txt", SAMPLE_TEXT)
        store._cache.clear()  # simulate eviction
        result = store.read_text("agent-1", "doc-1")
        assert result is not None
        assert result.total_lines == 10
        assert "Line 1: Introduction" in result.content


# ══════════════════════════════════════════════════════════════════
# FileStore.read_doc_range
# ══════════════════════════════════════════════════════════════════


class TestReadDocRange:
    def test_basic_range(self, populated_store: FileStore) -> None:
        result = populated_store.read_text_range("agent-1", "doc-1", 3, 5)
        assert result is not None
        assert result.start_line == 3
        assert result.end_line == 5
        assert "Line 3:" in result.content
        assert "Line 5:" in result.content
        assert "Line 2:" not in result.content
        assert "Line 6:" not in result.content

    def test_single_line_range(self, populated_store: FileStore) -> None:
        result = populated_store.read_text_range("agent-1", "doc-1", 7, 7)
        assert result is not None
        assert result.start_line == 7
        assert result.end_line == 7
        assert "testing" in result.content

    def test_range_clamps_to_bounds(self, populated_store: FileStore) -> None:
        result = populated_store.read_text_range("agent-1", "doc-1", -5, 3)
        assert result is not None
        assert result.start_line == 1

    def test_range_clamps_end_to_total(self, populated_store: FileStore) -> None:
        result = populated_store.read_text_range("agent-1", "doc-1", 8, 9999)
        assert result is not None
        assert result.end_line == 10

    def test_range_start_past_end_returns_empty(
        self, populated_store: FileStore
    ) -> None:
        result = populated_store.read_text_range("agent-1", "doc-1", 15, 20)
        assert result is not None
        assert result.content == ""

    def test_range_caps_at_800_lines(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "big", "big.txt", LONG_TEXT)
        result = store.read_text_range("agent-1", "big", 100, 1200)
        assert result is not None
        # Should only return 800 lines: 100..899
        assert result.start_line == 100
        assert result.end_line == 899

    def test_range_nonexistent_doc(self, populated_store: FileStore) -> None:
        assert populated_store.read_text_range("agent-1", "nope", 1, 5) is None


# ══════════════════════════════════════════════════════════════════
# FileStore.search_doc
# ══════════════════════════════════════════════════════════════════


class TestSearchDoc:
    def test_basic_keyword_search(self, populated_store: FileStore) -> None:
        result = populated_store.search_text("agent-1", "doc-1", "testing")
        assert result is not None
        assert result.total_hits == 1
        assert len(result.hits) == 1
        assert result.hits[0].line_number == 7
        assert "testing" in result.hits[0].line_content

    def test_case_insensitive_by_default(self, populated_store: FileStore) -> None:
        result = populated_store.search_text("agent-1", "doc-1", "python")
        assert result is not None
        # Should match "PYTHON" on line 4 and "Python" on line 6
        assert result.total_hits == 2

    def test_case_sensitive_search(self, populated_store: FileStore) -> None:
        result = populated_store.search_text(
            "agent-1", "doc-1", "PYTHON", case_insensitive=False
        )
        assert result is not None
        assert result.total_hits == 1
        assert result.hits[0].line_number == 4

    def test_no_matches(self, populated_store: FileStore) -> None:
        result = populated_store.search_text("agent-1", "doc-1", "nonexistent_word")
        assert result is not None
        assert result.total_hits == 0
        assert result.hits == []

    def test_empty_query(self, populated_store: FileStore) -> None:
        result = populated_store.search_text("agent-1", "doc-1", "")
        assert result is not None
        assert result.total_hits == 0

    def test_context_lines_present(self, populated_store: FileStore) -> None:
        result = populated_store.search_text(
            "agent-1", "doc-1", "testing", context_lines=2
        )
        assert result is not None
        hit = result.hits[0]
        assert hit.line_number == 7
        # 2 context lines before: lines 5, 6
        assert len(hit.context_before) == 2
        # 2 context lines after: lines 8, 9
        assert len(hit.context_after) == 2

    def test_context_clamps_at_start(self, populated_store: FileStore) -> None:
        """Searching the first line shouldn't produce negative-index context."""
        result = populated_store.search_text(
            "agent-1", "doc-1", "Introduction", context_lines=5
        )
        assert result is not None
        hit = result.hits[0]
        assert hit.line_number == 1
        assert len(hit.context_before) == 0

    def test_context_clamps_at_end(self, populated_store: FileStore) -> None:
        """Searching the last line shouldn't exceed doc length."""
        result = populated_store.search_text(
            "agent-1", "doc-1", "End of document", context_lines=5
        )
        assert result is not None
        hit = result.hits[0]
        assert hit.line_number == 10
        assert len(hit.context_after) == 0

    def test_max_hits_cap(self, store: FileStore) -> None:
        """When many lines match, hits are capped but total_hits is accurate."""
        text = "\n".join(f"match line {i}" for i in range(100))
        store.add_text_file("agent-1", "doc-1", "many.txt", text)
        result = store.search_text("agent-1", "doc-1", "match", max_hits=5)
        assert result is not None
        assert len(result.hits) == 5
        assert result.total_hits == 100

    def test_special_regex_chars_escaped(self, store: FileStore) -> None:
        """Characters like . * + should be treated as literals, not regex."""
        store.add_text_file(
            "agent-1", "doc-1", "regex.txt", "price is $10.00\nprice is $1000"
        )
        result = store.search_text("agent-1", "doc-1", "$10.00")
        assert result is not None
        assert result.total_hits == 1
        assert "10.00" in result.hits[0].line_content

    def test_search_nonexistent_doc(self, populated_store: FileStore) -> None:
        assert populated_store.search_text("agent-1", "nope", "test") is None


# ══════════════════════════════════════════════════════════════════
# FileStore.remove_*
# ══════════════════════════════════════════════════════════════════


class TestRemoveDocument:
    def test_remove_single_document(self, populated_store: FileStore) -> None:
        assert populated_store.remove_file("agent-1", "doc-1") is True
        assert populated_store.read_text("agent-1", "doc-1") is None
        # Disk should also be gone
        assert not populated_store._file_dir("agent-1", "doc-1").exists()

    def test_remove_nonexistent_document(self, populated_store: FileStore) -> None:
        assert populated_store.remove_file("agent-1", "nope") is False

    def test_remove_all_agent_docs(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "doc-1", "a.txt", "a")
        store.add_text_file("agent-1", "doc-2", "b.txt", "b")
        count = store.remove_agent_files("agent-1")
        assert count == 2
        assert store.list_files("agent-1") == []
        # Entire agent dir should be gone
        assert not (store._base_dir / "agent-1").exists()

    def test_remove_agent_docs_doesnt_affect_other_agents(
        self, store: FileStore
    ) -> None:
        store.add_text_file("agent-1", "doc-1", "a.txt", "a")
        store.add_text_file("agent-2", "doc-2", "b.txt", "b")
        store.remove_agent_files("agent-1")
        assert len(store.list_files("agent-2")) == 1

    def test_remove_clears_both_cache_and_disk(self, store: FileStore) -> None:
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello")
        assert store.remove_file("agent-1", "doc-1") is True
        # Not in cache
        assert store._cache.get("agent-1", {}).get("doc-1") is None
        # Not on disk
        assert not store._text_path("agent-1", "doc-1").exists()


# ══════════════════════════════════════════════════════════════════
# TTL eviction
# ══════════════════════════════════════════════════════════════════


class TestTTLEviction:
    def test_evict_expired_entries(self, tmp_path: object) -> None:
        """Entries older than TTL are evicted from cache but stay on disk."""
        store = FileStore(base_dir=str(tmp_path), ttl=0)  # 0s = instant expiry
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello")

        # Force the entry to be expired by backdating last_accessed
        entry = store._cache["agent-1"]["doc-1"]
        entry.last_accessed = time.monotonic() - 10

        evicted = store.evict_expired()
        assert evicted == 1
        assert "agent-1" not in store._cache

        # Disk should still have the file
        assert store._text_path("agent-1", "doc-1").exists()

        # Should still be readable (reloads from disk)
        result = store.read_text("agent-1", "doc-1")
        assert result is not None
        assert result.content == "hello"

    def test_active_entry_not_evicted(self, tmp_path: object) -> None:
        """Recently accessed entries should not be evicted."""
        store = FileStore(base_dir=str(tmp_path), ttl=300)
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello")

        evicted = store.evict_expired()
        assert evicted == 0
        assert "doc-1" in store._cache.get("agent-1", {})

    def test_access_refreshes_ttl(self, tmp_path: object) -> None:
        """Accessing a document should reset its TTL."""
        store = FileStore(base_dir=str(tmp_path), ttl=5)
        store.add_text_file("agent-1", "doc-1", "test.txt", "hello")

        # Backdate
        entry = store._cache["agent-1"]["doc-1"]
        old_time = time.monotonic() - 10
        entry.last_accessed = old_time

        # Access should refresh
        store.read_text("agent-1", "doc-1")
        assert entry.last_accessed > old_time


# ══════════════════════════════════════════════════════════════════
# Disk cleanup
# ══════════════════════════════════════════════════════════════════


class TestDiskCleanup:
    def test_cleanup_old_files(self, tmp_path: object) -> None:
        store = FileStore(base_dir=str(tmp_path), ttl=300)
        store.add_text_file("agent-1", "doc-1", "old.txt", "old content")

        # Backdate the metadata on disk
        meta_path = store._meta_path("agent-1", "doc-1")
        meta_data = json.loads(meta_path.read_text())
        meta_data["created_at"] = time.time() - 7200  # 2 hours ago
        meta_path.write_text(json.dumps(meta_data))

        removed = store.cleanup_expired_disk(max_age_seconds=3600)
        assert removed == 1
        assert not store._file_dir("agent-1", "doc-1").exists()

    def test_cleanup_keeps_fresh_files(self, tmp_path: object) -> None:
        store = FileStore(base_dir=str(tmp_path), ttl=300)
        store.add_text_file("agent-1", "doc-1", "fresh.txt", "fresh content")

        removed = store.cleanup_expired_disk(max_age_seconds=3600)
        assert removed == 0
        assert store._file_dir("agent-1", "doc-1").exists()

    def test_cleanup_nonexistent_base_dir(self) -> None:
        store = FileStore(base_dir="/tmp/nonexistent_studio_test_dir", ttl=300)
        removed = store.cleanup_expired_disk()
        assert removed == 0


# ══════════════════════════════════════════════════════════════════
# extract_text
# ══════════════════════════════════════════════════════════════════


class TestExtractText:
    def test_txt_file(self) -> None:
        content = b"Hello, world!\nSecond line."
        text = extract_text(content, "test.txt")
        assert text == "Hello, world!\nSecond line."

    def test_md_file(self) -> None:
        content = b"# Heading\n\nParagraph."
        text = extract_text(content, "readme.md")
        assert "# Heading" in text

    def test_txt_handles_utf8(self) -> None:
        content = "Héllo wörld café".encode()
        text = extract_text(content, "unicode.txt")
        assert "café" in text

    def test_txt_handles_bad_encoding(self) -> None:
        content = b"\xff\xfe invalid bytes"
        text = extract_text(content, "bad.txt")
        # Should not raise, just replace bad chars
        assert isinstance(text, str)

    def test_unknown_extension_falls_back_to_text(self) -> None:
        content = b"plain text content"
        text = extract_text(content, "data.csv")
        assert text == "plain text content"

    def test_pdf_extraction(self) -> None:
        """Verify PDF extraction works (requires pymupdf)."""
        try:
            import fitz
        except ImportError:
            pytest.skip("pymupdf not installed")

        # Create a minimal PDF in memory
        doc = fitz.open()
        page = doc.new_page()
        page.insert_text((72, 72), "Hello from PDF")
        pdf_bytes = doc.tobytes()
        doc.close()

        text = extract_text(pdf_bytes, "test.pdf")
        assert "Hello from PDF" in text

    def test_docx_extraction(self) -> None:
        """Verify DOCX extraction works (requires python-docx)."""
        try:
            from docx import Document
        except ImportError:
            pytest.skip("python-docx not installed")

        import io

        doc = Document()
        doc.add_paragraph("Hello from DOCX")
        buf = io.BytesIO()
        doc.save(buf)

        text = extract_text(buf.getvalue(), "test.docx")
        assert "Hello from DOCX" in text


# ══════════════════════════════════════════════════════════════════
# Path traversal / ID injection prevention
# ══════════════════════════════════════════════════════════════════


class TestPathTraversalPrevention:
    """Verify that crafted agent_id/doc_id values can't escape the sandbox."""

    MALICIOUS_IDS = [
        "../../../etc",
        "../../passwd",
        "/absolute/path",
        "foo/bar",
        "foo\\bar",
        "..",
        ".",
        "",
        "hello world",  # spaces
        "id;rm -rf /",  # shell injection
        "id\x00null",  # null byte
    ]

    def test_add_document_rejects_bad_agent_id(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            with pytest.raises(ValueError):
                store.add_text_file(bad_id, "doc-1", "test.txt", "hello")

    def test_add_document_rejects_bad_doc_id(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            with pytest.raises(ValueError):
                store.add_text_file("agent-1", bad_id, "test.txt", "hello")

    def test_read_doc_rejects_bad_ids(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.read_text(bad_id, "doc-1") is None
            assert store.read_text("agent-1", bad_id) is None

    def test_read_doc_range_rejects_bad_ids(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.read_text_range(bad_id, "doc-1", 1, 10) is None
            assert store.read_text_range("agent-1", bad_id, 1, 10) is None

    def test_search_doc_rejects_bad_ids(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.search_text(bad_id, "doc-1", "test") is None
            assert store.search_text("agent-1", bad_id, "test") is None

    def test_list_documents_rejects_bad_agent_id(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.list_files(bad_id) == []

    def test_remove_document_rejects_bad_ids(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.remove_file(bad_id, "doc-1") is False
            assert store.remove_file("agent-1", bad_id) is False

    def test_remove_agent_docs_rejects_bad_id(self, store: FileStore) -> None:
        for bad_id in self.MALICIOUS_IDS:
            assert store.remove_agent_files(bad_id) == 0

    def test_valid_uuid_style_ids_accepted(self, store: FileStore) -> None:
        """UUIDs (with hyphens) should work fine."""
        meta = store.add_text_file(
            "550e8400-e29b-41d4-a716-446655440000",
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "test.txt",
            "valid uuid ids",
        )
        assert meta.total_lines == 1

    def test_valid_simple_ids_accepted(self, store: FileStore) -> None:
        """Simple alphanumeric + underscore/hyphen IDs should work."""
        meta = store.add_text_file("agent_1", "doc-2", "test.txt", "ok")
        assert meta.total_lines == 1


# ══════════════════════════════════════════════════════════════════
# Integration-style tests
# ══════════════════════════════════════════════════════════════════


class TestDocReaderWorkflow:
    """End-to-end workflow: upload → read → search → read range."""

    def test_full_workflow(self, store: FileStore) -> None:
        """Simulate the LLM tool-call flow."""
        # 1. Upload a document
        menu = (
            "# Restaurant Menu\n"
            "\n"
            "## Appetizers\n"
            "Caesar Salad - $12\n"
            "Soup of the Day - $8\n"
            "Bruschetta - $10\n"
            "\n"
            "## Main Course\n"
            "Grilled Salmon - $28\n"
            "Filet Mignon - $45\n"
            "Chicken Parmesan - $22\n"
            "Vegetable Risotto - $18\n"
            "\n"
            "## Desserts\n"
            "Tiramisu - $10\n"
            "Crème Brûlée - $12\n"
            "Chocolate Fondant - $14\n"
            "\n"
            "## Hours\n"
            "Monday-Friday: 11am-10pm\n"
            "Saturday-Sunday: 10am-11pm\n"
        )
        meta = store.add_text_file("a1", "d1", "menu.md", menu)
        assert meta.total_lines == 21

        # 2. List docs
        docs = store.list_files("a1")
        assert len(docs) == 1
        assert docs[0]["filename"] == "menu.md"

        # 3. Read first N lines (entire doc fits)
        read = store.read_text("a1", "d1")
        assert read is not None
        assert read.total_lines == 21
        assert "Restaurant Menu" in read.content

        # 4. Search for a keyword
        search = store.search_text("a1", "d1", "salmon")
        assert search is not None
        assert search.total_hits == 1
        assert search.hits[0].line_number == 9

        # 5. Read a specific range based on search results
        rng = store.read_text_range("a1", "d1", 8, 12)
        assert rng is not None
        assert "Main Course" in rng.content
        assert "Salmon" in rng.content
        assert "Risotto" in rng.content

        # 6. Search for pricing
        price_search = store.search_text("a1", "d1", "$12")
        assert price_search is not None
        assert price_search.total_hits == 2  # Caesar Salad and Crème Brûlée

    def test_overwrite_doc_same_id(self, store: FileStore) -> None:
        """Re-adding a doc with the same ID replaces it."""
        store.add_text_file("a1", "d1", "v1.txt", "version 1")
        store.add_text_file("a1", "d1", "v2.txt", "version 2\nline 2")

        result = store.read_text("a1", "d1")
        assert result is not None
        assert result.total_lines == 2
        assert "version 2" in result.content

    def test_large_document_navigation(self, store: FileStore) -> None:
        """Verify navigating a document larger than the 800-line read cap."""
        store.add_text_file("a1", "big", "big.txt", LONG_TEXT)

        # First read: capped at 800
        r1 = store.read_text("a1", "big")
        assert r1 is not None
        assert r1.end_line == 800
        assert r1.total_lines == 1500

        # Continue from 801
        r2 = store.read_text_range("a1", "big", 801, 1000)
        assert r2 is not None
        assert "Line 801:" in r2.content
        assert "Line 1000:" in r2.content

        # Search works across the whole doc
        search = store.search_text("a1", "big", "Line 1234")
        assert search is not None
        assert search.total_hits == 1
        assert search.hits[0].line_number == 1234

    def test_survives_cache_eviction(self, tmp_path: object) -> None:
        """Full workflow still works after cache entries are evicted."""
        store = FileStore(base_dir=str(tmp_path), ttl=0)
        store.add_text_file("a1", "d1", "test.txt", SAMPLE_TEXT)

        # Evict everything
        store._cache["a1"]["d1"].last_accessed = time.monotonic() - 10
        store.evict_expired()

        # All operations should still work (reload from disk)
        assert store.list_files("a1")[0]["total_lines"] == 10
        assert store.read_text("a1", "d1") is not None
        assert store.read_text_range("a1", "d1", 3, 5) is not None
        assert store.search_text("a1", "d1", "testing") is not None
