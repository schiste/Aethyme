"""Contract tests for the headless Codex playground eval runner."""

from __future__ import annotations

import importlib.util
import json
import subprocess
from pathlib import Path
from types import ModuleType

import pytest

_PACKAGE_ROOT = Path(__file__).resolve().parents[2]
_RUNNER_PATH = _PACKAGE_ROOT / "scripts" / "eval" / "run_codex_eval.py"
_GATE_PATH = _PACKAGE_ROOT / "scripts" / "eval" / "check_regression_gate.py"


def _load_runner() -> ModuleType:
    spec = importlib.util.spec_from_file_location("run_codex_eval", _RUNNER_PATH)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_gate() -> ModuleType:
    spec = importlib.util.spec_from_file_location("check_regression_gate", _GATE_PATH)
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
    monkeypatch.setenv("AETHYME_ENGINE_SOCKET_DIR", "/leaky/socket/root")
    monkeypatch.setenv("AETHYMEBENCH_SELF_TOOL", "/leaky/manifest.toml")
    monkeypatch.setenv("AETHYME_EVAL_REPO", "/tmp/Playground/Demo/Demo - Control")

    env = runner._codex_env("control", Path("/tmp/Aethyme/packages/aethyme"))

    assert "AETHYME_ROOT" not in env
    assert "AETHYME_ENGINE_SOCKET_DIR" not in env
    assert "AETHYMEBENCH_SELF_TOOL" not in env
    assert "AETHYME_EVAL_REPO" not in env


def test_aethyme_environment_sets_short_socket_dir() -> None:
    runner = _load_runner()
    tool_repo = Path("/tmp/Aethyme/packages/aethyme")

    env = runner._codex_env("aethyme", tool_repo)

    assert env["AETHYME_ROOT"] == str(tool_repo)
    assert env["AETHYME_ENGINE_SOCKET_DIR"] == "/tmp/aethyme-codex-engine-sockets"


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
                    "aggregated_output": "xyz",
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

    assert runner._command_output_chars(events_file) == 9


