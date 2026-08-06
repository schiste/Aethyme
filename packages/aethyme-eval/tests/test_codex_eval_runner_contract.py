"""Contract tests for the headless Codex playground eval runner."""

from __future__ import annotations

import importlib.util
import json
import subprocess
from pathlib import Path
from types import ModuleType

import pytest

_EVAL_PACKAGE_ROOT = Path(__file__).resolve().parents[1]
_MONOREPO_ROOT = _EVAL_PACKAGE_ROOT.parents[1]
# The MEASURED system. Several cases below pass it where an "Aethyme
# checkout" is expected — as the tool repo handed to the aethyme arm, and
# as a repo path the contract must refuse as an eval target (cardinal
# rule 1). It stays `packages/aethyme` after the python-retirement
# Phase 7 move of this file into `packages/aethyme-eval`.
_PACKAGE_ROOT = _MONOREPO_ROOT / "packages" / "aethyme"
_RUNNER_PATH = _EVAL_PACKAGE_ROOT / "scripts" / "run_codex_eval.py"
_GATE_PATH = _EVAL_PACKAGE_ROOT / "scripts" / "check_regression_gate.py"


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


def _surface_flow_observability(missing: list[str] | None = None) -> dict[str, object]:
    missing = missing or []
    coverage: dict[str, object] = {"backend": {"status": "covered"}}
    for surface_type in missing:
        coverage[surface_type] = {"status": "source_present_not_indexed"}
    return {
        "observability": {
            "surface_flow_graph": {
                "status": "partial" if missing else "covered",
                "coverage": coverage,
                "missing_expected_surfaces": [
                    {"surface_type": surface_type, "reason": "fixture expects a visible gap"}
                    for surface_type in missing
                ],
            },
            "graph_completeness_by_surface_type": coverage,
            "missing_expected_surfaces": [
                {"surface_type": surface_type, "reason": "fixture expects a visible gap"}
                for surface_type in missing
            ],
        }
    }


def _surface_flow_subsystems(fixture_id: str) -> list[dict[str, object]]:
    if fixture_id == "edge_proxy_backend_auth":
        return [
            {"role": "ingress_proxy", "top_verification_targets": ["gcp-run-proxy"]},
            {"role": "backend_validator", "top_verification_targets": ["backend/api_keys"]},
        ]
    if fixture_id == "django_backend_auth":
        return [{"role": "backend_validator", "top_verification_targets": ["backend/auth"]}]
    if fixture_id in {"oidc_session_auth", "webhook_secret_auth"}:
        return [
            {
                "role": "provider_or_secondary_token",
                "top_verification_targets": ["backend/providers"],
            }
        ]
    return [{"role": "ingress_proxy", "top_verification_targets": ["src/routes"]}]


def _strict_gate_payload(
    *,
    arm: str,
    fixture_id: str = "edge_proxy_backend_auth",
    repo_path: str | None = None,
    token_estimate: int = 1_000,
    command_output_chars: int = 1_000,
    aethyme_invoked: bool | None = None,
    generated_artifact_leaked: bool = False,
    quality: float = 4.0,
    missing_coverage: list[str] | None = None,
    answer: str = "stable answer",
) -> dict[str, object]:
    invoked = arm == "aethyme" if aethyme_invoked is None else aethyme_invoked
    output_tokens = min(100, token_estimate)
    total_input_tokens = token_estimate - output_tokens
    structured_output: dict[str, object] = {
        "selected_files": ["src/auth.py", "src/routes.py"],
        "snippets": [{"path": "src/auth.py"}],
        "answer": answer,
        **_surface_flow_observability(missing_coverage or []),
    }
    if arm == "aethyme":
        structured_output["subsystems"] = _surface_flow_subsystems(fixture_id)

    return {
        "arm": arm,
        "fixture_id": fixture_id,
        "contract": {
            "playground_repo": True,
            "aethyme_self_eval": False,
            "arm": arm,
            "repo_path": repo_path or f"/tmp/Playground/Demo/Demo - {arm.title()}",
            "fixture_id": fixture_id,
        },
        "regression_metrics": {
            "token_estimate": token_estimate,
            "total_input_tokens": total_input_tokens,
            "output_tokens": output_tokens,
            "cached_input_tokens": 0,
            "uncached_input_tokens": total_input_tokens,
            "uncached_plus_output_tokens": token_estimate,
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": command_output_chars,
            "max_single_command_output_chars": command_output_chars,
            "explore_output_chars": command_output_chars if arm == "aethyme" else 0,
            "cumulative_replay_token_estimate": token_estimate,
            "generated_artifact_leaked": generated_artifact_leaked,
            "aethyme_path_leaked": generated_artifact_leaked,
            "aethyme_invoked": invoked,
            "first_aethyme_call_before_broad_search": True if arm == "aethyme" else None,
            "broad_rg_after_successful_explore": False,
            "successful_explore_detected": arm == "aethyme",
        },
        "structured_output": structured_output,
        "reviewer_quality_score": quality,
    }


