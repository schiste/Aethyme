"""Focused tests for engine caching and evaluation runners."""

from __future__ import annotations

import json
import subprocess
import sys
import types
from pathlib import Path

from src.eval.control_prompt import build_aethyme_prompt
from src.eval.explain_repo import command_runner, run_explain_repo_evaluation
from src.eval.navigation_ctf import run_navigation_ctf_evaluation
from src.eval.runner import _resolve_command
from src.indexing import engine as engine_module
from src.indexing.repository_snapshot import capture_snapshot


def build_demo_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "src").mkdir(exist_ok=True)
    (root / "README.md").write_text("# Demo Repo\n", encoding="utf-8")
    (root / "src" / "main.py").write_text("def main():\n    return 1\n", encoding="utf-8")


def test_engine_inspect_uses_snapshot_cache(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    cache_root = tmp_path / "cache"
    cache_root.mkdir()
    fake_binary = tmp_path / "aethyme-engine-cli"
    fake_binary.write_text("", encoding="utf-8")

    calls: list[tuple[str, ...]] = []

    monkeypatch.setattr(engine_module, "CACHE_ROOT", cache_root)
    monkeypatch.setattr(engine_module, "ensure_engine_binary", lambda: fake_binary)

    def fake_run(*args: str) -> str:
        calls.append(args)
        return json.dumps(
            {
                "snapshot": {
                    "root": str(repo_path),
                    "languages": ["python"],
                    "top_level_dirs": ["src"],
                    "readme_path": "README.md",
                    "files": [{"path": "README.md", "language": None, "line_count": 1, "size_bytes": 12}],
                },
                "symbols": [],
                "edges": [],
                "risk_flags": [],
            }
        )

    monkeypatch.setattr(engine_module, "_run_binary_command", fake_run)

    first = engine_module.inspect_repository(repo_path)
    second = engine_module.inspect_repository(repo_path)

    assert first == second
    assert len(calls) == 1


def test_engine_command_rejects_unknown_transport(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "unknown")

    try:
        engine_module._run_binary_command("inspect", "--repo", "/tmp/repo")
    except engine_module.EngineError as exc:
        assert "Unsupported engine transport" in str(exc)
    else:
        raise AssertionError("Expected EngineError for unsupported engine transport")


def test_engine_runtime_info_reflects_configured_transport(monkeypatch, tmp_path: Path) -> None:
    fake_binary = tmp_path / "aethyme-engine-cli"
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "subprocess")
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", fake_binary)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "subprocess"
    assert info["transport_source"] == "env"
    assert info["transport_supported"] is True
    assert info["binary_exists"] is False
    assert info["transport_ready"] is False
    assert info["transport_detail"] == "binary not built"
    assert info["resolved_transport"] == "subprocess"
    assert "auto" in info["supported_transports"]
    assert "subprocess" in info["supported_transports"]
    assert "pyo3" in info["supported_transports"]


def test_register_engine_transport_runner(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "fake")

    engine_module.register_engine_transport("fake", lambda args: json.dumps({"args": list(args)}))
    try:
        payload = json.loads(engine_module._run_binary_command("inspect", "--repo", "/tmp/repo"))
    finally:
        engine_module.ENGINE_TRANSPORT_RUNNERS.pop("fake", None)

    assert payload["args"] == ["inspect", "--repo", "/tmp/repo"]


def test_register_engine_transport_normalizes_name(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "custom")
    engine_module.register_engine_transport("  CUSTOM  ", lambda _args: "{}")
    try:
        assert "custom" in engine_module.ENGINE_TRANSPORT_RUNNERS
        output = engine_module._run_binary_command("inspect")
    finally:
        engine_module.ENGINE_TRANSPORT_RUNNERS.pop("custom", None)
    assert output == "{}"


def test_register_engine_transport_prevents_accidental_override() -> None:
    try:
        engine_module.register_engine_transport("subprocess", lambda _args: "{}")
    except ValueError as exc:
        assert "already registered" in str(exc)
    else:
        raise AssertionError("Expected ValueError for duplicate transport registration")


