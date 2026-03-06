"""Tests for scorecard CLI commands."""

import importlib
import json
from pathlib import Path
from typing import Any, cast

from tests.support.repo_builders import (
    build_good_scorecard_repo,
    build_problematic_scorecard_repo,
)

cli = importlib.import_module("src.cli").cli

def _runner() -> Any:
    click_testing = importlib.import_module("click.testing")
    runner_cls = cast(type[Any], click_testing.CliRunner)
    return runner_cls()


class TestAiReadyCommand:
    def test_ai_ready_command_exists(self) -> None:
        runner = _runner()
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "ai-ready" in result.output

    def test_scan_good_repo_markdown(self, tmp_path: Path) -> None:
        runner = _runner()
        result = runner.invoke(
            cli,
            ["ai-ready", "--repo", str(build_good_scorecard_repo(tmp_path)), "--format", "md"],
        )

        assert result.exit_code in [0, 1]
        assert "AI-Readiness Scorecard Report" in result.output or "Score" in result.output

    def test_scan_good_repo_json(self, tmp_path: Path) -> None:
        runner = _runner()
        output_file = tmp_path / "scorecard.json"
        result = runner.invoke(
            cli,
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--format",
                "json",
                "--output",
                str(output_file),
            ],
        )

        assert result.exit_code in [0, 1]
        assert output_file.exists()
        data = json.loads(output_file.read_text())
        assert "scan_id" in data
        assert "score" in data

    def test_scan_problematic_repo(self, tmp_path: Path) -> None:
        runner = _runner()
        result = runner.invoke(
            cli,
            ["ai-ready", "--repo", str(build_problematic_scorecard_repo(tmp_path)), "--format", "md"],
        )

        assert result.exit_code in [1, 2]

    def test_selective_detectors(self, tmp_path: Path) -> None:
        runner = _runner()
        result = runner.invoke(
            cli,
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--detectors",
                "data-ui-coverage,folder-docs",
                "--format",
                "md",
            ],
        )

        assert result.exit_code in [0, 1]

    def test_both_formats_output(self, tmp_path: Path) -> None:
        runner = _runner()
        result = runner.invoke(
            cli,
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--format",
                "both",
                "--output",
                str(tmp_path / "scorecard"),
            ],
        )

        assert result.exit_code in [0, 1]
        assert list(tmp_path.glob("*.json")) or list(tmp_path.glob("*.md"))

    def test_exit_codes(self, tmp_path: Path) -> None:
        runner = _runner()
        good_result = runner.invoke(
            cli,
            ["ai-ready", "--repo", str(build_good_scorecard_repo(tmp_path))],
        )
        assert good_result.exit_code in [0, 1]

        problematic_result = runner.invoke(
            cli,
            ["ai-ready", "--repo", str(build_problematic_scorecard_repo(tmp_path))],
        )
        assert problematic_result.exit_code in [1, 2]

    def test_invalid_repo_path(self) -> None:
        runner = _runner()
        result = runner.invoke(cli, ["ai-ready", "--repo", "/nonexistent/path"])

        assert result.exit_code == 2
        assert "does not exist" in result.output