def _event_gate_payload(
    *,
    arm: str,
    event_log_file: Path | None = None,
    fixture_id: str = "edge_proxy_backend_auth",
    command_output_chars: int = 1_000,
) -> dict[str, object]:
    payload: dict[str, object] = {
        "arm": arm,
        "fixture_id": fixture_id,
        "contract": {
            "playground_repo": True,
            "aethyme_self_eval": False,
            "arm": arm,
            "repo_path": f"/tmp/Playground/Demo/Demo - {arm.title()}",
            "fixture_id": fixture_id,
        },
        "input_tokens": 800,
        "output_tokens": 200,
        "command_output_chars": command_output_chars,
        "artifact_leakage": {
            "generated_artifact_leaked": False,
            "aethyme_path_leaked": False,
        },
        "structured_output": {
            "selected_files": ["src/auth.py", "src/routes.py"],
            "snippets": [{"path": "src/auth.py"}],
            "answer": "stable answer",
        },
        "reviewer_quality_score": 4.0,
    }
    if event_log_file is not None:
        payload["event_log_file"] = str(event_log_file)
    return payload


def _write_events(path: Path, events: list[dict[str, object]]) -> None:
    path.write_text(
        "".join(json.dumps(event) + "\n" for event in events),
        encoding="utf-8",
    )


def _explore_output_with_lanes() -> str:
    return json.dumps(
        {
            "subsystems": _surface_flow_subsystems("edge_proxy_backend_auth"),
            "observability": _surface_flow_observability()["observability"],
        }
    )


def test_control_command_uses_clean_codex_runner_without_tool_repo() -> None:
    runner = _load_runner()
    repo_path = Path("/tmp/Playground/Demo/Demo - Control")
    tool_repo = Path("/tmp/Aethyme/packages/aethyme")
    schema_file = Path("/private/tmp/aethyme-eval/schema/schema.json")
    last_message_file = Path("/private/tmp/aethyme-eval/artifacts/control/last-message.json")

    command = runner._build_codex_command(
        repo_path=repo_path,
        tool_repo=tool_repo,
        arm="control",
        schema_file=schema_file,
        last_message_file=last_message_file,
        prompt="inspect auth flow",
    )

    assert "--ignore-user-config" in command
    assert "--json" in command
    assert command.count("--add-dir") == 2
    add_dirs = _command_add_dirs(command)
    assert str(schema_file.parent) in add_dirs
    assert str(last_message_file.parent) in add_dirs
    assert "/tmp" not in add_dirs
    assert str(tool_repo) not in command
    assert command[-3:] == ["-C", str(repo_path), "inspect auth flow"]


def test_aethyme_command_adds_only_the_tool_repo_surface() -> None:
    runner = _load_runner()
    repo_path = Path("/tmp/Playground/Demo/Demo - Aethyme")
    tool_repo = Path("/tmp/Aethyme/packages/aethyme")
    schema_file = Path("/private/tmp/aethyme-eval/schema/schema.json")
    last_message_file = Path("/private/tmp/aethyme-eval/artifacts/aethyme/last-message.json")

    command = runner._build_codex_command(
        repo_path=repo_path,
        tool_repo=tool_repo,
        arm="aethyme",
        schema_file=schema_file,
        last_message_file=last_message_file,
        prompt="inspect auth flow",
    )

    assert "--ignore-user-config" in command
    assert command.count("--add-dir") == 3
    add_dirs = _command_add_dirs(command)
    assert str(schema_file.parent) in add_dirs
    assert str(last_message_file.parent) in add_dirs
    assert "/tmp" not in add_dirs
    assert str(tool_repo) in command
    assert command[-3:] == ["-C", str(repo_path), "inspect auth flow"]