def test_runner_emits_stable_regression_metrics(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
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
    artifact_dir = tmp_path / "artifacts"

    monkeypatch.setenv("AETHYME_PLAYGROUND_ROOTS", str(playground_root))
    monkeypatch.setenv("AETHYME_EVAL_ARM", "aethyme")
    monkeypatch.setenv("AETHYME_EVAL_REPO", str(repo_path))
    monkeypatch.setenv("AETHYME_EVAL_ARTIFACT_DIR", str(artifact_dir))
    monkeypatch.setenv("AETHYME_EVAL_OUTPUT_SCHEMA", '{"type":"object"}')
    monkeypatch.setenv("AETHYME_EVAL_PROMPT", "inspect auth flow")
    monkeypatch.setenv("AETHYME_EVAL_TOOL_REPO", str(_PACKAGE_ROOT))

    command_output = "aethyme explore --repo .\n"

    def fake_run(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        last_message_path = Path(command[command.index("--output-last-message") + 1])
        last_message_path.write_text(
            json.dumps(
                {
                    "selected_files": ["src/auth.py", "src/routes.py"],
                    "snippets": [{"path": "src/auth.py"}],
                    "answer": "clean final answer",
                }
            ),
            encoding="utf-8",
        )
        stdout = (
            json.dumps(
                {
                    "type": "item.completed",
                    "item": {
                        "cmd": ["aethyme", "explore", "--repo", "."],
                        "stdout": command_output,
                    },
                }
            )
            + "\n"
            + json.dumps(
                {
                    "type": "turn.completed",
                    "usage": {"input_tokens": 100, "output_tokens": 25},
                }
            )
            + "\n"
        )
        return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    assert runner.main() == 0
    payload = json.loads(capsys.readouterr().out)
    metrics = payload["regression_metrics"]
    assert metrics == {
        "token_estimate": 125,
        "selected_file_count": 2,
        "snippet_count": 1,
        "command_output_chars": len(command_output),
        "aethyme_path_leaked": False,
        "aethyme_invoked": True,
        "arm": "aethyme",
    }


def test_leakage_gate_checks_selected_files_snippets_command_output_and_final_answer(
    tmp_path: Path,
) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": "read .aethyme/graph_store.redb",
                    "message": ".aethyme here is not command output",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    leakage = runner._detect_artifact_leakage(
        structured_output={
            "selected_files": ["src/auth/token.py", ".aethyme/graph/auth.bin"],
            "snippets": [{"path": "src/web.py"}, {"path": ".aethyme/generated/context.json"}],
        },
        final_output_message="The answer mentions .aethyme/graph.",
        events_file=events_file,
    )

    assert leakage["aethyme_path_leaked"] is True
    sources = {leak["source"] for leak in leakage["leaks"]}
    assert sources == {"structured_output", "final_output_message", "command_output"}
    paths = {leak["path"] for leak in leakage["leaks"]}
    assert "$.selected_files[1]" in paths
    assert "$.snippets[1].path" in paths
    assert "event[1].item.stdout" in paths
    assert all(".aethyme" in leak["excerpt"] for leak in leakage["leaks"])


def test_leakage_gate_ignores_non_output_event_metadata(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "message": "internal metadata mentions .aethyme but was not stdout",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    leakage = runner._detect_artifact_leakage(
        structured_output={"selected_files": ["src/auth/token.py"], "snippets": []},
        final_output_message="No generated path leaked.",
        events_file=events_file,
    )

    assert leakage["aethyme_path_leaked"] is False
    assert leakage["leaks"] == []


def test_leakage_gate_ignores_aethyme_product_domains(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "aggregated_output": "external client calls https://mordor.aethyme.com/api",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    leakage = runner._detect_artifact_leakage(
        structured_output={
            "evidence": ["external client calls https://mordor.aethyme.com/api"],
        },
        final_output_message="Boundary includes https://mordor.aethyme.com/api.",
        events_file=events_file,
    )

    assert leakage["aethyme_path_leaked"] is False
    assert leakage["leaks"] == []


def test_main_fails_when_command_output_leaks_aethyme_path(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    runner = _load_runner()
    playground_root = tmp_path / "Playground"
    repo_path = playground_root / "Demo" / "Demo - Control"
    repo_path.mkdir(parents=True)
    artifact_dir = tmp_path / "artifacts"

    monkeypatch.setenv("AETHYME_PLAYGROUND_ROOTS", str(playground_root))
    monkeypatch.setenv("AETHYME_EVAL_ARM", "control")
    monkeypatch.setenv("AETHYME_EVAL_REPO", str(repo_path))
    monkeypatch.setenv("AETHYME_EVAL_ARTIFACT_DIR", str(artifact_dir))
    monkeypatch.setenv("AETHYME_EVAL_OUTPUT_SCHEMA", '{"type":"object"}')
    monkeypatch.setenv("AETHYME_EVAL_PROMPT", "inspect auth flow")

    def fake_run(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        last_message_path = Path(command[command.index("--output-last-message") + 1])
        last_message_path.write_text(json.dumps({"answer": "clean final answer"}), encoding="utf-8")
        stdout = (
            json.dumps(
                {
                    "type": "item.completed",
                    "item": {"stdout": "read .aethyme/graph_store.redb"},
                }
            )
            + "\n"
        )
        return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")

    monkeypatch.setattr(runner.subprocess, "run", fake_run)

    assert runner.main() == 3
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifact_leakage"]["aethyme_path_leaked"] is True
    assert Path(payload["leakage_file"]).is_file()


def test_regression_gate_compares_counts_not_selected_file_identity() -> None:
    gate = _load_gate()
    control = {
        "regression_metrics": {
            "token_estimate": 1_000,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 2_000,
            "aethyme_path_leaked": False,
            "aethyme_invoked": False,
        },
        "structured_output": {"selected_files": ["src/a.py", "src/b.py"]},
        "reviewer_quality_score": 4.0,
    }
    aethyme = {
        "regression_metrics": {
            "token_estimate": 1_050,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 2_100,
            "aethyme_path_leaked": False,
            "aethyme_invoked": True,
        },
        "structured_output": {"selected_files": ["src/c.py", "src/d.py"]},
        "reviewer_quality_score": 4.0,
    }

    report = gate.compare_runs(control, aethyme)

    assert report["passed"] is True
    assert report["contract"]["selected_file_contents_compared"] is False
    assert report["contract"]["selected_file_count_compared"] is True


def test_regression_gate_can_infer_invocation_from_legacy_event_log(tmp_path: Path) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {"cmd": ["aethyme", "explore", "--task", "auth"]},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    control = {
        "input_tokens": 100,
        "output_tokens": 50,
        "command_output_chars": 100,
        "artifact_leakage": {"aethyme_path_leaked": False},
        "reviewer_quality_score": 4.0,
    }
    aethyme = {
        "input_tokens": 100,
        "output_tokens": 50,
        "command_output_chars": 100,
        "artifact_leakage": {"aethyme_path_leaked": False},
        "event_log_file": str(events_file),
        "reviewer_quality_score": 4.0,
    }

    report = gate.compare_runs(control, aethyme)

    assert report["passed"] is True
    assert report["aethyme_metrics"]["aethyme_invoked"] is True


def test_regression_gate_fails_hygiene_invocation_and_quality_regressions() -> None:
    gate = _load_gate()
    control = {
        "regression_metrics": {
            "token_estimate": 1_000,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 2_000,
            "aethyme_path_leaked": False,
            "aethyme_invoked": False,
        },
        "reviewer_quality_score": 5.0,
    }
    aethyme = {
        "regression_metrics": {
            "token_estimate": 1_000,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 2_000,
            "aethyme_path_leaked": True,
            "aethyme_invoked": False,
        },
        "reviewer_quality_score": 4.0,
    }

    report = gate.compare_runs(control, aethyme)

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "aethyme_invoked" in failures
    assert "aethyme_no_aethyme_path_leak" in failures
    assert "final_answer_quality_not_worse" in failures


def test_regression_gate_fails_budget_delta_without_file_equality() -> None:
    gate = _load_gate()
    control = {
        "regression_metrics": {
            "token_estimate": 1_000,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 1_000,
            "aethyme_path_leaked": False,
            "aethyme_invoked": False,
        },
        "reviewer_quality_score": 4.0,
    }
    aethyme = {
        "regression_metrics": {
            "token_estimate": 1_500,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 1_000,
            "aethyme_path_leaked": False,
            "aethyme_invoked": True,
        },
        "structured_output": {"selected_files": ["src/other.py", "src/path.py"]},
        "reviewer_quality_score": 4.0,
    }

    report = gate.compare_runs(
        control,
        aethyme,
        token_delta_ratio=0.0,
        token_slack=0,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert failures == {"token_estimate_delta"}
