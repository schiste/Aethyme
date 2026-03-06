"""Render task-context packs for local operator workflows."""

from __future__ import annotations

from typing import Any


def render_pack_summary(pack: dict[str, Any]) -> str:
    """Render a concise text summary of a task-context pack."""
    lines = [f"Task: {pack['task']['raw']}"]
    anchors = pack.get("anchors", [])
    if anchors:
        lines.append("Anchors:")
        for anchor in anchors:
            lines.append(f"- {anchor['id']} ({anchor['reason']})")
    navigation = pack.get("navigation_order", [])
    if navigation:
        lines.append("Navigation order:")
        for item in navigation:
            lines.append(f"- {item}")
    risks = pack.get("risk_flags", [])
    if risks:
        lines.append("High-risk areas:")
        for risk in risks:
            lines.append(f"- {risk['scope']} ({risk['area']}): {risk['reason']}")
    return "\n".join(lines)


def render_prompt_pack(pack: dict[str, Any]) -> str:
    """Render a compact prompt-oriented view of a task-context pack."""
    lines = [f"Task: {pack['task']['raw']}"]

    anchors = pack.get("anchors", [])
    if anchors:
        lines.append("Start here:")
        for anchor in anchors[:3]:
            file_hint = f" @ {anchor['file']}" if anchor.get("file") else ""
            lines.append(f"- {anchor['id']}{file_hint}: {anchor['reason']}")

    in_scope = pack.get("in_scope", {})
    in_scope_files = in_scope.get("files", [])
    if in_scope_files:
        lines.append("In scope files:")
        for item in in_scope_files[:5]:
            lines.append(f"- {item['value']}")

    dependencies = pack.get("dependencies", [])
    if dependencies:
        lines.append("Relevant dependencies:")
        for dependency in dependencies[:5]:
            lines.append(f"- {dependency['from']} -> {dependency['to']}")

    snippets = pack.get("snippets", [])
    if snippets:
        lines.append("Read these snippets:")
        for snippet in snippets[:5]:
            lines.append(
                f"- {snippet['file']}:{snippet['start_line']}-{snippet['end_line']} ({snippet['kind']})"
            )

    out_of_scope = pack.get("out_of_scope", {})
    out_of_scope_areas = out_of_scope.get("areas", [])
    if out_of_scope_areas:
        lines.append("Avoid unless needed:")
        for item in out_of_scope_areas[:5]:
            lines.append(f"- {item['value']}: {item['reason']}")

    risks = pack.get("risk_flags", [])
    if risks:
        lines.append("High-risk areas:")
        for risk in risks[:5]:
            lines.append(f"- {risk['scope']} ({risk['area']})")

    navigation = pack.get("navigation_order", [])
    if navigation:
        lines.append("Navigation order:")
        for item in navigation[:5]:
            lines.append(f"- {item}")

    return "\n".join(lines)
