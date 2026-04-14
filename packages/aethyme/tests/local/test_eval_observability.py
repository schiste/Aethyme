"""Observability artifact tests for the eval lifecycle."""

from __future__ import annotations

import json
from pathlib import Path

from src.eval import observability as obs


def test_setup_task_artifacts_are_persisted(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setattr(obs, "SETUP_RUNS_ROOT", tmp_path / "setup-runs")

    request = {
        "target": "mediawiki",
        "source": "https://example.com/repo.git",
        "commit": "abc123",
        "force": False,
    }
    task_dir = obs.initialize_setup_task_artifacts("setup-mediawiki-1", request)
    obs.append_log(task_dir / "events.log", "setup queued")
    result_path = obs.write_setup_task_result(
        "setup-mediawiki-1",
        {
            "status": "complete",
            "request": request,
            "skipped": True,
            "output": "already ready",
        },
    )

    assert task_dir == tmp_path / "setup-runs" / "setup-mediawiki-1"
    assert (task_dir / "request.json").exists()
    assert (task_dir / "events.log").read_text(encoding="utf-8").strip().endswith("setup queued")

    payload = json.loads(result_path.read_text(encoding="utf-8"))
    assert payload["status"] == "complete"
    assert payload["request"]["target"] == "mediawiki"
    assert payload["skipped"] is True
    assert payload["contract_versions"]["run_metadata"] == "1"


def test_plan_and_phase_artifacts_are_self_contained(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setattr(obs, "PLANS_ROOT", tmp_path / "plans")
    run_dir = tmp_path / "eval-runs" / "20260414-mediawiki-dead-code"
    run_dir.mkdir(parents=True)

    request = {
        "evalType": "dead-code",
        "target": "mediawiki",
        "model": "haiku",
        "reasoning": "high",
    }
    plan = {
        "meta": {"conditions": ["control-cto-off", "control-cto-on", "explore", "leverage"]},
        "paths": {"run_dir": str(run_dir)},
        "phases": [{"name": "launch"}],
    }

    snapshot = obs.persist_plan_snapshot(plan=plan, request=request)
    phase_path = obs.write_phase_record(
        run_dir,
        phase_name="build inputs",
        status="success",
        order=1,
        started_at="2026-04-14T10:00:00+00:00",
        finished_at="2026-04-14T10:00:03+00:00",
        details={"prompt_count": 4},
    )
    run_plan_path = obs.write_run_plan(
        run_dir,
        plan=plan,
        request=request,
        plan_snapshot=snapshot,
    )

    snapshot_payload = json.loads(Path(snapshot["path"]).read_text(encoding="utf-8"))
    assert snapshot_payload["request"]["evalType"] == "dead-code"
    assert snapshot_payload["plan"]["paths"]["run_dir"] == str(run_dir)

    phase_payload = json.loads(phase_path.read_text(encoding="utf-8"))
    assert phase_payload["phase"] == "build inputs"
    assert phase_payload["status"] == "success"
    assert phase_payload["duration_seconds"] == 3.0
    assert phase_payload["details"]["prompt_count"] == 4

    run_plan_payload = json.loads(run_plan_path.read_text(encoding="utf-8"))
    assert run_plan_payload["plan_snapshot"]["id"] == snapshot["id"]
    assert run_plan_payload["request"]["model"] == "haiku"
