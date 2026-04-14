"""Durable observability helpers for evaluation lifecycle artifacts."""

from __future__ import annotations

import json
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from src.contracts.versions import contract_versions

from .report import EVAL_RUNS_ROOT

SETUP_RUNS_ROOT = EVAL_RUNS_ROOT / "setup-runs"
PLANS_ROOT = EVAL_RUNS_ROOT / "plans"


def utc_now_iso() -> str:
    return datetime.now(UTC).isoformat()


def _slugify(value: str) -> str:
    text = value.strip().lower()
    text = re.sub(r"[^a-z0-9]+", "-", text)
    return text.strip("-") or "artifact"


def _ensure_parent(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)


def write_json(path: Path, payload: dict[str, Any]) -> Path:
    _ensure_parent(path)
    path.write_text(json.dumps(payload, indent=2, default=str), encoding="utf-8")
    return path


def append_log(path: Path, message: str) -> Path:
    _ensure_parent(path)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(f"{utc_now_iso()} {message}\n")
    return path


def setup_task_dir(task_id: str) -> Path:
    return SETUP_RUNS_ROOT / task_id


def initialize_setup_task_artifacts(task_id: str, request: dict[str, Any]) -> Path:
    task_dir = setup_task_dir(task_id)
    task_dir.mkdir(parents=True, exist_ok=True)
    write_json(
        task_dir / "request.json",
        {
            "task_id": task_id,
            "created_at": utc_now_iso(),
            "contract_versions": contract_versions(),
            "request": request,
        },
    )
    return task_dir


def write_setup_task_result(task_id: str, payload: dict[str, Any]) -> Path:
    task_dir = setup_task_dir(task_id)
    task_dir.mkdir(parents=True, exist_ok=True)
    return write_json(
        task_dir / "result.json",
        {
            "task_id": task_id,
            "updated_at": utc_now_iso(),
            "contract_versions": contract_versions(),
            **payload,
        },
    )


def persist_plan_snapshot(
    *,
    plan: dict[str, Any],
    request: dict[str, Any],
    plan_id: str | None = None,
) -> dict[str, Any]:
    effective_plan_id = plan_id or _default_plan_id(request, plan)
    path = PLANS_ROOT / f"{effective_plan_id}.json"
    payload = {
        "id": effective_plan_id,
        "created_at": utc_now_iso(),
        "contract_versions": contract_versions(),
        "request": request,
        "plan": plan,
        "path": str(path),
    }
    write_json(path, payload)
    return payload


def write_run_plan(
    run_dir: Path,
    *,
    plan: dict[str, Any],
    request: dict[str, Any],
    plan_snapshot: dict[str, Any] | None = None,
) -> Path:
    payload = {
        "created_at": utc_now_iso(),
        "contract_versions": contract_versions(),
        "request": request,
        "plan": plan,
    }
    if plan_snapshot:
        payload["plan_snapshot"] = {
            "id": plan_snapshot.get("id"),
            "path": plan_snapshot.get("path"),
            "created_at": plan_snapshot.get("created_at"),
        }
    return write_json(run_dir / "plan.json", payload)


def write_phase_record(
    run_dir: Path,
    *,
    phase_name: str,
    status: str,
    started_at: str | None = None,
    finished_at: str | None = None,
    details: dict[str, Any] | None = None,
    order: int | None = None,
) -> Path:
    phase_dir = run_dir / "phases"
    phase_dir.mkdir(parents=True, exist_ok=True)
    slug = _slugify(phase_name)
    filename = f"{order:02d}-{slug}.json" if order is not None else f"{slug}.json"
    payload: dict[str, Any] = {
        "phase": phase_name,
        "status": status,
        "contract_versions": contract_versions(),
        "updated_at": utc_now_iso(),
    }
    if started_at:
        payload["started_at"] = started_at
    if finished_at:
        payload["finished_at"] = finished_at
    if started_at and finished_at:
        duration = _duration_seconds(started_at, finished_at)
        if duration is not None:
            payload["duration_seconds"] = duration
    if details:
        payload["details"] = details
    return write_json(phase_dir / filename, payload)


def _default_plan_id(request: dict[str, Any], plan: dict[str, Any]) -> str:
    run_dir = plan.get("paths", {}).get("run_dir")
    if run_dir:
        return Path(run_dir).name
    stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    target = request.get("target", "target")
    eval_type = request.get("evalType") or request.get("eval_type") or "eval"
    return f"{stamp}-{_slugify(str(target))}-{_slugify(str(eval_type))}"


def _duration_seconds(started_at: str, finished_at: str) -> float | None:
    try:
        start = datetime.fromisoformat(started_at.replace("Z", "+00:00"))
        end = datetime.fromisoformat(finished_at.replace("Z", "+00:00"))
    except ValueError:
        return None
    return round((end - start).total_seconds(), 3)
