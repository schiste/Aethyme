"""Local-first CLI tests for the Rust-backed repository workflow."""

from __future__ import annotations

import json
from pathlib import Path

from click.testing import CliRunner

from src.cli import cli


def build_demo_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "src").mkdir(exist_ok=True)
    (root / "README.md").write_text("# Demo Repo\n\nSmall test repo.\n", encoding="utf-8")
    (root / "src" / "main.py").write_text(
        "from auth import validate_token\n\n"
        "def main():\n"
        "    return validate_token()\n",
        encoding="utf-8",
    )
    (root / "src" / "auth.py").write_text(
        "def validate_token():\n"
        "    return True\n",
        encoding="utf-8",
    )


def test_local_repo_inspect_and_pack(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)
    runner = CliRunner()

    inspect_result = runner.invoke(
        cli,
        ["repo", "inspect", str(repo_path), "--json-output"],
    )
    assert inspect_result.exit_code == 0, inspect_result.output
    inspect_payload = json.loads(inspect_result.output)
    assert inspect_payload["snapshot"]["readme_path"] == "README.md"
    assert "signals" in inspect_payload
    assert inspect_payload["signals"]["boundary_clarity"]["score"] >= 0
    assert inspect_payload["symbols"]
    assert inspect_payload["areas"]
    assert inspect_payload["files"]
    assert inspect_payload["graph"]["nodes"]

    pack_result = runner.invoke(
        cli,
        ["task", "pack", "--repo", str(repo_path), "--task", "Explain this repo", "--json-output"],
    )
    assert pack_result.exit_code == 0, pack_result.output
    pack_payload = json.loads(pack_result.output)
    assert pack_payload["task"]["kind"] == "explain_repo"
    assert pack_payload["anchors"]
    assert "README.md" in pack_payload["navigation_order"]
    assert pack_payload["in_scope"]["areas"]


def test_local_eval_explain_repo(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)
    runner = CliRunner()
    monkeypatch.setattr(
        "src.eval.explain_repo.write_explain_repo_markdown_report",
        lambda **kwargs: tmp_path / "report.md",
    )

    result = runner.invoke(
        cli,
        ["eval", "explain-repo", "--repo", str(repo_path), "--json-output"],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["task"] == "Explain this repo"
    assert "signals" in payload
    assert payload["signals"]["parser_visibility"]["score"] >= 0
    assert payload["report"]["baseline_prompt_chars"] > 0
    assert payload["report"]["aethyme_prompt_chars"] < payload["report"]["baseline_prompt_chars"]
    assert payload["report_path"].endswith(".md")
    assert payload["output_schema"]["type"] == "object"
    assert payload["reference_output"]["code_areas"]
    assert "representative_code_files" in payload["reference_output"]
    assert "Navigation order:" in payload["explanation"]


def test_local_graph_navigation_commands(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)
    runner = CliRunner()

    node_result = runner.invoke(
        cli,
        ["graph", "node", str(repo_path), "src/main.py", "--json-output"],
    )
    assert node_result.exit_code == 0, node_result.output
    node_payload = json.loads(node_result.output)
    assert node_payload["kind"] == "file"

    children_result = runner.invoke(
        cli,
        ["graph", "children", str(repo_path), "src", "--json-output"],
    )
    assert children_result.exit_code == 0, children_result.output
    children_payload = json.loads(children_result.output)
    assert children_payload["items"]

    overview_result = runner.invoke(
        cli,
        ["graph", "overview", str(repo_path), "--json-output"],
    )
    assert overview_result.exit_code == 0, overview_result.output
    overview_payload = json.loads(overview_result.output)
    assert "signals" in overview_payload
    assert overview_payload["signals"]["parser_visibility"]["score"] >= 0


def test_local_task_navigation_commands(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)
    runner = CliRunner()

    anchors_result = runner.invoke(
        cli,
        ["task", "anchors", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"],
    )
    assert anchors_result.exit_code == 0, anchors_result.output
    anchors_payload = json.loads(anchors_result.output)
    assert anchors_payload["anchors"]

    scope_result = runner.invoke(
        cli,
        ["task", "scope", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"],
    )
    assert scope_result.exit_code == 0, scope_result.output
    scope_payload = json.loads(scope_result.output)
    assert scope_payload["in_scope_files"]
    assert "src/auth.py" in scope_payload["in_scope_files"]
    assert "src/main.py" in scope_payload["in_scope_files"]
    assert any(symbol.startswith("src/auth.py::") for symbol in scope_payload["in_scope_symbols"])

    next_result = runner.invoke(
        cli,
        ["task", "next", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"],
    )
    assert next_result.exit_code == 0, next_result.output
    next_payload = json.loads(next_result.output)
    assert next_payload["items"]
    next_displays = [item["display"] for item in next_payload["items"]]
    assert "src/auth.py" in next_displays
    assert "src/main.py" in next_displays

    expand_result = runner.invoke(
        cli,
        ["task", "expand", "--repo", str(repo_path), "--node", "src/auth.py", "--json-output"],
    )
    assert expand_result.exit_code == 0, expand_result.output
    expand_payload = json.loads(expand_result.output)
    assert "dependencies" in expand_payload
