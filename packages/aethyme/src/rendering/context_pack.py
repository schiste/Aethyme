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

    confidence = pack.get("confidence")
    if isinstance(confidence, dict):
        anchor_conf = confidence.get("anchor_confidence")
        scope_conf = confidence.get("scope_confidence")
        if anchor_conf is not None or scope_conf is not None:
            lines.append(
                "Confidence:"
                f" anchor={anchor_conf if anchor_conf is not None else 'n/a'}"
                f", scope={scope_conf if scope_conf is not None else 'n/a'}"
            )

    budget = pack.get("budget")
    if isinstance(budget, dict):
        budget_fields = [
            "max_anchors",
            "max_files",
            "max_snippets",
            "dependency_depth",
            "impact_depth",
            "content_budget",
            "max_content_files",
            "max_lines_per_file",
        ]
        rendered = [f"{field}={budget[field]}" for field in budget_fields if field in budget]
        if rendered:
            lines.append("Caps: " + ", ".join(rendered))

    capped_files: list[str] = []
    for file_content in pack.get("file_contents", []):
        if not isinstance(file_content, dict):
            continue
        end_line = file_content.get("end_line")
        total_lines = file_content.get("total_lines")
        path = file_content.get("path", "<unknown>")
        if (
            isinstance(end_line, int)
            and isinstance(total_lines, int)
            and total_lines > end_line
        ):
            capped_files.append(f"{path} ({end_line}/{total_lines} lines)")
    if capped_files:
        lines.append("Truncated content:")
        for item in capped_files[:5]:
            lines.append(f"- {item}")
    return "\n".join(lines)


def render_prompt_pack(pack: dict[str, Any]) -> str:
    """Render a compact prompt-oriented view of a task-context pack."""
    task = pack.get("task", {})
    if task.get("kind") == "explain_repo":
        overview = pack.get("overview", {})
        items: list[str] = []
        items.extend(overview.get("overview_docs", [])[:2])
        items.extend(overview.get("code_areas", [])[:2])
        items.extend(overview.get("reference_areas", [])[:1])
        items.extend(overview.get("subareas", [])[:1])
        items.extend(overview.get("key_configs", [])[:1])
        items.extend(overview.get("entrypoints", [])[:1])
        items = [item for index, item in enumerate(items) if item and item not in items[:index]]
        navigation = pack.get("navigation_order", [])
        lines = []
        if items:
            lines.append("Overview: " + " | ".join(items[:5]))
        if navigation:
            lines.append("Order: " + " -> ".join(navigation[:4]))
        return "\n".join(lines)

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
    scope_items = [
        item.get("value")
        for item in in_scope_files[:3]
        if isinstance(item, dict) and isinstance(item.get("value"), str)
    ] + [
        item.get("value")
        for item in in_scope_areas[:3]
        if isinstance(item, dict) and isinstance(item.get("value"), str)
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
        avoid_items = [
            item.get("value")
            for item in out_of_scope_areas[:3]
            if isinstance(item, dict) and isinstance(item.get("value"), str)
        ]
        if avoid_items:
            lines.append("Avoid: " + " | ".join(avoid_items))

    risks = pack.get("risk_flags", [])
    if risks:
        lines.append("Risk: " + " | ".join([risk["scope"] for risk in risks[:3]]))

    navigation = pack.get("navigation_order", [])
    if navigation:
        lines.append("Order: " + " -> ".join(navigation[:4]))

    return "\n".join(lines)


def render_explain_repo_text(inspect: dict[str, Any], pack: dict[str, Any]) -> str:
    """Render a deterministic repo explanation from inspect + pack data."""
    snapshot = inspect.get("snapshot", {})
    overview = pack.get("overview", {})
    languages = ", ".join(snapshot.get("languages", [])) or "unknown"
    top_level_dirs = ", ".join(snapshot.get("top_level_dirs", []))
    files_count = inspect.get("files_count", len(inspect.get("files", [])))
    functions_count = inspect.get("functions_count", len(inspect.get("functions", [])))
    classes_count = inspect.get("classes_count", len(inspect.get("classes", [])))
    docs_count = inspect.get("docs_count", len(inspect.get("docs", [])))
    configs_count = inspect.get("configs_count", len(inspect.get("configs", [])))
    lines = [
        f"Task: {pack.get('task', {}).get('raw', 'Explain this repo')}",
        f"Languages: {languages}",
        f"Top-level directories: {top_level_dirs}",
        f"Files indexed: {files_count}",
        f"Functions indexed: {functions_count}",
        f"Classes indexed: {classes_count}",
        f"Docs indexed: {docs_count}",
        f"Configs indexed: {configs_count}",
    ]

    overview_docs = overview.get("overview_docs", [])
    if overview_docs:
        lines.append(f"README: {overview_docs[0]}")

    code_areas = overview.get("code_areas", [])
    if code_areas:
        lines.append("")
        lines.append("Code areas:")
        for area in code_areas[:3]:
            lines.append(f"- {area}")

    reference_areas = overview.get("reference_areas", [])
    if reference_areas:
        lines.append("")
        lines.append("Reference areas:")
        for area in reference_areas[:3]:
            lines.append(f"- {area}")

    subareas = overview.get("subareas", [])
    if subareas:
        lines.append("")
        lines.append("Key subareas:")
        for area in subareas[:4]:
            lines.append(f"- {area}")

    key_configs = overview.get("key_configs", [])
    if key_configs:
        lines.append("")
        lines.append("Key configs:")
        for config in key_configs[:3]:
            lines.append(f"- {config}")

    entrypoints = overview.get("entrypoints", [])
    if entrypoints:
        lines.append("")
        lines.append("Entrypoints:")
        for entrypoint in entrypoints[:3]:
            lines.append(f"- {entrypoint}")

    representative_code_files = overview.get("representative_code_files", [])
    if representative_code_files:
        lines.append("")
        lines.append("Representative code:")
        for path in representative_code_files[:3]:
            lines.append(f"- {path}")

    representative_docs = overview.get("representative_docs", [])
    if representative_docs:
        lines.append("")
        lines.append("Representative docs:")
        for path in representative_docs[:3]:
            lines.append(f"- {path}")

    navigation = pack.get("navigation_order", [])
    if navigation:
        lines.append("")
        lines.append("Navigation order:")
        for item in navigation[:5]:
            lines.append(f"- {item}")

    return "\n".join(lines)
