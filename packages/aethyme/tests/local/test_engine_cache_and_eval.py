"""Focused tests for engine caching and evaluation runners."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from src.eval.control_prompt import build_aethyme_prompt
from src.eval.explain_repo import command_runner, run_explain_repo_evaluation
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


def test_explain_repo_evaluation_runs_command_runners(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    runner_script = tmp_path / "runner.py"
    runner_script.write_text(
        "import json, os\n"
        "prompt = os.environ['AETHYME_EVAL_PROMPT']\n"
        "label = os.environ['AETHYME_EVAL_LABEL']\n"
        "print(json.dumps({\n"
        "  'label': label,\n"
        "  'input_tokens': len(prompt.split()),\n"
        "  'output_tokens': 7,\n"
        "  'retries': 1 if label == 'baseline' else 0,\n"
        "  'review_burden': 2\n"
        "}))\n",
        encoding="utf-8",
    )

    monkeypatch.setattr(
        "src.eval.explain_repo.build_task_pack",
        lambda repo, task: {
            "task": {"raw": task, "kind": "explain_repo"},
            "navigation_order": ["README.md"],
            "risk_flags": [],
        },
    )
    monkeypatch.setattr(
        "src.eval.explain_repo.explain_task",
        lambda repo, task: "Task: Explain this repo\nNavigation order:\n- README.md",
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
    assert result["report"]["baseline_run"]["retries"] == 1
    assert result["report"]["aethyme_run"]["retries"] == 0


def test_aethyme_prompt_uses_compact_pack_rendering() -> None:
    pack = {
        "task": {"raw": "Explain this repo"},
        "anchors": [
            {"id": "README.md", "file": "README.md", "reason": "repository readme"},
        ],
        "in_scope": {"files": [{"value": "README.md"}]},
        "out_of_scope": {"areas": [{"value": "migrations/", "reason": "high-risk area"}]},
        "dependencies": [{"from": "src/main.py", "to": "auth"}],
        "snippets": [{"file": "README.md", "start_line": 1, "end_line": 10, "kind": "overview"}],
        "risk_flags": [{"scope": "src/auth.py", "area": "auth"}],
        "navigation_order": ["README.md"],
    }

    prompt = build_aethyme_prompt(Path("/tmp/repo"), "Explain this repo", pack)

    assert "Pack: {" not in prompt
    assert "Start:" in prompt
    assert "Avoid:" in prompt


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
