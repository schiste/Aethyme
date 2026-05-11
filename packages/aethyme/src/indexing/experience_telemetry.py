"""Stable repo-local telemetry for Aethyme experience-layer lifecycle events."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .onboarding import ACT_STARTER_JSON_PATH, ONBOARDING_JSON_PATH

TELEMETRY_LOG_PATH = ".aethyme/generated/experience-telemetry.jsonl"


def append_event(repo_path: Path, event_type: str, payload: dict[str, Any]) -> Path:
    """Append one telemetry event to the repo-local ledger."""
    repo_path = Path(repo_path).expanduser().resolve()
    log_path = repo_path / TELEMETRY_LOG_PATH
    log_path.parent.mkdir(parents=True, exist_ok=True)
    event = {
        "schema_version": "aethyme-experience-telemetry-v1",
        "event_type": event_type,
        "timestamp": datetime.now(UTC).replace(microsecond=0).isoformat(),
        "repo_path": str(repo_path),
        "payload": payload,
    }
    with log_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(event) + "\n")
    return log_path


def summarize_events(repo_path: Path) -> dict[str, Any]:
    """Summarize repo-local experience telemetry."""
    repo_path = Path(repo_path).expanduser().resolve()
    log_path = repo_path / TELEMETRY_LOG_PATH
    if not log_path.exists():
        return {
            "exists": False,
            "path": TELEMETRY_LOG_PATH,
            "event_count": 0,
            "by_type": {},
            "last_event_type": None,
        }
    counts: dict[str, int] = {}
    last_event_type: str | None = None
    total = 0
    for line in log_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        total += 1
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            counts["invalid_json"] = counts.get("invalid_json", 0) + 1
            last_event_type = "invalid_json"
            continue
        event_type = str(event.get("event_type") or "unknown")
        counts[event_type] = counts.get(event_type, 0) + 1
        last_event_type = event_type
    return {
        "exists": True,
        "path": TELEMETRY_LOG_PATH,
        "event_count": total,
        "by_type": counts,
        "last_event_type": last_event_type,
    }


def detailed_report(repo_path: Path) -> dict[str, Any]:
    """Return a detailed repo-local telemetry report."""
    repo_path = Path(repo_path).expanduser().resolve()
    log_path = repo_path / TELEMETRY_LOG_PATH
    summary = summarize_events(repo_path)
    if not log_path.exists():
        return {
            **summary,
            "recent_events": [],
            "wrapper_invocations": {},
            "latest_payloads": {},
        }

    recent_events: list[dict[str, Any]] = []
    wrapper_invocations: dict[str, int] = {}
    latest_payloads: dict[str, dict[str, Any]] = {}

    for line in log_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_type = str(event.get("event_type") or "unknown")
        payload = event.get("payload") or {}
        latest_payloads[event_type] = payload
        if event_type == "wrapper.invocation":
            wrapper_name = str(payload.get("wrapper_name") or "unknown")
            wrapper_invocations[wrapper_name] = wrapper_invocations.get(wrapper_name, 0) + 1
        recent_events.append(
            {
                "timestamp": event.get("timestamp"),
                "event_type": event_type,
                "payload": payload,
            }
        )

    kpis = _derive_kpis(latest_payloads, wrapper_invocations, summary["by_type"])
    return {
        **summary,
        "recent_events": recent_events[-10:],
        "wrapper_invocations": wrapper_invocations,
        "latest_payloads": latest_payloads,
        "kpis": kpis,
    }


def event_payload_from_generated_artifacts(repo_path: Path) -> dict[str, Any]:
    """Return a compact payload from generated onboarding and Act artifacts."""
    repo_path = Path(repo_path).expanduser().resolve()
    onboarding_path = repo_path / ONBOARDING_JSON_PATH
    act_path = repo_path / ACT_STARTER_JSON_PATH
    payload: dict[str, Any] = {}
    if onboarding_path.exists():
        onboarding = json.loads(onboarding_path.read_text(encoding="utf-8"))
        payload["onboarding"] = {
            "commands": onboarding["telemetry"]["counts"]["commands"],
            "areas": onboarding["telemetry"]["counts"]["areas"],
            "entrypoints": onboarding["telemetry"]["counts"]["entrypoints"],
            "notes": onboarding["telemetry"]["counts"]["notes"],
            "overrides_applied": onboarding["telemetry"]["overrides_applied"],
            "override_invalid": onboarding["telemetry"]["override_invalid"],
        }
    if act_path.exists():
        act = json.loads(act_path.read_text(encoding="utf-8"))
        payload["act"] = {
            "has_fast_test": bool(act["commands"].get("fast_test")),
            "entrypoints": act["telemetry"]["entrypoint_count"],
            "caution_zones": act["telemetry"]["caution_zone_count"],
        }
    return payload


def record_wrapper_invocation(
    repo_path: Path,
    *,
    wrapper_name: str,
    details: dict[str, Any] | None = None,
) -> Path:
    """Record that an Aethyme-provided wrapper or hook was invoked."""
    payload: dict[str, Any] = {"wrapper_name": wrapper_name}
    if details:
        payload["details"] = details
    return append_event(repo_path, "wrapper.invocation", payload)


def _derive_kpis(
    latest_payloads: dict[str, dict[str, Any]],
    wrapper_invocations: dict[str, int],
    counts: dict[str, int],
) -> dict[str, Any]:
    onboarding = _latest_onboarding_payload(latest_payloads)
    act = _latest_act_payload(latest_payloads)
    wrapper_total = sum(wrapper_invocations.values())

    signals: list[dict[str, str | bool]] = []
    if counts.get("enhance.deploy", 0) > 0 and wrapper_total == 0:
        signals.append(
            {
                "status": "attention",
                "code": "enhanced_but_no_wrapper_usage",
                "message": "Enhancement was deployed, but no Aethyme wrapper invocation has been recorded yet.",
            }
        )
    if onboarding.get("override_invalid"):
        signals.append(
            {
                "status": "attention",
                "code": "invalid_override_present",
                "message": "Onboarding override file exists but is invalid.",
            }
        )
    if onboarding and not act.get("has_fast_test", False):
        signals.append(
            {
                "status": "attention",
                "code": "no_fast_test_detected",
                "message": "Onboarding/Act artifacts exist but no fast test command was detected.",
            }
        )
    if onboarding.get("overrides_applied"):
        signals.append(
            {
                "status": "info",
                "code": "maintainer_overrides_active",
                "message": "Maintainer onboarding overrides are active for this repository.",
            }
        )

    return {
        "wrapper_total": wrapper_total,
        "onboarding_commands": onboarding.get("commands", 0),
        "onboarding_notes": onboarding.get("notes", 0),
        "act_has_fast_test": act.get("has_fast_test", False),
        "signals": signals,
        "suggestions": _suggestions_from_signals(signals),
    }


def _latest_onboarding_payload(latest_payloads: dict[str, dict[str, Any]]) -> dict[str, Any]:
    for event_type in ("enhance.verify", "enhance.deploy", "repo.compile-skills"):
        payload = latest_payloads.get(event_type) or {}
        onboarding = payload.get("onboarding")
        if isinstance(onboarding, dict):
            return onboarding
    return {}


def _latest_act_payload(latest_payloads: dict[str, dict[str, Any]]) -> dict[str, Any]:
    for event_type in ("enhance.verify", "enhance.deploy", "repo.compile-skills"):
        payload = latest_payloads.get(event_type) or {}
        act = payload.get("act")
        if isinstance(act, dict):
            return act
    return {}


def _suggestions_from_signals(
    signals: list[dict[str, str | bool]],
) -> list[dict[str, str]]:
    suggestions: list[dict[str, str]] = []
    codes = {str(signal["code"]) for signal in signals}
    if "enhanced_but_no_wrapper_usage" in codes:
        suggestions.append(
            {
                "code": "load_onboarding_and_use_wrapper",
                "message": "Load `repo-onboarding` first and invoke `.codex/skills/aethyme/aethyme-explore` or the equivalent Aethyme wrapper for the next broad task.",
            }
        )
    if "invalid_override_present" in codes:
        suggestions.append(
            {
                "code": "fix_or_reinitialize_override",
                "message": "Run `aethyme repo validate-onboarding-overrides <repo>` to inspect errors, then fix the JSON or rerun `aethyme repo init-onboarding-overrides <repo> --force`.",
            }
        )
    if "no_fast_test_detected" in codes:
        suggestions.append(
            {
                "code": "add_fast_test_override",
                "message": "Add a maintainer override for `commands[].test` in `.aethyme/overrides/onboarding.json`, then rerun `aethyme repo compile-skills <repo>` or `aethyme enhance deploy --repo <repo>`.",
            }
        )
    if "maintainer_overrides_active" in codes:
        suggestions.append(
            {
                "code": "review_override_drift",
                "message": "Review whether the override notes and commands are still current after recent repo changes and regenerate onboarding if needed.",
            }
        )
    return suggestions
