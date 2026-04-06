"""Skill tools — search and load integration skills for the builder LLM.

Replaces the old keyword-matching approach with LLM-driven tool calls.
The LLM decides when it needs skill info and calls these tools directly.

Two tools:
  - search_skills(query) — fuzzy search against name + description
  - load_skill(name) — load the full SKILL.md instructions

Skills are auto-discovered from the skills/ directory by scanning
SKILL.md frontmatter. No separate registry file needed.
"""

import re
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any

import yaml
from loguru import logger
from pydantic_ai import Agent, RunContext

from app.lib import PROJECT_ROOT

_SKILLS_DIR = PROJECT_ROOT / "skills"
MAX_SEARCH_RESULTS = 5

# Only allow simple alphanumeric + underscore/hyphen skill names.
# Rejects path traversal (../, /, \) and any other shenanigans.
_SAFE_NAME_RE = re.compile(r"^[a-zA-Z0-9_-]+$")


# ═══════════════════════════════════════════════════════════════════
# Skill Index — auto-discovered from SKILL.md frontmatter
# ═══════════════════════════════════════════════════════════════════


@lru_cache(maxsize=64)
def _load_skill_body(skills_dir: str, name: str) -> str | None:
    """Load and cache a SKILL.md body (without frontmatter)."""
    if not _SAFE_NAME_RE.match(name):
        logger.warning("Rejected unsafe skill name: {!r}", name)
        return None

    skill_md = Path(skills_dir) / name / "SKILL.md"
    if not skill_md.exists():
        return None

    content = skill_md.read_text(encoding="utf-8")
    if content.startswith("---"):
        parts = content.split("---", 2)
        if len(parts) >= 3:
            return parts[2].strip()
    return content


@dataclass
class SkillEntry:
    """A skill's metadata parsed from its SKILL.md frontmatter."""

    name: str
    display_name: str
    description: str
    auth_type: str
    category: str


class SkillIndex:
    """Auto-discovered, searchable index of available skills.

    Scans the skills/ directory at init time, parses SKILL.md frontmatter
    for each skill, and builds an in-memory index for search.
    """

    def __init__(self, skills_dir: Path | None = None) -> None:
        self._dir = skills_dir or _SKILLS_DIR
        self._entries: list[SkillEntry] = []
        self._refresh()

    def _refresh(self) -> None:
        """Scan skills/ directory for SKILL.md files and parse frontmatter."""
        self._entries = []
        if not self._dir.exists():
            logger.warning("Skills directory not found: {}", self._dir)
            return

        for child in sorted(self._dir.iterdir()):
            if not child.is_dir() or child.name.startswith("_"):
                continue
            skill_md = child / "SKILL.md"
            if not skill_md.exists():
                continue

            try:
                content = skill_md.read_text(encoding="utf-8")
                if not content.startswith("---"):
                    continue
                parts = content.split("---", 2)
                if len(parts) < 3:
                    continue
                fm: dict[str, Any] = yaml.safe_load(parts[1]) or {}
                self._entries.append(
                    SkillEntry(
                        name=fm.get("name", child.name),
                        display_name=fm.get("display_name", child.name),
                        description=fm.get("description", ""),
                        auth_type=fm.get("auth_type", "none"),
                        category=fm.get("category", "general"),
                    )
                )
            except Exception:
                logger.warning("Failed to parse skill frontmatter: {}", skill_md)

        logger.info("Discovered {} skills from {}", len(self._entries), self._dir)

    def search(self, query: str) -> tuple[list[SkillEntry], int]:
        """Search skills by query words against name and description.

        Returns (results capped at MAX_SEARCH_RESULTS, total_matches).
        """
        if not query.strip():
            return self._entries[:MAX_SEARCH_RESULTS], len(self._entries)

        query_words = query.lower().split()
        scored: list[tuple[int, SkillEntry]] = []

        for entry in self._entries:
            searchable = (
                f"{entry.name} {entry.display_name} "
                f"{entry.description} {entry.category}"
            ).lower()
            score = sum(1 for word in query_words if word in searchable)
            if score > 0:
                scored.append((score, entry))

        scored.sort(key=lambda x: x[0], reverse=True)
        total = len(scored)
        results = [entry for _, entry in scored[:MAX_SEARCH_RESULTS]]
        return results, total

    def load(self, name: str) -> str | None:
        """Load the full SKILL.md body (without frontmatter) for a skill."""
        return _load_skill_body(str(self._dir), name)

    @property
    def count(self) -> int:
        return len(self._entries)

    @property
    def categories(self) -> list[str]:
        return sorted({e.category for e in self._entries})

    def summary_line(self) -> str:
        """Skill registry summary injected into the builder system prompt.

        Lists every registered skill name so the LLM knows exactly which
        names to use in secret() calls and action_cards.
        """
        if not self._entries:
            return "No skills registered."
        lines = [
            f"{self.count} registered skills (use these exact names in secret() and action_cards):"
        ]
        for e in self._entries:
            lines.append(
                f"  - `{e.name}` ({e.display_name}, auth: {e.auth_type}): {e.description}"
            )
        return "\n".join(lines)


# Module-level singleton
skill_index = SkillIndex()


# ═══════════════════════════════════════════════════════════════════
# Tool Registration
# ═══════════════════════════════════════════════════════════════════


def register_skill_tools(agent: Agent[Any, Any]) -> None:
    """Register search_skills and load_skill tools on the builder agent."""

    @agent.tool
    async def search_skills(ctx: RunContext[Any], query: str) -> str:
        """Search available integration skills by keyword.

        Use this when the user mentions a third-party service, API,
        or integration (e.g. "Airtable", "calendar", "CRM", "Slack").
        Returns matching skill names and descriptions.
        Narrow your query if results are truncated.
        """
        results, total = skill_index.search(query)

        if not results:
            return f"No skills found matching '{query}'. Try different keywords."

        lines: list[str] = []
        for entry in results:
            lines.append(
                f"- **{entry.display_name}** (`{entry.name}`): "
                f"{entry.description} [auth: {entry.auth_type}]"
            )

        output = "\n".join(lines)

        if total > MAX_SEARCH_RESULTS:
            output += (
                f"\n\n({total - MAX_SEARCH_RESULTS} more results not shown. "
                f"Narrow your search to see more specific results.)"
            )

        return output

    @agent.tool
    async def load_skill(ctx: RunContext[Any], name: str) -> str:
        """Load the full instructions for a specific integration skill.

        Call this AFTER search_skills to get detailed instructions on
        how to generate tool configs for that integration. Use the exact
        skill name from search results (e.g. "airtable", "google_calendar").
        """
        content = skill_index.load(name)
        if content is None:
            available = [e.name for e in skill_index._entries]
            return f"Skill '{name}' not found. Available skills: {', '.join(available)}"

        logger.info("Builder loaded skill '{}' via tool call", name)
        return content
