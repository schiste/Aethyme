"""Repo-enhancement helpers that remain Python until Phase 3.

The `enhance deploy`/`enhance verify` implementation moved to the
`aethyme-enhance` Rust crate (python-retirement Phase 2 flip,
2026-07-29): the deploy pipeline, template/AGENTS.md rendering,
settings-hook merging, and legacy-migration helpers now live there,
with templates embedded at build time. What remains here are the
agents-override utilities and summary/status helpers still consumed by
the Python `repo` group (onboarding/telemetry UX — ports in Phase 3).
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from src.indexing.experience_telemetry import (
    summarize_events,
    write_status_artifacts,
)
from src.indexing.onboarding import (
    ACT_STARTER_JSON_PATH,
    ONBOARDING_JSON_PATH,
    expected_onboarding_files,
    override_freshness,
    recommendation_summary,
)

AGENTS_OVERRIDE_PATH = ".aethyme/overrides/agents.json"


def _load_agents_overrides(repo: Path) -> dict[str, Any]:
    override_path = repo / AGENTS_OVERRIDE_PATH
    if not override_path.exists():
        return {}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"_invalid_override": True, "_source": AGENTS_OVERRIDE_PATH}
    if not isinstance(payload, dict):
        return {"_invalid_override": True, "_source": AGENTS_OVERRIDE_PATH}
    payload["_source"] = AGENTS_OVERRIDE_PATH
    return payload


def agents_override_template() -> dict[str, Any]:
    return {
        "repo_summary": "One-paragraph repo-specific summary for agents.",
        "hard_constraints": [
            "Never bypass tenant isolation or authorization checks."
        ],
        "validation_rules": [
            "Run the smallest relevant test set before broader suites."
        ],
        "commit_hygiene_notes": [
            "Document domain invariants in the Memory section for substantive commits."
        ],
        "summon_policy_notes": [
            "Load repo-onboarding first for broad or unfamiliar tasks."
        ],
        "maintainer_markdown": "## Domain Notes\n\nAdd compact repo-specific guidance here.",
    }


def validate_agents_overrides(repo: Path) -> dict[str, Any]:
    repo = Path(repo).expanduser().resolve()
    override_path = repo / AGENTS_OVERRIDE_PATH
    if not override_path.exists():
        return {"ok": True, "exists": False, "path": AGENTS_OVERRIDE_PATH, "errors": []}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return {
            "ok": False,
            "exists": True,
            "path": AGENTS_OVERRIDE_PATH,
            "errors": [f"invalid JSON: {exc.msg}"],
        }
    if not isinstance(payload, dict):
        return {
            "ok": False,
            "exists": True,
            "path": AGENTS_OVERRIDE_PATH,
            "errors": ["override root must be a JSON object"],
        }
    errors: list[str] = []
    if "repo_summary" in payload and not isinstance(payload["repo_summary"], str):
        errors.append("repo_summary must be a string")
    for key in (
        "hard_constraints",
        "validation_rules",
        "commit_hygiene_notes",
        "summon_policy_notes",
    ):
        value = payload.get(key)
        if value is not None and not (
            isinstance(value, list) and all(isinstance(item, str) for item in value)
        ):
            errors.append(f"{key} must be a list of strings")
    if "maintainer_markdown" in payload and not isinstance(
        payload["maintainer_markdown"], str
    ):
        errors.append("maintainer_markdown must be a string")
    return {
        "ok": not errors,
        "exists": True,
        "path": AGENTS_OVERRIDE_PATH,
        "errors": errors,
    }


def summarize(repo: Path) -> dict[str, Any]:
    """Return a small enhancement summary with recommendations."""
    files = expected_onboarding_files(repo)
    onboarding = json.loads(files[ONBOARDING_JSON_PATH])
    act = json.loads(files[ACT_STARTER_JSON_PATH])
    recommendation = recommendation_summary(repo)
    return {
        "recommended_skill": recommendation["recommended_skill"],
        "recommended_mode": recommendation["recommended_mode"],
        "reason": recommendation["reason"],
        "onboarding": {
            "commands": onboarding["telemetry"]["counts"]["commands"],
            "areas": onboarding["telemetry"]["counts"]["areas"],
            "entrypoints": onboarding["telemetry"]["counts"]["entrypoints"],
            "notes": onboarding["telemetry"]["counts"]["notes"],
            "overrides_applied": onboarding["telemetry"]["overrides_applied"],
            "override_invalid": onboarding["telemetry"]["override_invalid"],
        },
        "act": {
            "has_fast_test": bool(act["commands"].get("fast_test")),
            "entrypoints": act["telemetry"]["entrypoint_count"],
            "caution_zones": act["telemetry"]["caution_zone_count"],
        },
        "freshness": override_freshness(repo),
        "experience_telemetry": summarize_events(repo),
    }


def refresh_status(repo: Path) -> dict[str, Any]:
    """Refresh repo-local experience status artifacts."""
    return write_status_artifacts(repo)