def test_codex_artifact_dirs_are_deduplicated() -> None:
    runner = _load_runner()
    artifact_dir = Path("/private/tmp/aethyme-eval/artifacts/run")

    assert runner._codex_artifact_dirs(
        artifact_dir / "schema.json",
        artifact_dir / "last-message.json",
    ) == (artifact_dir,)


def _command_add_dirs(command: list[str]) -> list[str]:
    return [command[index + 1] for index, item in enumerate(command[:-1]) if item == "--add-dir"]


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


def test_runner_usage_metrics_split_cached_and_uncached_tokens(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    _write_events(
        events_file,
        [
            {
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 355_332,
                    "output_tokens": 4_526,
                    "cached_input_tokens": 311_808,
                },
            }
        ],
    )

    usage = runner._parse_usage(events_file)

    assert usage["input_tokens"] == 355_332
    assert usage["output_tokens"] == 4_526
    assert usage["cached_input_tokens"] == 311_808
    assert usage["uncached_input_tokens"] == 43_524
    assert usage["uncached_plus_output_tokens"] == 48_050


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
    assert metrics["token_estimate"] == 125
    assert metrics["total_input_tokens"] == 100
    assert metrics["output_tokens"] == 25
    assert metrics["cached_input_tokens"] == 0
    assert metrics["uncached_input_tokens"] == 100
    assert metrics["uncached_plus_output_tokens"] == 125
    assert metrics["selected_file_count"] == 2
    assert metrics["snippet_count"] == 1
    assert metrics["command_output_chars"] == len(command_output)
    assert metrics["generated_artifact_leaked"] is False
    assert metrics["aethyme_path_leaked"] is False
    assert metrics["aethyme_invoked"] is True
    assert metrics["arm"] == "aethyme"
    assert isinstance(metrics["output_fingerprint"], str)
    assert len(metrics["output_fingerprint"]) == 64


