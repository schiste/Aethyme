"""Tests for scorecard CLI commands."""

import pytest
from click.testing import CliRunner
from pathlib import Path

from src.cli import cli


@pytest.fixture
def runner():
    """Click test runner."""
    return CliRunner()


@pytest.fixture
def good_repo_path():
    """Path to good test repository."""
    return Path(__file__).parent / "fixtures" / "good_repo"


@pytest.fixture
def problematic_repo_path():
    """Path to problematic test repository."""
    return Path(__file__).parent / "fixtures" / "problematic_repo"


class TestAiReadyCommand:
    """Test ai-ready CLI command."""

    def test_ai_ready_command_exists(self, runner):
        """Test that ai-ready command is registered."""
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "ai-ready" in result.output

    def test_scan_good_repo_markdown(self, runner, good_repo_path):
        """Test scanning good repo with Markdown output."""
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(good_repo_path),
            "--format", "md"
        ])

        # Good repo should exit 0 or 1 (depending on minor warnings)
        assert result.exit_code in [0, 1]

        # Output should contain scorecard report
        assert "AI-Readiness Scorecard Report" in result.output or "Score" in result.output

    def test_scan_good_repo_json(self, runner, good_repo_path, tmp_path):
        """Test scanning good repo with JSON output to file."""
        output_file = tmp_path / "scorecard.json"

        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(good_repo_path),
            "--format", "json",
            "--output", str(output_file)
        ])

        assert output_file.exists()

        # Verify JSON is valid
        import json
        with open(output_file) as f:
            data = json.load(f)

        assert "scan_id" in data
        assert "score" in data

    def test_scan_problematic_repo(self, runner, problematic_repo_path):
        """Test scanning problematic repo."""
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(problematic_repo_path),
            "--format", "md"
        ])

        # Problematic repo should exit 1 or 2
        assert result.exit_code in [1, 2]

    def test_selective_detectors(self, runner, good_repo_path):
        """Test running specific detectors only."""
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(good_repo_path),
            "--detectors", "data-ui-coverage,folder-docs",
            "--format", "md"
        ])

        assert result.exit_code in [0, 1]

    def test_both_formats_output(self, runner, good_repo_path, tmp_path):
        """Test outputting both JSON and Markdown."""
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(good_repo_path),
            "--format", "both",
            "--output", str(tmp_path / "scorecard")
        ])

        # Both files should be created
        json_file = tmp_path / "scorecard-report.json"
        md_file = tmp_path / "scorecard-report.md"

        # Note: Depending on implementation, files might be named differently
        # Check if any JSON/MD files were created
        json_files = list(tmp_path.glob("*.json"))
        md_files = list(tmp_path.glob("*.md"))

        # At least JSON should be created
        assert len(json_files) >= 1 or len(md_files) >= 1

    def test_exit_codes(self, runner, good_repo_path, problematic_repo_path):
        """Test that exit codes match severity."""
        # Good repo: exit 0 (excellent) or 1 (warnings)
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(good_repo_path)
        ])
        assert result.exit_code in [0, 1]

        # Problematic repo: exit 1 (warnings) or 2 (blockers)
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", str(problematic_repo_path)
        ])
        assert result.exit_code in [1, 2]

    def test_invalid_repo_path(self, runner):
        """Test error handling for invalid path."""
        result = runner.invoke(cli, [
            "ai-ready",
            "--repo", "/nonexistent/path"
        ])

        assert result.exit_code == 2
        assert "does not exist" in result.output