def test_engine_command_dispatches_to_pyo3_transport(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    fake_module = types.SimpleNamespace(
        run_engine_command=lambda args: json.dumps({"transport": "pyo3", "args": args})
    )
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    payload = json.loads(engine_module._run_binary_command("inspect", "--repo", "/tmp/repo"))
    assert payload["transport"] == "pyo3"
    assert payload["args"] == ["inspect", "--repo", "/tmp/repo"]


def test_engine_runtime_info_reports_pyo3_readiness(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    fake_module = types.SimpleNamespace(run_engine_command=lambda _args: "{}")
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "pyo3"
    assert info["resolved_transport"] == "pyo3"
    assert info["transport_supported"] is True
    assert info["transport_ready"] is True
    assert info["transport_detail"] == "ready"


def test_engine_runtime_info_auto_prefers_pyo3_when_available(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "auto")
    fake_module = types.SimpleNamespace(run_engine_command=lambda _args: "{}")
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["resolved_transport"] == "pyo3"
    assert info["transport_ready"] is True
    assert "auto->pyo3" in info["transport_detail"]


def test_engine_runtime_info_auto_falls_back_to_subprocess(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "auto")
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_ready"] is False
    assert "auto->subprocess" in info["transport_detail"]


def test_engine_runtime_info_defaults_to_auto(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.delenv("AETHYME_ENGINE_TRANSPORT", raising=False)
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["transport_source"] == "default"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_ready"] is False
    assert info["transport_supported"] is True


def test_engine_runtime_info_blank_env_defaults_to_auto(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "   ")
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["transport_source"] == "default"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_supported"] is True


def test_engine_command_pyo3_transport_requires_callable(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    monkeypatch.setitem(sys.modules, "aethyme_py", types.SimpleNamespace())

    try:
        engine_module._run_binary_command("inspect")
    except engine_module.EngineError as exc:
        assert "run_engine_command" in str(exc)
    else:
        raise AssertionError("Expected EngineError when PyO3 callable is missing")


def test_explain_repo_generates_artifacts_and_invokes_backends(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    runner_script = tmp_path / "runner.py"
    runner_script.write_text(
        "import json, os\n"
        "from pathlib import Path\n"
        "prompt = os.environ['AETHYME_EVAL_PROMPT']\n"
        "label = os.environ['AETHYME_EVAL_LABEL']\n"
        "schema = json.loads(Path(os.environ['AETHYME_EVAL_OUTPUT_SCHEMA_FILE']).read_text())\n"
        "print(json.dumps({\n"
        "  'label': label,\n"
        "  'input_tokens': len(prompt.split()),\n"
        "  'output_tokens': 7,\n"
        "  'retries': 1 if label == 'control' else 0,\n"
        "  'review_burden': 2,\n"
        "  'final_output_message': f'{label} final output',\n"
        "  'structured_output': {\n"
        "    'label': label,\n"
        "    'ok': True,\n"
        "    'schema_type': schema['type'],\n"
        "    'tool_repo': os.environ['AETHYME_EVAL_TOOL_REPO'],\n"
        "    'tool_python': os.environ['AETHYME_EVAL_TOOL_PYTHON']\n"
        "  }\n"
        "}))\n",
        encoding="utf-8",
    )

    monkeypatch.setattr(
        "src.eval.explain_repo.build_task_pack",
        lambda repo, task: {
            "task": {"raw": task, "kind": "explain_repo"},
            "summary": {
                "snapshot": {
                    "languages": ["python"],
                    "top_level_dirs": ["src"],
                    "readme_path": "README.md",
                },
                "files_count": 2,
                "functions_count": 1,
                "classes_count": 0,
                "docs_count": 1,
                "configs_count": 0,
            },
            "signals": {
                "parser_visibility": {
                    "score": 90,
                    "level": "strong",
                    "evidence": ["supported source files: 1/1"],
                }
            },
            "overview": {
                "overview_docs": ["README.md"],
                "code_areas": ["src"],
                "reference_areas": [],
                "subareas": [],
                "entrypoints": ["src/main.py"],
                "key_configs": [],
                "representative_code_files": ["src/main.py"],
                "representative_docs": ["README.md"],
            },
            "navigation_order": ["README.md"],
            "risk_flags": [],
        },
    )
    monkeypatch.setattr(
        "src.eval.explain_repo.write_explain_repo_markdown_report",
        lambda **kwargs: tmp_path / "report.md",
    )

    baseline = command_runner(f"{sys.executable} {runner_script}", working_directory=repo_path)
    aethyme = command_runner(f"{sys.executable} {runner_script}", working_directory=repo_path)
    result = run_explain_repo_evaluation(
        repo_path,
        baseline_runner=baseline,
        aethyme_runner=aethyme,
    )

    assert result["baseline_run"] is not None
    assert result["aethyme_run"] is not None
    assert result["baseline_run"]["input_tokens"] is not None
    assert result["baseline_run"]["final_output_message"] == "control final output"
    assert result["aethyme_run"]["structured_output"]["label"] == "leverage"
    assert result["aethyme_run"]["structured_output"]["ok"] is True
    assert result["aethyme_run"]["structured_output"]["schema_type"] == "object"
    assert result["aethyme_run"]["structured_output"]["tool_repo"].endswith("packages/aethyme")
    assert result["aethyme_run"]["structured_output"]["tool_python"].endswith(".venv/bin/python")
    assert result["report"]["condition_runs"]["control"]["retries"] == 1
    assert result["report"]["condition_runs"]["leverage"]["retries"] == 0
    assert result["report_path"].endswith("report.md")
    assert result["signals"]["parser_visibility"]["score"] == 90
    assert result["output_schema"]["type"] == "object"
    assert result["reference_output"]["navigation_order"][0] == "README.md"
    assert result["reference_output"]["code_areas"] == ["src"]
    assert result["reference_output"]["navigation_order"]


def test_aethyme_prompt_uses_compact_pack_rendering() -> None:
    pack = {
        "task": {"raw": "Explain this repo", "kind": "explain_repo"},
        "overview": {
            "overview_docs": ["README.md", "documentation/technical-architecture.md"],
            "code_areas": ["GameEngine", "tools"],
            "reference_areas": ["documentation"],
            "entrypoints": ["src/main.py"],
            "representative_code_files": ["src/main.py"],
            "representative_docs": ["README.md"],
        },
        "navigation_order": ["README.md", "GameEngine", "tools"],
        "risk_flags": [],
    }

    prompt = build_aethyme_prompt(Path("/tmp/repo"), "Explain this repo", pack)

    assert "Repository path:" not in prompt
    assert "Use Aethyme pack only." in prompt
    assert "Overview:" in prompt
    assert "Order:" in prompt


def test_write_explain_repo_report_includes_verbose_results(tmp_path: Path) -> None:
    from src.eval.report import write_explain_repo_markdown_report

    control_run = {"exit_code": 0, "duration_seconds": 0.1, "input_tokens": 10, "output_tokens": 20, "retries": 1, "review_burden": 2, "stdout": "base", "stderr": "", "command": "control", "final_output_message": "control final output", "structured_output": {"result": "control"}, "tool_calls": None}
    leverage_run = {"exit_code": 0, "duration_seconds": 0.2, "input_tokens": 8, "output_tokens": 18, "retries": 0, "review_burden": 1, "stdout": "assist", "stderr": "", "command": "leverage", "final_output_message": "leverage final output", "structured_output": {"result": "leverage"}, "tool_calls": [{"tool": "shell", "input_summary": "cat README.md"}, {"tool": "shell", "input_summary": "ls src/"}]}
    result = {
        "task": "Explain this repo",
        "pack": {"task": {"raw": "Explain this repo"}},
        "explanation": "repo explanation",
        "signals": {"parser_visibility": {"score": 80, "level": "strong", "evidence": ["supported source files: 10/10"]}},
        "control": {"prompt": "control prompt", "run": control_run, "assessment": None},
        "leverage": {"prompt": "leverage prompt", "run": leverage_run, "assessment": None},
        # Legacy keys
        "baseline_prompt": "control prompt",
        "aethyme_prompt": "leverage prompt",
        "baseline_run": control_run,
        "aethyme_run": leverage_run,
        "report": {
            "condition_prompt_chars": {"control": 14, "leverage": 15},
            "navigation_items": 1,
            "risk_items": 0,
            "condition_runs": {"control": control_run, "leverage": leverage_run},
            "baseline_prompt_chars": 14,
            "aethyme_prompt_chars": 15,
            "baseline_run": control_run,
            "aethyme_run": leverage_run,
        },
    }

    monkeypatch_report_root = tmp_path / "reports"
    from src.eval import report as report_module
    original_root = report_module.REPORTS_ROOT
    report_module.REPORTS_ROOT = monkeypatch_report_root
    try:
        report_path = write_explain_repo_markdown_report(repo_path=tmp_path / "repo", result=result)
    finally:
        report_module.REPORTS_ROOT = original_root

    content = report_path.read_text(encoding="utf-8")
    assert "## Control" in content
    assert "## Leverage" in content
    assert "## Repo Signals" in content
    assert "control final output" in content
    assert "leverage final output" in content
    assert "\"parser_visibility\"" in content
    assert "\"result\": \"control\"" in content
    assert "\"result\": \"leverage\"" in content
    # Diagnostic sections
    assert "## Tool Call Analysis" in content
    assert "## Context Pack Audit" in content
    assert "## Graph Quality Notes" in content
    assert "## Prompt Effectiveness" in content
    assert "## Lessons & Action Items" in content


def test_render_markdown_skips_legacy_diagnostics_when_not_applicable() -> None:
    from src.eval.report import _render_markdown

    result = {
        "task": "Bug fix task",
        "eval_type": "bug-fix",
        "control": {"prompt": "control", "run": {"structured_output": {"ok": True}}, "assessment": None},
        "leverage": {"prompt": "leverage", "run": {"structured_output": {"ok": True}}, "assessment": None},
    }

    content = _render_markdown(repo_path=Path("/tmp/repo"), result=result)

    assert "## Context Pack Audit" not in content
    assert "## Graph Quality Notes" not in content
    assert "## Prompt Effectiveness" not in content
    assert "## Lessons & Action Items" not in content


def test_capture_snapshot_uses_git_commit_when_clean(tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    subprocess.run(["git", "init"], cwd=repo_path, check=True, capture_output=True, text=True)
    subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=repo_path, check=True)
    subprocess.run(["git", "config", "user.name", "Test User"], cwd=repo_path, check=True)
    subprocess.run(["git", "add", "."], cwd=repo_path, check=True, capture_output=True, text=True)
    subprocess.run(["git", "commit", "-m", "initial"], cwd=repo_path, check=True, capture_output=True, text=True)

    snapshot = capture_snapshot(repo_path)

    assert snapshot.commit is not None
    assert snapshot.dirty is False
    assert snapshot.cache_key == snapshot.commit


def test_resolve_command_rewrites_project_relative_script_paths(tmp_path: Path) -> None:
    script_dir = tmp_path / "scripts" / "eval"
    script_dir.mkdir(parents=True)
    script_path = script_dir / "runner.py"
    script_path.write_text("print('ok')\n", encoding="utf-8")

    original_project_root = sys.modules["src.eval.runner"].PROJECT_ROOT
    sys.modules["src.eval.runner"].PROJECT_ROOT = tmp_path
    try:
        resolved = _resolve_command(".venv/bin/python scripts/eval/runner.py")
    finally:
        sys.modules["src.eval.runner"].PROJECT_ROOT = original_project_root

    assert resolved[1] == str(script_path)


def test_navigation_ctf_builds_reference_output(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    monkeypatch.setattr(
        "src.eval.navigation_ctf.inspect_repository",
        lambda repo: {
            "snapshot": {"top_level_dirs": ["src"]},
            "configs": [
                {
                    "id": "config:demo:pyproject.toml",
                    "path": "pyproject.toml",
                    "area_id": "area:demo:src",
                }
            ],
            "areas": [{"id": "area:demo:src", "name": "src"}],
            "files": [{"id": "file:demo:src/main.py", "path": "src/main.py"}],
            "edges": [
                {
                    "from": "config:demo:pyproject.toml",
                    "to": "file:demo:src/main.py",
                    "kind": "entrypoint_for",
                },
                {
                    "from": "config:demo:pyproject.toml",
                    "to": "area:demo:src",
                    "kind": "configures",
                },
            ],
            "docs": [],
        },
    )
    monkeypatch.setattr(
        "src.eval.navigation_ctf.build_task_pack",
        lambda repo, task: {
            "task": {"raw": task, "kind": "change_symbol"},
            "anchors": [],
            "navigation_order": ["pyproject.toml", "src/main.py"],
            "risk_flags": [],
        },
    )
    monkeypatch.setattr(
        "src.eval.navigation_ctf.write_navigation_ctf_markdown_report",
        lambda **kwargs: tmp_path / "navigation.md",
    )

    result = run_navigation_ctf_evaluation(repo_path)

    assert result["challenge"]["kind"] == "navigation_ctf"
    assert result["reference_output"]["config_target"]["path"] == "pyproject.toml"
    assert result["reference_output"]["code_target"]["path"] == "src/main.py"
    assert result["reference_output"]["management_area"]["name"] == "src"
    assert result["navigation_context"]["tool_repo_path"].endswith("packages/aethyme")
    assert result["navigation_context"]["tool_python"].endswith(".venv/bin/python")
    assert ".venv/bin/python -m src.cli" in result["navigation_context"]["commands"][0]
    assert f"task anchors --repo {repo_path} --task <task> --json-output" in result["navigation_context"]["commands"][0]
    assert result["report_path"].endswith("navigation.md")
