"""CLI/rendering regression tests for completeness and confidence signal surfacing."""

from __future__ import annotations

from pathlib import Path

from click.testing import CliRunner

from src.cli import cli
from src.rendering.context_pack import render_pack_summary, render_prompt_pack


def test_graph_node_non_json_surfaces_completeness_signals(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.graph_node",
        lambda _repo, _target: {
            "id": "fn:demo:main",
            "kind": "function",
            "label": "main",
            "confidence": 920,
            "truncated": True,
            "reason": "result cap reached",
            "caps": {"max_items": 50},
        },
    )

    runner = CliRunner()
    result = runner.invoke(cli, ["graph", "node", str(repo_path), "main"])

    assert result.exit_code == 0, result.output
    assert "Confidence: 920" in result.output
    assert "Truncated: yes" in result.output
    assert "Truncation reason: result cap reached" in result.output
    assert 'Caps: {"max_items": 50}' in result.output


def test_task_scope_non_json_renders_reason_fields(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.task_scope",
        lambda _repo, _task: {
            "task": "Update auth flow",
            "in_scope_files": [
                {"value": "src/auth.py", "reason": "anchor file"},
            ],
            "in_scope_areas": [
                {"value": "src", "reason": "contains anchor"},
            ],
            "out_of_scope": [
                {"value": "docs", "reason": "non-runtime"},
            ],
            "risks": ["auth regression"],
        },
    )

    runner = CliRunner()
    result = runner.invoke(
        cli,
        ["task", "scope", "--repo", str(repo_path), "--task", "Update auth flow"],
    )

    assert result.exit_code == 0, result.output
    assert "- src/auth.py (anchor file)" in result.output
    assert "- src (contains anchor)" in result.output
    assert "- docs (non-runtime)" in result.output


def test_render_pack_summary_includes_confidence_caps_and_truncation() -> None:
    summary = render_pack_summary(
        {
            "task": {"raw": "Explain this repo"},
            "confidence": {"anchor_confidence": 0.91, "scope_confidence": 0.73},
            "budget": {
                "max_anchors": 3,
                "max_files": 5,
                "max_snippets": 8,
                "dependency_depth": 2,
                "impact_depth": 3,
                "content_budget": 4096,
                "max_content_files": 2,
                "max_lines_per_file": 120,
            },
            "file_contents": [
                {"path": "src/main.py", "end_line": 120, "total_lines": 280},
                {"path": "src/auth.py", "end_line": 40, "total_lines": 40},
            ],
        }
    )

    assert "Confidence: anchor=0.91, scope=0.73" in summary
    assert "Caps:" in summary
    assert "max_anchors=3" in summary
    assert "Truncated content:" in summary
    assert "src/main.py (120/280 lines)" in summary


def test_render_prompt_pack_handles_scope_items_without_value_key() -> None:
    prompt = render_prompt_pack(
        {
            "task": {"kind": "change_task"},
            "in_scope": {
                "files": [{"value": "src/main.py"}, {"bad": "skip"}],
                "areas": ["src"],
            },
            "out_of_scope": {
                "areas": [{"value": "docs"}, {"bad": "skip"}],
            },
            "navigation_order": ["src/main.py", "src/auth.py"],
        }
    )

    assert "Scope: src/main.py" in prompt
    assert "Avoid: docs" in prompt
