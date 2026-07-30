"""Implementation-blind CLI tests for `aethyme ai-ready` (Phase 4).

Replaces tests/scorecard/test_cli.py after the native flip: the same
assertions run against the router binary as a subprocess, so they hold
for whichever implementation answers (aethyme-quality since the Phase 4
flip). Detector/engine/formatter behavior is covered by the Rust unit
tests in rust/crates/aethyme-quality and the parity corpus that gated
the flip.
"""

from __future__ import annotations

import json
from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme
from tests.support.repo_builders import (
    build_good_scorecard_repo,
    build_problematic_scorecard_repo,
)


class TestAiReadyCommand:
    def test_scan_good_repo_markdown(self, tmp_path: Path) -> None:
        result = invoke_aethyme(
            ["ai-ready", "--repo", str(build_good_scorecard_repo(tmp_path)), "--format", "md"]
        )
        assert result.exit_code in [0, 1]
        assert "AI-Readiness Scorecard Report" in result.output or "Score" in result.output

    def test_scan_good_repo_json(self, tmp_path: Path) -> None:
        output_file = tmp_path / "scorecard.json"
        result = invoke_aethyme(
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--format",
                "json",
                "--output",
                str(output_file),
            ]
        )
        assert result.exit_code in [0, 1]
        assert output_file.exists()
        data = json.loads(output_file.read_text())
        assert "scan_id" in data
        assert "score" in data

    def test_scan_problematic_repo(self, tmp_path: Path) -> None:
        result = invoke_aethyme(
            [
                "ai-ready",
                "--repo",
                str(build_problematic_scorecard_repo(tmp_path)),
                "--format",
                "md",
            ]
        )
        assert result.exit_code in [1, 2]

    def test_selective_detectors(self, tmp_path: Path) -> None:
        result = invoke_aethyme(
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--detectors",
                "data-ui-coverage,folder-docs",
                "--format",
                "md",
            ]
        )
        assert result.exit_code in [0, 1]

    def test_both_formats_output(self, tmp_path: Path) -> None:
        result = invoke_aethyme(
            [
                "ai-ready",
                "--repo",
                str(build_good_scorecard_repo(tmp_path)),
                "--format",
                "both",
                "--output",
                str(tmp_path / "scorecard"),
            ]
        )
        assert result.exit_code in [0, 1]
        assert list(tmp_path.glob("*.json")) or list(tmp_path.glob("*.md"))

    def test_exit_codes(self, tmp_path: Path) -> None:
        good_result = invoke_aethyme(
            ["ai-ready", "--repo", str(build_good_scorecard_repo(tmp_path))]
        )
        assert good_result.exit_code in [0, 1]

        problematic_result = invoke_aethyme(
            ["ai-ready", "--repo", str(build_problematic_scorecard_repo(tmp_path))]
        )
        assert problematic_result.exit_code in [1, 2]

    def test_invalid_repo_path(self) -> None:
        result = invoke_aethyme(["ai-ready", "--repo", "/nonexistent/path"])
        assert result.exit_code == 2
        assert "does not exist" in result.output

    def test_help_names_the_options(self) -> None:
        result = invoke_aethyme(["ai-ready", "--help"])
        assert result.exit_code == 0
        for flag in ["--repo", "--repo-id", "--format", "--output", "--detectors"]:
            assert flag in result.output

    def test_report_shape_on_problematic_repo(self, tmp_path: Path) -> None:
        """The parts of the report contract the corpus goldens froze."""
        result = invoke_aethyme(
            [
                "ai-ready",
                "--repo",
                str(build_problematic_scorecard_repo(tmp_path)),
                "--format",
                "json",
            ]
        )
        assert result.exit_code == 2  # blocker present
        payload = result.output[result.output.index("{") :]
        payload = payload[: payload.rindex("}") + 1]
        data = json.loads(payload)
        assert data["score"] == 24
        assert data["summary"] == {
            "total_findings": 13,
            "blockers": 1,
            "warnings": 11,
            "info": 1,
        }
        assert [d["name"] for d in data["detectors"]] == [
            "data-ui-coverage",
            "folder-docs",
            "relative-links",
            "i18n-gaps",
            "generated-files",
            "schema-drift",
            "route-coverage",
            "ability-coverage",
        ]
