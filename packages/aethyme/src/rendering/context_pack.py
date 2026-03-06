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
    lines: list[str] = []

    anchors = pack.get("anchors", [])
    if anchors:
        lines.append(
            "Start: "
            + " | ".join(
                [
                    (
                        f"{anchor['id']}@{anchor['file']}"
                        if anchor.get("file") and anchor["file"] != anchor["id"]
                        else anchor["id"]
                    )
                    for anchor in anchors[:4]
                ]
            )
        )

    in_scope = pack.get("in_scope", {})
    in_scope_files = in_scope.get("files", [])
    in_scope_areas = in_scope.get("areas", [])
    scope_items = [item["value"] for item in in_scope_files[:3]] + [
        item["value"] for item in in_scope_areas[:3]
    ]
    if scope_items:
        lines.append("Scope: " + " | ".join(scope_items))

    if in_scope_files:
        lines.append(
            "Read: "
            + " | ".join(
                [
                    f"{snippet['file']}:{snippet['start_line']}-{snippet['end_line']}"
                    for snippet in pack.get("snippets", [])[:3]
                ]
            )
        )

    dependencies = pack.get("dependencies", [])
    if dependencies:
        lines.append(
            "Deps: "
            + " | ".join(
                [f"{dependency['from']}->{dependency['to']}" for dependency in dependencies[:3]]
            )
        )

    out_of_scope = pack.get("out_of_scope", {})
    out_of_scope_areas = out_of_scope.get("areas", [])
    if out_of_scope_areas:
        lines.append("Avoid: " + " | ".join([item["value"] for item in out_of_scope_areas[:3]]))

    risks = pack.get("risk_flags", [])
    if risks:
        lines.append("Risk: " + " | ".join([risk["scope"] for risk in risks[:3]]))

    navigation = pack.get("navigation_order", [])
    if navigation:
        lines.append("Order: " + " -> ".join(navigation[:4]))

    return "\n".join(lines)
