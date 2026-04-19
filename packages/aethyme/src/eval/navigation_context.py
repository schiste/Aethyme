"""Shared navigation-context shaping helpers.

Keeps agent-facing payloads aligned with engine-native structures so we don't
reintroduce drift between evaluation contexts and deterministic engine outputs.
"""

from __future__ import annotations

from typing import Any


def _extract_values(items: list[Any]) -> list[str]:
    values: list[str] = []
    for item in items:
        if isinstance(item, dict):
            value = item.get("value")
            if isinstance(value, str):
                values.append(value)
        elif isinstance(item, str):
            values.append(item)
    return values


def build_scope_view(task: str, task_pack: dict[str, Any]) -> dict[str, Any]:
    """Build a scope view that includes both simple and detailed structures."""
    in_scope = task_pack.get("in_scope", {})
    out_of_scope = task_pack.get("out_of_scope", {})

    return {
        "task": task,
        "navigation_order": task_pack.get("navigation_order", []),
        # Flattened views for backward compatibility in prompts/tool consumers.
        "in_scope_files": _extract_values(in_scope.get("files", [])),
        "in_scope_symbols": _extract_values(in_scope.get("symbols", [])),
        "in_scope_areas": _extract_values(in_scope.get("areas", [])),
        "out_of_scope": _extract_values(out_of_scope.get("areas", [])),
        "risks": [risk["scope"] for risk in task_pack.get("risk_flags", [])],
        # Detailed engine-native structures for reason/cap/confidence visibility.
        "in_scope_files_detailed": in_scope.get("files", []),
        "in_scope_symbols_detailed": in_scope.get("symbols", []),
        "in_scope_areas_detailed": in_scope.get("areas", []),
        "out_of_scope_detailed": out_of_scope.get("areas", []),
        "risk_flags": task_pack.get("risk_flags", []),
        "confidence": task_pack.get("confidence", {}),
        "budget": task_pack.get("budget", {}),
    }
