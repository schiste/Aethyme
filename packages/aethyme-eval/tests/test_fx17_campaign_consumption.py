"""Regression coverage for the once-only FX17 campaign claim handoff."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

import pytest

_EVAL_ROOT = Path(__file__).resolve().parents[1]
_FX17_DIR = _EVAL_ROOT / "evals" / "fx17"
_RUNNER_PATH = _FX17_DIR / "run_fx17.py"
_SPEC = importlib.util.spec_from_file_location("fx17_run_fx17_campaign_test", _RUNNER_PATH)
assert _SPEC is not None and _SPEC.loader is not None
fx17 = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fx17)


def test_run_hands_gate_registry_to_once_only_claim_before_any_model_launch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    registry = tmp_path / "disposable-registry"
    registry.mkdir()
    output = tmp_path / "trial-output"
    approval_path = tmp_path / "approval.json"
    freeze_root = _FX17_DIR / "frozen"
    payloads, _, _, schedule_hash, payload_hash = fx17.verify_frozen(freeze_root)

    approval = {
        "schema": "aethyme.fx17.explicit-trial-approval/v1",
        "approved": True,
        "approved_by": "unpaid regression test",
        "original_fixture_manifest_sha256": fx17.FIXTURE_SHA,
        "independent_brief_manifest_sha256": fx17.BRIEF_SHA,
        "combined_schedule_sha256": schedule_hash,
        "payloads_sha256": payload_hash,
        "assigned_denominator": 6,
        "model": fx17.MODEL,
        "reasoning_effort": fx17.EFFORT,
        "max_wall_seconds_per_attempt": fx17.WALL,
        "max_tool_calls_per_attempt": fx17.TOOL_MAX,
        "max_tokens_per_attempt": fx17.TOKEN_MAX,
        "max_attempts_per_row": 1,
        "automatic_retries": 0,
        "runtime_contract_correction_sha256": fx17.RUNTIME_CORRECTION_SHA256,
        "runtime_profile_sha256": fx17.runtime_profile_hash(),
        "output_token_limit_enforcement": "unverified",
        "provider_retry_behavior": "unknown",
        "consumption_registry_path": str(registry.resolve(strict=True)),
    }
    approval_raw = fx17.jbytes(approval)
    approval_path.write_bytes(approval_raw)

    fake_controls = {
        "independent_review": {"reviewer": "unpaid regression stub"},
        "runner_sha256": fx17.runner_hash(),
        "control_harness_sha256": "0" * 64,
        "subject_profile_template_sha256": fx17.profile_hash(),
        "evidence_sha256": "1" * 64,
    }

    class ModelLaunchForbidden(RuntimeError):
        pass

    def reject_model_row(*_args: object, **_kwargs: object) -> None:
        raise ModelLaunchForbidden

    def reject_process_launch(*_args: object, **_kwargs: object) -> None:
        pytest.fail("model subprocess must not launch in this test")

    monkeypatch.setattr(fx17, "require_controls", lambda _path: fake_controls)
    monkeypatch.setattr(fx17, "verify_codex_binary", lambda: fx17.CODEX_SHA256)
    monkeypatch.setattr(fx17.sys, "platform", "darwin")
    monkeypatch.setattr(fx17, "run_row", reject_model_row)
    monkeypatch.setattr(fx17.subprocess, "Popen", reject_process_launch)

    with pytest.raises(ModelLaunchForbidden):
        fx17.run(
            freeze_root,
            approval_path,
            tmp_path / "stub-controls.json",
            output,
            schedule_hash,
            payload_hash,
        )

    key = fx17.campaign_consumption_key(schedule_hash, payload_hash)
    marker = registry / f"fx17-{key}.consumed.json"
    assert marker.is_file()
    marker_bytes = marker.read_bytes()
    marker_record = json.loads(marker_bytes)
    assert marker_record["approval_sha256"] == fx17.digest(approval_raw)
    assert marker_record["state"] == "started"
    assert (output / "results.json").is_file()
    state = json.loads((output / "results.json").read_bytes())
    assert state["assigned_denominator"] == 6
    assert state["rows"][0]["status"] == "running"
    assert len(payloads) == 6

    with pytest.raises(fx17.Fx17Error, match="already consumed"):
        fx17.claim_campaign_consumption(
            str(registry.resolve(strict=True)), schedule_hash, payload_hash, approval_raw
        )
    assert marker.read_bytes() == marker_bytes
