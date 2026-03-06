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
    assert inspect_payload["symbols"]

    pack_result = runner.invoke(
        cli,
        ["task", "pack", "--repo", str(repo_path), "--task", "Explain this repo", "--json-output"],
    )
    assert pack_result.exit_code == 0, pack_result.output
    pack_payload = json.loads(pack_result.output)
    assert pack_payload["task"]["kind"] == "explain_repo"
    assert pack_payload["anchors"]
    assert "README.md" in pack_payload["navigation_order"]


def test_local_eval_explain_repo(tmp_path: Path) -> None:
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)
    runner = CliRunner()

    result = runner.invoke(
        cli,
        ["eval", "explain-repo", "--repo", str(repo_path), "--json-output"],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["task"] == "Explain this repo"
    assert payload["report"]["baseline_prompt_chars"] > 0
    assert payload["report"]["aethyme_prompt_chars"] > payload["report"]["baseline_prompt_chars"]
    assert "Navigation order:" in payload["explanation"]
