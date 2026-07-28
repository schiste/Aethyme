"""Contract tests for the headless Codex playground eval runner."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
from types import ModuleType

import pytest

_PACKAGE_ROOT = Path(__file__).resolve().parents[2]
_RUNNER_PATH = _PACKAGE_ROOT / "scripts" / "eval" / "run_codex_eval.py"


def _load_runner() -> ModuleType:
    spec = importlib.util.spec_from_file_location("run_codex_eval", _RUNNER_PATH)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_control_command_uses_clean_codex_runner_without_tool_repo() -> None:
    runner = _load_runner()
    repo_path = Path("/tmp/Playground/Demo/Demo - Control")
    tool_repo = Path("/tmp/Aethyme/packages/aethyme")

    command = runner._build_codex_command(
        repo_path=repo_path,
        tool_repo=tool_repo,
        arm="control",
        schema_file=Path("/tmp/schema.json"),
        last_message_file=Path("/tmp/last-message.json"),
        prompt="inspect auth flow",
    )

    assert "--ignore-user-config" in command
    assert "--json" in command
    assert command.count("--add-dir") == 1
    assert str(tool_repo) not in command
    assert command[-3:] == ["-C", str(repo_path), "inspect auth flow"]


def test_aethyme_command_adds_only_the_tool_repo_surface() -> None:
    runner = _load_runner()
    repo_path = Path("/tmp/Playground/Demo/Demo - Aethyme")
    tool_repo = Path("/tmp/Aethyme/packages/aethyme")

    command = runner._build_codex_command(
        repo_path=repo_path,
        tool_repo=tool_repo,
        arm="aethyme",
        schema_file=Path("/tmp/schema.json"),
        last_message_file=Path("/tmp/last-message.json"),
        prompt="inspect auth flow",
    )

    assert "--ignore-user-config" in command
    assert command.count("--add-dir") == 2
    assert str(tool_repo) in command
    assert command[-3:] == ["-C", str(repo_path), "inspect auth flow"]


def test_control_environment_removes_aethyme_leakage(monkeypatch: pytest.MonkeyPatch) -> None:
    runner = _load_runner()
    monkeypatch.setenv("AETHYME_ROOT", "/leaky/tool/root")
    monkeypatch.setenv("AETHYMEBENCH_SELF_TOOL", "/leaky/manifest.toml")
    monkeypatch.setenv("AETHYME_EVAL_REPO", "/tmp/Playground/Demo/Demo - Control")

    env = runner._codex_env("control", Path("/tmp/Aethyme/packages/aethyme"))

    assert "AETHYME_ROOT" not in env
    assert "AETHYMEBENCH_SELF_TOOL" not in env
    assert "AETHYME_EVAL_REPO" not in env


def test_control_contract_rejects_generated_artifact_leakage(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runner = _load_runner()
    playground_root = tmp_path / "Playground"
    repo_path = playground_root / "Demo" / "Demo - Control"
    repo_path.mkdir(parents=True)
    monkeypatch.setenv("AETHYME_PLAYGROUND_ROOTS", str(playground_root))

    assert runner._enforce_eval_contract(repo_path, _PACKAGE_ROOT, "control")["arm"] == "control"

    (repo_path / ".aethyme").mkdir()
    with pytest.raises(runner.ContractError, match="generated/tool artifacts"):
        runner._enforce_eval_contract(repo_path, _PACKAGE_ROOT, "control")


def test_aethyme_contract_requires_intended_surface_only(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runner = _load_runner()
    playground_root = tmp_path / "Playground"
    repo_path = playground_root / "Demo" / "Demo - Aethyme"
    for path in (
        ".codex/skills/aethyme/references",
        ".aethyme/graph",
    ):
        (repo_path / path).mkdir(parents=True)
    for path in (
        ".codex/skills/aethyme/SKILL.md",
        ".codex/skills/aethyme/references/explore.md",
        ".aethyme/graph_store.redb",
        "AGENTS.md",
        "CLAUDE.md",
    ):
        (repo_path / path).write_text("generated\n", encoding="utf-8")
    monkeypatch.setenv("AETHYME_PLAYGROUND_ROOTS", str(playground_root))

    contract = runner._enforce_eval_contract(repo_path, _PACKAGE_ROOT, "aethyme")
    assert contract["aethyme_intended_surface_present"] is True

    (repo_path / ".codex/skills/eval").mkdir()
    with pytest.raises(runner.ContractError, match="internal eval skill"):
        runner._enforce_eval_contract(repo_path, _PACKAGE_ROOT, "aethyme")


def test_command_output_metric_reads_event_output_fields(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": "abc",
                    "stderr": "de",
                    "output": "f",
                    "message": "not command output",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    assert runner._command_output_chars(events_file) == 6
