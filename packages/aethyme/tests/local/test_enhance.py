"""Tests for enhancement deployment and generated onboarding artifacts."""

from __future__ import annotations

import json
from pathlib import Path

from click.testing import CliRunner

from src.cli import cli
from src.enhance import deploy, is_ok, verify


def _build_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "package.json").write_text(
        '{"name":"demo","scripts":{"test":"vitest","dev":"vite","build":"vite build"}}\n',
        encoding="utf-8",
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (root / "src").mkdir(exist_ok=True)
    (root / "src" / "main.ts").write_text("console.log('demo')\n", encoding="utf-8")


def test_enhance_deploy_writes_generated_onboarding(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)

    actions = deploy(repo_path)
    written = {action.target.relative_path for action in actions}

    assert ".codex/skills/aethyme/SKILL.md" in written
    assert ".codex/skills/repo-onboarding/SKILL.md" in written
    assert ".claude/skills/repo-onboarding/SKILL.md" in written
    assert ".codex/skills/repo-act/SKILL.md" in written
    assert ".claude/skills/repo-act/SKILL.md" in written
    assert ".aethyme/generated/onboarding.json" in written
    assert ".aethyme/generated/act-starter.json" in written

    artifact = json.loads((repo_path / ".aethyme/generated/onboarding.json").read_text(encoding="utf-8"))
    act_artifact = json.loads((repo_path / ".aethyme/generated/act-starter.json").read_text(encoding="utf-8"))
    telemetry_lines = (repo_path / ".aethyme/generated/experience-telemetry.jsonl").read_text(
        encoding="utf-8"
    ).splitlines()
    assert artifact["telemetry"]["counts"]["commands"] >= 1
    assert artifact["summon"]["task_signals"]
    assert act_artifact["starter_checklists"]["debugging"]
    assert any(json.loads(line)["event_type"] == "enhance.deploy" for line in telemetry_lines)

    results = verify(repo_path)
    assert is_ok(results)

    wrapper_text = (repo_path / ".claude" / "hooks" / "aethyme-load-context.sh").read_text(
        encoding="utf-8"
    )
    assert "repo record-wrapper-invocation" in wrapper_text
    assert "--wrapper aethyme-sessionstart-hook" in wrapper_text


def test_init_and_validate_onboarding_overrides_cli(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    runner = CliRunner()

    init_result = runner.invoke(cli, ["repo", "init-onboarding-overrides", str(repo_path)])
    assert init_result.exit_code == 0, init_result.output
    assert (repo_path / ".aethyme" / "overrides" / "onboarding.json").exists()

    validate_result = runner.invoke(cli, ["repo", "validate-onboarding-overrides", str(repo_path)])
    assert validate_result.exit_code == 0, validate_result.output
    assert "Valid override file" in validate_result.output
    telemetry_lines = (repo_path / ".aethyme/generated/experience-telemetry.jsonl").read_text(
        encoding="utf-8"
    ).splitlines()
    event_types = [json.loads(line)["event_type"] for line in telemetry_lines]
    assert "repo.init-onboarding-overrides" in event_types
    assert "repo.validate-onboarding-overrides" in event_types


def test_enhance_verify_prints_summary(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    deploy(repo_path)
    runner = CliRunner()

    result = runner.invoke(cli, ["enhance", "verify", "--repo", str(repo_path)])
    assert result.exit_code == 0, result.output
    assert "Enhancement summary:" in result.output
    assert "Recommendation: load `repo-onboarding` then run `explore`" in result.output
    assert "Experience telemetry:" in result.output
    telemetry_lines = (repo_path / ".aethyme/generated/experience-telemetry.jsonl").read_text(
        encoding="utf-8"
    ).splitlines()
    assert any(json.loads(line)["event_type"] == "enhance.verify" for line in telemetry_lines)


def test_static_wrapper_template_records_invocation_signal() -> None:
    wrapper_text = (
        Path("packages/aethyme/skills/aethyme/aethyme-explore")
        .read_text(encoding="utf-8")
    )
    assert "repo record-wrapper-invocation" in wrapper_text
    assert "--wrapper aethyme-explore" in wrapper_text


def test_repo_experience_telemetry_reports_json_and_text(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    deploy(repo_path)
    runner = CliRunner()

    runner.invoke(
        cli,
        [
            "repo",
            "record-wrapper-invocation",
            str(repo_path),
            "--wrapper",
            "aethyme-explore",
            "--detail",
            "source=test",
        ],
    )

    json_result = runner.invoke(
        cli,
        ["repo", "experience-telemetry", str(repo_path), "--json-output"],
    )
    assert json_result.exit_code == 0, json_result.output
    payload = json.loads(json_result.output)
    assert payload["event_count"] >= 2
    assert payload["wrapper_invocations"]["aethyme-explore"] >= 1
    assert "enhance.deploy" in payload["by_type"]
    assert payload["kpis"]["wrapper_total"] >= 1
    assert payload["kpis"]["act_has_fast_test"] is True

    text_result = runner.invoke(cli, ["repo", "experience-telemetry", str(repo_path)])
    assert text_result.exit_code == 0, text_result.output
    assert "Wrapper invocations:" in text_result.output
    assert "KPIs:" in text_result.output
    assert "aethyme-explore" in text_result.output


def test_repo_experience_telemetry_flags_no_wrapper_usage(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    _build_repo(repo_path)
    deploy(repo_path)
    runner = CliRunner()

    result = runner.invoke(cli, ["repo", "experience-telemetry", str(repo_path), "--json-output"])
    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    codes = [signal["code"] for signal in payload["kpis"]["signals"]]
    assert "enhanced_but_no_wrapper_usage" in codes
    suggestion_codes = [suggestion["code"] for suggestion in payload["kpis"]["suggestions"]]
    assert "load_onboarding_and_use_wrapper" in suggestion_codes

    text_result = runner.invoke(cli, ["repo", "experience-telemetry", str(repo_path)])
    assert text_result.exit_code == 0, text_result.output
    assert "Suggestions:" in text_result.output
    assert "load_onboarding_and_use_wrapper" in text_result.output