def test_runner_invocation_detection_ignores_non_command_text_and_output(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": "The answer says to run aethyme explore later.",
                    "message": ["aethyme", "explore"],
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    assert runner._aethyme_invoked(events_file) is False


def test_runner_invocation_detection_handles_shell_wrapped_command(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.started",
                "item": {
                    "command": (
                        "/bin/zsh -lc "
                        "'/tmp/aethyme/rust/target/release/aethyme explore "
                        '--repo "$PWD" --format answer-json\''
                    )
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    assert runner._aethyme_invoked(events_file) is True


def test_runner_invocation_detection_handles_shell_variable_binary(tmp_path: Path) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "command": (
                        "/bin/zsh -lc "
                        "'AETHYME_BIN=/tool/rust/target/release/aethyme; "
                        '"$AETHYME_BIN" explore --repo "$PWD" --format answer-json\''
                    )
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    assert runner._aethyme_invoked(events_file) is True


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


def test_leakage_gate_detects_generated_scaffolding_beyond_aethyme_paths(
    tmp_path: Path,
) -> None:
    runner = _load_runner()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": "opened .codex/skills/aethyme/SKILL.md and graph_store.redb",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )

    leakage = runner._detect_artifact_leakage(
        structured_output={
            "selected_files": ["AGENTS.md", ".chau7/snippets/current.json"],
            "snippets": [{"path": ".claude/skills/aethyme/SKILL.md"}],
        },
        final_output_message="The answer should not mention CLAUDE.md.",
        events_file=events_file,
    )

    assert leakage["generated_artifact_leaked"] is True
    markers = {leak["marker"] for leak in leakage["leaks"]}
    assert {
        ".codex",
        "graph_store.redb",
        "AGENTS.md",
        ".chau7",
        ".claude",
        "CLAUDE.md",
    }.issubset(markers)


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


def test_regression_gate_uses_uncached_plus_output_as_strict_budget_signal() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control")
    control["regression_metrics"].update(
        {
            "token_estimate": 208_655,
            "total_input_tokens": 204_197,
            "output_tokens": 4_458,
            "cached_input_tokens": 139_264,
            "uncached_input_tokens": 64_933,
            "uncached_plus_output_tokens": 69_391,
        }
    )
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["regression_metrics"].update(
        {
            "token_estimate": 359_858,
            "total_input_tokens": 355_332,
            "output_tokens": 4_526,
            "cached_input_tokens": 311_808,
            "uncached_input_tokens": 43_524,
            "uncached_plus_output_tokens": 48_050,
        }
    )

    report = gate.compare_runs(
        control,
        aethyme,
        token_delta_ratio=0.0,
        token_slack=0,
    )

    assert report["passed"] is True
    budget_check = next(
        check for check in report["checks"] if check["name"] == "uncached_plus_output_budget_delta"
    )
    assert budget_check["baseline"] == 69_391
    assert budget_check["actual"] == 48_050
    total_check = next(
        check for check in report["checks"] if check["name"] == "token_estimate_delta"
    )
    assert total_check["warning"] is True


def test_regression_gate_recovers_cached_usage_from_legacy_event_logs(tmp_path: Path) -> None:
    gate = _load_gate()
    control_events = tmp_path / "control-events.jsonl"
    aethyme_events = tmp_path / "aethyme-events.jsonl"
    _write_events(
        control_events,
        [
            {
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 204_197,
                    "output_tokens": 4_458,
                    "cached_input_tokens": 139_264,
                },
            }
        ],
    )
    _write_events(
        aethyme_events,
        [
            {
                "type": "item.completed",
                "item": {
                    "cmd": ["aethyme", "explore", "--request", "auth token"],
                    "stdout": _explore_output_with_lanes(),
                },
            },
            {
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 355_332,
                    "output_tokens": 4_526,
                    "cached_input_tokens": 311_808,
                },
            },
        ],
    )
    control = _event_gate_payload(arm="control", event_log_file=control_events)
    control["input_tokens"] = 204_197
    control["output_tokens"] = 4_458
    aethyme = _event_gate_payload(arm="aethyme", event_log_file=aethyme_events)
    aethyme["input_tokens"] = 355_332
    aethyme["output_tokens"] = 4_526

    report = gate.compare_runs(
        control,
        aethyme,
        token_delta_ratio=0.0,
        token_slack=0,
        command_output_delta_ratio=10.0,
    )

    assert report["passed"] is True
    assert report["control_metrics"]["cached_input_tokens"] == 139_264
    assert report["control_metrics"]["uncached_plus_output_tokens"] == 69_391
    assert report["aethyme_metrics"]["cached_input_tokens"] == 311_808
    assert report["aethyme_metrics"]["uncached_plus_output_tokens"] == 48_050


def test_regression_gate_fails_uncached_budget_even_when_total_estimate_drops() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control")
    control["regression_metrics"].update(
        {
            "token_estimate": 100_000,
            "total_input_tokens": 95_000,
            "output_tokens": 5_000,
            "cached_input_tokens": 80_000,
            "uncached_input_tokens": 15_000,
            "uncached_plus_output_tokens": 20_000,
        }
    )
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["regression_metrics"].update(
        {
            "token_estimate": 80_000,
            "total_input_tokens": 75_000,
            "output_tokens": 5_000,
            "cached_input_tokens": 45_000,
            "uncached_input_tokens": 30_000,
            "uncached_plus_output_tokens": 35_000,
        }
    )

    report = gate.compare_runs(
        control,
        aethyme,
        token_delta_ratio=0.0,
        token_slack=0,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert failures == {"uncached_plus_output_budget_delta"}


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


def test_regression_gate_can_infer_shell_wrapped_aethyme_invocation(tmp_path: Path) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.started",
                "item": {
                    "command": (
                        "/bin/zsh -lc "
                        "'/tmp/aethyme/rust/target/release/aethyme explore "
                        '--repo "$PWD" --show-observability\''
                    )
                },
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


def test_regression_gate_enforces_single_command_and_explore_output_caps(
    tmp_path: Path,
) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    explore_output = _explore_output_with_lanes()
    _write_events(
        events_file,
        [
            {
                "type": "item.completed",
                "item": {
                    "cmd": ["aethyme", "explore", "--request", "auth token"],
                    "stdout": explore_output,
                },
            }
        ],
    )
    control = _event_gate_payload(arm="control", command_output_chars=100)
    aethyme = _event_gate_payload(
        arm="aethyme",
        event_log_file=events_file,
        command_output_chars=len(explore_output),
    )

    report = gate.compare_runs(
        control,
        aethyme,
        command_output_delta_ratio=10.0,
        max_command_output_chars=10_000,
        max_single_command_output_chars=40,
        max_explore_output_chars=40,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "aethyme_single_command_output_bounded" in failures
    assert "aethyme_explore_output_bounded" in failures


def test_regression_gate_enforces_cumulative_replay_estimate_cap() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control", token_estimate=400)
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["regression_metrics"]["cumulative_replay_token_estimate"] = 900

    report = gate.compare_runs(
        control,
        aethyme,
        token_delta_ratio=10.0,
        max_cumulative_replay_estimate=500,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert failures == {"aethyme_cumulative_replay_estimate_bounded"}


def test_regression_gate_requires_first_aethyme_call_before_broad_search(
    tmp_path: Path,
) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    _write_events(
        events_file,
        [
            {
                "type": "item.completed",
                "item": {"cmd": ["rg", "auth"], "stdout": "src/auth.py\n"},
            },
            {
                "type": "item.completed",
                "item": {
                    "cmd": ["aethyme", "explore", "--request", "auth token"],
                    "stdout": _explore_output_with_lanes(),
                },
            },
        ],
    )
    control = _event_gate_payload(arm="control")
    aethyme = _event_gate_payload(arm="aethyme", event_log_file=events_file)

    report = gate.compare_runs(
        control,
        aethyme,
        command_output_delta_ratio=10.0,
        require_event_sequence=True,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "aethyme_first_call_before_broad_search" in failures


def test_regression_gate_detects_shell_variable_aethyme_invocation(
    tmp_path: Path,
) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    _write_events(
        events_file,
        [
            {
                "type": "item.completed",
                "item": {
                    "command": (
                        "/bin/zsh -lc "
                        "'AETHYME_BIN=/tool/rust/target/release/aethyme; "
                        '"$AETHYME_BIN" explore --repo "$PWD" --format answer-json\''
                    ),
                    "stdout": _explore_output_with_lanes(),
                },
            },
            {
                "type": "item.completed",
                "item": {"cmd": ["rg", "auth"], "stdout": "src/auth.py\n"},
            },
        ],
    )
    control = _event_gate_payload(arm="control")
    aethyme = _event_gate_payload(arm="aethyme", event_log_file=events_file)

    report = gate.compare_runs(
        control,
        aethyme,
        command_output_delta_ratio=10.0,
        require_event_sequence=True,
    )

    assert report["passed"] is True
    assert report["aethyme_metrics"]["aethyme_invoked"] is True
    assert report["aethyme_metrics"]["first_aethyme_call_before_broad_search"] is True


def test_regression_gate_warns_on_broad_rg_after_successful_explore(
    tmp_path: Path,
) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    _write_events(
        events_file,
        [
            {
                "type": "item.completed",
                "item": {
                    "cmd": ["aethyme", "explore", "--request", "auth token"],
                    "stdout": _explore_output_with_lanes(),
                },
            },
            {
                "type": "item.completed",
                "item": {"cmd": ["rg", "auth"], "stdout": "src/auth.py\n"},
            },
        ],
    )
    control = _event_gate_payload(arm="control")
    aethyme = _event_gate_payload(arm="aethyme", event_log_file=events_file)

    report = gate.compare_runs(
        control,
        aethyme,
        command_output_delta_ratio=10.0,
        require_event_sequence=True,
    )

    assert report["passed"] is True
    warning = next(
        check for check in report["checks"] if check["name"] == "broad_rg_after_successful_explore"
    )
    assert warning["warning"] is True
    assert warning["commands"] == ["rg auth"]


def test_regression_gate_does_not_infer_invocation_from_text_mentions(tmp_path: Path) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": "No command was run; aethyme explore is only mentioned.",
                    "message": ["aethyme", "explore"],
                },
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

    assert report["passed"] is False
    assert report["aethyme_metrics"]["aethyme_invoked"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert failures == {"aethyme_invoked"}


def test_regression_gate_fails_missing_required_metrics() -> None:
    gate = _load_gate()
    control = {
        "regression_metrics": {
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
            "selected_file_count": 2,
            "snippet_count": 1,
            "command_output_chars": 1_000,
            "aethyme_path_leaked": False,
            "aethyme_invoked": True,
        },
        "reviewer_quality_score": 4.0,
    }

    report = gate.compare_runs(control, aethyme)

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "control_metric_token_estimate_valid" in failures
    assert "aethyme_metric_token_estimate_valid" in failures


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
    assert "aethyme_no_generated_artifact_leak" in failures
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
    assert failures == {"uncached_plus_output_budget_delta"}


def test_strict_regression_gate_accepts_repeat_and_reported_missing_coverage() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control", missing_coverage=[])
    aethyme = _strict_gate_payload(arm="aethyme", missing_coverage=["edge_proxy"])

    report = gate.compare_runs(
        control,
        aethyme,
        control_repeat=control,
        aethyme_repeat=aethyme,
        fixture_id="edge-proxy + backend auth",
        expected_missing_coverage=["edge_proxy"],
        max_command_output_chars=2_000,
        require_playground_contract=True,
        require_determinism=True,
        require_coverage_report=True,
    )

    assert report["passed"] is True
    passed = {check["name"] for check in report["checks"] if check["passed"]}
    assert "control_output_deterministic" in passed
    assert "aethyme_output_deterministic" in passed
    assert "surface_flow_coverage_reported" in passed
    assert report["fixture_id"] == "edge_proxy_backend_auth"


def test_strict_regression_gate_fails_missing_repeat_bounded_output_and_self_eval() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(
        arm="control",
        repo_path=str(_PACKAGE_ROOT),
        command_output_chars=3_000,
    )
    aethyme = _strict_gate_payload(arm="aethyme", command_output_chars=3_000)

    report = gate.compare_runs(
        control,
        aethyme,
        max_command_output_chars=2_000,
        require_playground_contract=True,
        require_determinism=True,
        require_coverage_report=True,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "control_repo_path_not_aethyme_checkout" in failures
    assert "control_command_output_bounded" in failures
    assert "aethyme_command_output_bounded" in failures
    assert "control_output_deterministic" in failures
    assert "aethyme_output_deterministic" in failures


def test_regression_gate_fails_when_missing_coverage_is_hidden() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control")
    aethyme = _strict_gate_payload(arm="aethyme", missing_coverage=[])
    aethyme["structured_output"] = {
        "observability": {
            "surface_flow_graph": {
                "status": "partial",
                "coverage": {"edge_proxy": {"status": "source_present_not_indexed"}},
                "missing_expected_surfaces": [],
            }
        }
    }

    report = gate.compare_runs(
        control,
        aethyme,
        control_repeat=control,
        aethyme_repeat=aethyme,
        expected_missing_coverage=["edge_proxy"],
        require_determinism=True,
        require_coverage_report=True,
    )

    assert report["passed"] is False
    failures = {check["name"] for check in report["checks"] if not check["passed"]}
    assert "surface_flow_coverage_reported" in failures
    coverage_check = next(
        check for check in report["checks"] if check["name"] == "surface_flow_coverage_reported"
    )
    assert coverage_check["hidden_missing_coverage"] == ["edge_proxy"]


def test_regression_gate_requires_surface_flow_lanes_for_auth_token_tasks() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control")
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["structured_output"].pop("subsystems")

    report = gate.compare_runs(
        control,
        aethyme,
        fixture_id="edge_proxy_backend_auth",
        require_auth_surface_lanes=True,
    )

    assert report["passed"] is False
    lane_check = next(
        check
        for check in report["checks"]
        if check["name"] == "auth_token_surface_flow_lanes_present"
    )
    assert lane_check["missing_roles"] == ["backend_validator", "ingress_proxy"]


def test_regression_gate_accepts_user_facing_surface_flow_role_aliases() -> None:
    gate = _load_gate()
    control = _strict_gate_payload(arm="control")
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["structured_output"]["subsystems"] = [
        {
            "role": "edge credential transport",
            "top_verification_targets": ["gcp-run-proxy/src/worker.mjs"],
        },
        {
            "role": "authoritative pk validator",
            "top_verification_targets": ["backend/api_keys/middleware.py"],
        },
    ]

    report = gate.compare_runs(
        control,
        aethyme,
        fixture_id="edge_proxy_backend_auth",
        require_auth_surface_lanes=True,
    )

    assert report["passed"] is True
    lane_check = next(
        check
        for check in report["checks"]
        if check["name"] == "auth_token_surface_flow_lanes_present"
    )
    assert set(lane_check["roles"]) >= {"backend_validator", "ingress_proxy"}


def test_regression_gate_merges_surface_flow_lanes_from_pretty_command_output(
    tmp_path: Path,
) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    explore_output = {
        "subsystems": _surface_flow_subsystems("edge_proxy_backend_auth"),
    }
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "aggregated_output": "projection follows:\n"
                    + json.dumps(explore_output, indent=2)
                    + "\nsource verification follows",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    control = _strict_gate_payload(arm="control")
    aethyme = _strict_gate_payload(arm="aethyme")
    aethyme["structured_output"]["subsystems"] = [
        {"role": "edge credential transport", "top_verification_targets": ["proxy"]},
    ]
    aethyme["regression_metrics"]["surface_flow_lane_roles"] = ["edge credential transport"]
    aethyme["event_log_file"] = str(events_file)

    report = gate.compare_runs(
        control,
        aethyme,
        fixture_id="edge_proxy_backend_auth",
        require_auth_surface_lanes=True,
    )

    assert report["passed"] is True
    lane_check = next(
        check
        for check in report["checks"]
        if check["name"] == "auth_token_surface_flow_lanes_present"
    )
    assert set(lane_check["roles"]) >= {"backend_validator", "ingress_proxy"}


def test_regression_gate_reads_coverage_from_aethyme_command_output(tmp_path: Path) -> None:
    gate = _load_gate()
    events_file = tmp_path / "events.jsonl"
    explore_output = {
        **_surface_flow_observability(["webhook"]),
        "subsystems": _surface_flow_subsystems("edge_proxy_backend_auth"),
    }
    events_file.write_text(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "stdout": json.dumps(explore_output),
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    control = _strict_gate_payload(arm="control")
    aethyme = _strict_gate_payload(arm="aethyme", missing_coverage=[])
    aethyme["structured_output"] = {"answer": "checked command output"}
    aethyme["event_log_file"] = str(events_file)

    report = gate.compare_runs(
        control,
        aethyme,
        control_repeat=control,
        aethyme_repeat=aethyme,
        expected_missing_coverage=["webhook"],
        require_determinism=True,
        require_coverage_report=True,
    )

    assert report["passed"] is True


def test_regression_gate_suite_requires_all_surface_flow_fixture_families() -> None:
    gate = _load_gate()
    complete_runs = []
    for fixture_id in gate.REQUIRED_PLAYGROUND_FIXTURES:
        control = _strict_gate_payload(arm="control", fixture_id=fixture_id)
        aethyme = _strict_gate_payload(arm="aethyme", fixture_id=fixture_id)
        complete_runs.append(
            {
                "fixture_id": fixture_id,
                "control": control,
                "aethyme": aethyme,
                "control_repeat": control,
                "aethyme_repeat": aethyme,
                "control_quality": 4,
                "aethyme_quality": 4,
            }
        )

    passing = gate.compare_suite({"runs": complete_runs})
    assert passing["passed"] is True
    expected_cadence = list(gate.REQUIRED_PLAYGROUND_FIXTURE_ORDER)
    assert passing["fixture_cadence"] == expected_cadence
    assert passing["present_fixtures_in_suite_order"] == expected_cadence

    missing_fixture = gate.compare_suite({"runs": complete_runs[:-1]})
    assert missing_fixture["passed"] is False
    failures = {check["name"] for check in missing_fixture["checks"] if not check["passed"]}
    assert "required_fixture_queue_job_behavior_present" in failures

    out_of_order_runs = list(complete_runs)
    out_of_order_runs[0], out_of_order_runs[1] = (
        out_of_order_runs[1],
        out_of_order_runs[0],
    )
    out_of_order = gate.compare_suite({"runs": out_of_order_runs})
    assert out_of_order["passed"] is False
    order_check = next(
        check for check in out_of_order["checks"] if check["name"] == "fixture_cadence_order"
    )
    assert order_check["actual_order"][:2] == [
        "django_backend_auth",
        "edge_proxy_backend_auth",
    ]
    assert order_check["expected_order_for_present_fixtures"] == expected_cadence
