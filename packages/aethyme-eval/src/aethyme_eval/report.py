"""Human and machine report rendering."""

from __future__ import annotations

import json
from typing import Any


def render_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def render_markdown(payload: dict[str, Any]) -> str:
    baseline = payload.get("baseline") or {}
    summary = payload.get("summary") or {}
    lines = [
        "# Aethyme Eval Regression Report",
        "",
        f"- Source: `{payload.get('source') or '-'}`",
        f"- Baseline: `{baseline.get('name') or '-'}`",
        f"- Methodology hash: `{baseline.get('methodology_hash') or '-'}`",
        (
            f"- Summary: {summary.get('fail', 0)} fail, "
            f"{summary.get('warn', 0)} warn, {summary.get('pass', 0)} pass"
        ),
        "",
        "| Status | Model | Target | Eval | Scenario | Condition | Tokens | Cost | Duration | Quality Δ | Notes |",
        "|---|---|---|---|---|---:|---:|---:|---:|---:|---|",
    ]
    for row in payload.get("rows", []):
        key = row["key"]
        notes = "; ".join(row.get("reasons") or [])
        lines.append(
            "| {status} | `{model}` | `{target}` | `{eval_type}` | `{scenario}` | `{condition}` | "
            "{tokens} | {cost} | {duration} | {quality} | {notes} |".format(
                status=row["status"],
                model=key["model"],
                target=key["target"],
                eval_type=key["eval_type"],
                scenario=key.get("scenario") or "-",
                condition=key["condition"],
                tokens=_ratio(row.get("adjusted_token_ratio"), row.get("token_ratio")),
                cost=_number(row.get("cost_ratio")),
                duration=_number(row.get("duration_ratio")),
                quality=_number(row.get("quality_delta")),
                notes=notes or "-",
            )
        )
    lines.append("")
    return "\n".join(lines)


def _number(value: float | None) -> str:
    if value is None:
        return "-"
    return f"{value:.2f}"


def _ratio(adjusted: float | None, raw: float | None) -> str:
    if raw is None:
        return "-"
    if adjusted is not None and adjusted != raw:
        return f"{adjusted:.2f} adj / {raw:.2f} raw"
    return f"{raw:.2f}"
