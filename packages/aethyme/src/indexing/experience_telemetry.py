"""Stable repo-local telemetry for Aethyme experience-layer lifecycle events."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .onboarding import ACT_STARTER_JSON_PATH, ONBOARDING_JSON_PATH, override_freshness

TELEMETRY_LOG_PATH = ".aethyme/generated/experience-telemetry.jsonl"
STATUS_JSON_PATH = ".aethyme/generated/experience-status.json"
STATUS_MARKDOWN_PATH = ".aethyme/generated/experience-status.md"


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
    freshness = override_freshness(repo_path)
    if not log_path.exists():
        return {
            **summary,
            "recent_events": [],
            "wrapper_invocations": {},
            "latest_payloads": {},
            "freshness": freshness,
            "kpis": _derive_kpis({}, {}, {}, freshness),
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

    kpis = _derive_kpis(latest_payloads, wrapper_invocations, summary["by_type"], freshness)
    return {
        **summary,
        "recent_events": recent_events[-10:],
        "wrapper_invocations": wrapper_invocations,
        "latest_payloads": latest_payloads,
        "freshness": freshness,
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


def build_status_artifact(repo_path: Path) -> dict[str, Any]:
    """Build a compact repo experience status artifact."""
    report = detailed_report(repo_path)
    recommendation = _recommended_next_action(report)
    return {
        "schema_version": "aethyme-experience-status-v1",
        "path": {
            "telemetry": report["path"],
            "status_json": STATUS_JSON_PATH,
            "status_markdown": STATUS_MARKDOWN_PATH,
        },
        "enhancement": {
            "installed": report["by_type"].get("enhance.deploy", 0) > 0,
            "verified": report["by_type"].get("enhance.verify", 0) > 0,
        },
        "artifacts": {
            "onboarding_present": _latest_onboarding_payload(report.get("latest_payloads", {})) != {},
            "act_present": _latest_act_payload(report.get("latest_payloads", {})) != {},
            "override_exists": bool(report.get("freshness", {}).get("override_exists")),
            "override_regeneration_required": bool(
                report.get("freshness", {}).get("regeneration_required")
            ),
            "stale_targets": list(report.get("freshness", {}).get("stale_targets") or []),
        },
        "kpis": report.get("kpis", {}),
        "signals": report.get("kpis", {}).get("signals", []),
        "suggestions": report.get("kpis", {}).get("suggestions", []),
        "wrapper_invocations": report.get("wrapper_invocations", {}),
        "recent_events": report.get("recent_events", [])[-5:],
        "recommended_next_action": recommendation,
    }


def render_status_markdown(status: dict[str, Any]) -> str:
    """Render the experience status artifact as compact Markdown."""
    enhancement = status["enhancement"]
    artifacts = status["artifacts"]
    kpis = status["kpis"]
    signals = status["signals"]
    suggestions = status["suggestions"]
    recommendation = status["recommended_next_action"]

    lines = [
        "# Aethyme Experience Status",
        "",
        "## Summary",
        "",
        f"- Enhancement installed: `{'yes' if enhancement['installed'] else 'no'}`",
        f"- Enhancement verified: `{'yes' if enhancement['verified'] else 'no'}`",
        f"- Onboarding present: `{'yes' if artifacts['onboarding_present'] else 'no'}`",
        f"- Act starter present: `{'yes' if artifacts['act_present'] else 'no'}`",
        f"- Override exists: `{'yes' if artifacts['override_exists'] else 'no'}`",
        f"- Override regeneration required: `{'yes' if artifacts['override_regeneration_required'] else 'no'}`",
        f"- Wrapper invocations: `{kpis.get('wrapper_total', 0)}`",
        f"- Fast test detected: `{'yes' if kpis.get('act_has_fast_test') else 'no'}`",
        "",
        "## Attention Signals",
        "",
    ]
    if signals:
        for signal in signals:
            lines.append(f"- `{signal['status']}` `{signal['code']}`: {signal['message']}")
    else:
        lines.append("- none")

    lines.extend(["", "## Suggestions", ""])
    if suggestions:
        for suggestion in suggestions:
            lines.append(f"- `{suggestion['code']}`: {suggestion['message']}")
    else:
        lines.append("- none")

    lines.extend(
        [
            "",
            "## Recommended Next Action",
            "",
            f"- Command: `{recommendation['command']}`",
            f"- Reason: {recommendation['reason']}",
        ]
    )
    if artifacts["stale_targets"]:
        lines.append(f"- Stale targets: `{', '.join(artifacts['stale_targets'])}`")

    recent_events = status.get("recent_events") or []
    if recent_events:
        lines.extend(["", "## Recent Events", ""])
        for event in recent_events:
            lines.append(f"- `{event['timestamp']}` `{event['event_type']}`")

    return "\n".join(lines) + "\n"


def write_status_artifacts(repo_path: Path) -> dict[str, Any]:
    """Write repo-local experience status artifacts and return the status payload."""
    repo_path = Path(repo_path).expanduser().resolve()
    status = build_status_artifact(repo_path)
    status_json_path = repo_path / STATUS_JSON_PATH
    status_markdown_path = repo_path / STATUS_MARKDOWN_PATH
    status_json_path.parent.mkdir(parents=True, exist_ok=True)
    status_json_path.write_text(json.dumps(status, indent=2) + "\n", encoding="utf-8")
    status_markdown_path.write_text(render_status_markdown(status), encoding="utf-8")
    return status


def _derive_kpis(
    latest_payloads: dict[str, dict[str, Any]],
    wrapper_invocations: dict[str, int],
    counts: dict[str, int],
    freshness: dict[str, Any],
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
    if freshness.get("regeneration_required"):
        stale_targets = freshness.get("stale_targets") or []
        signals.append(
            {
                "status": "attention",
                "code": "override_regeneration_required",
                "message": "Onboarding overrides changed after generated artifacts and require regeneration"
                f" ({', '.join(stale_targets)}).",
            }
        )

    return {
        "wrapper_total": wrapper_total,
        "onboarding_commands": onboarding.get("commands", 0),
        "onboarding_notes": onboarding.get("notes", 0),
        "act_has_fast_test": act.get("has_fast_test", False),
        "override_regeneration_required": bool(freshness.get("regeneration_required")),
        "stale_targets": freshness.get("stale_targets", []),
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
    if "override_regeneration_required" in codes:
        suggestions.append(
            {
                "code": "regenerate_onboarding_artifacts",
                "message": "Rerun `aethyme repo compile-skills <repo>` or `aethyme enhance deploy --repo <repo>` so onboarding and Act artifacts match the current override file.",
            }
        )
    return suggestions


def _recommended_next_action(report: dict[str, Any]) -> dict[str, str]:
    kpis = report.get("kpis") or {}
    suggestions = kpis.get("suggestions") or []
    suggestion_codes = {suggestion["code"] for suggestion in suggestions}
    if "regenerate_onboarding_artifacts" in suggestion_codes:
        return {
            "command": "aethyme enhance deploy --repo <repo>",
            "reason": "Override file is newer than generated onboarding/Act artifacts.",
        }
    if "fix_or_reinitialize_override" in suggestion_codes:
        return {
            "command": "aethyme repo validate-onboarding-overrides <repo>",
            "reason": "Override file is invalid and should be fixed before relying on generated skills.",
        }
    if "load_onboarding_and_use_wrapper" in suggestion_codes:
        return {
            "command": "aethyme repo experience-telemetry <repo>",
            "reason": "Enhancement is installed but there is no wrapper usage recorded yet.",
        }
    if "add_fast_test_override" in suggestion_codes:
        return {
            "command": "aethyme repo init-onboarding-overrides <repo>",
            "reason": "Generated Act starter has no fast test command and may need a maintainer override.",
        }
    return {
        "command": "aethyme explore --repo <repo> --request \"<task>\" --format answer-json",
        "reason": "Experience layer is in a healthy state; proceed with Explore.",
    }
