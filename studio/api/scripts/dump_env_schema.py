"""Generate .env.example from the Pydantic Settings schema.

Usage:
    python -m scripts.dump_env_schema          # prints to stdout
    python -m scripts.dump_env_schema > .env.example  # write to file
"""

from __future__ import annotations

import json
import sys
from typing import Any, get_args, get_origin

from pydantic import BaseModel
from pydantic.fields import FieldInfo

from app.lib.config import Settings


def _python_value_to_env(value: Any) -> str:
    """Convert a Python default value to .env-file string representation."""
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, list):
        return json.dumps(value)
    if isinstance(value, str):
        return value
    return str(value)


def _format_type_hint(annotation: Any) -> str:
    """Return a human-friendly type string for documentation."""
    origin = get_origin(annotation)
    if origin is list:
        args = get_args(annotation)
        inner = args[0].__name__ if args else "str"
        return f"list[{inner}]"
    if hasattr(annotation, "__name__"):
        return annotation.__name__
    return str(annotation)


def _is_nested_model(field_info: FieldInfo) -> bool:
    """Check if a field's annotation is a BaseModel subclass."""
    annotation = field_info.annotation
    try:
        return isinstance(annotation, type) and issubclass(annotation, BaseModel)
    except TypeError:
        return False


def dump_env_schema() -> str:
    """Walk the Settings schema and produce a documented .env.example."""
    lines: list[str] = [
        "# ══════════════════════════════════════════════════════════════",
        "# Studio API — Environment Variables",
        "# ══════════════════════════════════════════════════════════════",
        "# Auto-generated from app/lib/config.py — do not edit by hand.",
        "# Re-generate: make env-schema",
        "#",
        "# Copy this file to .env and customise.",
        "# Precedence: .env → .env.local → .env.{{env}} → .env.{{env}}.local",
        "",
    ]

    settings_fields = Settings.model_fields

    # ── Top-level (non-nested) fields ────────────────────────
    top_fields: list[tuple[str, FieldInfo]] = []
    nested_fields: list[tuple[str, FieldInfo]] = []

    for name, field_info in settings_fields.items():
        if name == "model_config":
            continue
        if _is_nested_model(field_info):
            nested_fields.append((name, field_info))
        else:
            top_fields.append((name, field_info))

    if top_fields:
        lines.append("# ── Application ──────────────────────────────────────────────")
        for name, field_info in top_fields:
            env_name = name.upper()
            default = field_info.default
            if (
                default is not None
                and str(default) == "PydanticUndefined"
                and field_info.default_factory
            ):
                default = field_info.default_factory()
            type_hint = _format_type_hint(field_info.annotation)

            # Description from docstring or field description
            desc = field_info.description or ""
            if desc:
                lines.append(f"# {desc}")
            lines.append(f"# Type: {type_hint}")

            if default is not None and default is not ...:
                lines.append(f"{env_name}={_python_value_to_env(default)}")
            else:
                lines.append(f"# {env_name}=")
            lines.append("")

    # ── Nested model fields ──────────────────────────────────
    delimiter = "__"
    for parent_name, parent_field in nested_fields:
        model_cls = parent_field.annotation
        assert isinstance(model_cls, type) and issubclass(model_cls, BaseModel)

        # Section header from class docstring (first line)
        doc_lines = [
            line.strip() for line in (model_cls.__doc__ or "").strip().splitlines()
        ]
        header = doc_lines[0] if doc_lines else parent_name.title()
        lines.append(f"# ── {header} ─────")

        # Emit remaining docstring lines as comments (precedence, notes, etc.)
        for doc_line in doc_lines[1:]:
            if doc_line:
                lines.append(f"# {doc_line}")
            else:
                lines.append("#")

        for child_name, child_info in model_cls.model_fields.items():
            env_name = f"{parent_name.upper()}{delimiter}{child_name.upper()}"
            default = child_info.default
            type_hint = _format_type_hint(child_info.annotation)

            desc = child_info.description or ""
            if desc:
                lines.append(f"# {desc}")
            lines.append(f"# Type: {type_hint}")

            if default is not None and default is not ...:
                env_val = _python_value_to_env(default)
                # Comment out empty strings and secrets
                if (
                    env_val == ""
                    or "secret" in child_name
                    or "token" in child_name
                    or "key" in child_name
                ):
                    lines.append(f"# {env_name}={env_val}")
                else:
                    lines.append(f"{env_name}={env_val}")
            else:
                lines.append(f"# {env_name}=")
            lines.append("")

    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.stdout.write(dump_env_schema())
