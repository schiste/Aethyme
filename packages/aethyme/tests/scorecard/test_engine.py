"""Tests for scorecard engine."""

import json
from pathlib import Path

from src.scorecard.engine import ScanResult, ScorecardEngine
from tests.support.repo_builders import (
    build_good_scorecard_repo,
    build_problematic_scorecard_repo,
)


class TestScorecardEngine:
    """Test scorecard scanning engine."""

    def test_engine_initialization(self, tmp_path: Path) -> None:
        repo_path = build_good_scorecard_repo(tmp_path)
        engine = ScorecardEngine(repo_path)
        assert engine.repo_path == repo_path

    def test_engine_rejects_invalid_path(self) -> None:
        try:
            ScorecardEngine(Path("/nonexistent/path"))
            raise AssertionError("Expected ValueError for nonexistent path")
        except ValueError as exc:
            assert "does not exist" in str(exc)

    def test_full_scan_problematic_repo(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_problematic_scorecard_repo(tmp_path))
        result = engine.scan()

        assert isinstance(result, ScanResult)
        assert result.report is not None
        assert result.report.total_findings > 0
        assert 0 <= result.score <= 100
        assert len(result.report.detector_results) == 8
        assert result.report.blocker_count > 0 or result.report.warning_count > 0

    def test_full_scan_good_repo(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()

        assert isinstance(result, ScanResult)
        assert result.score >= 85
        assert result.report.blocker_count == 0
        assert len(result.report.detector_results) == 8

    def test_selective_detector_run(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_problematic_scorecard_repo(tmp_path))
        result = engine.scan(detectors=["data-ui-coverage", "relative-links"])

        assert len(result.report.detector_results) == 2
        detector_names = [dr.detector_name for dr in result.report.detector_results]
        assert "data-ui-coverage" in detector_names
        assert "relative-links" in detector_names

    def test_scan_performance_metrics(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()

        assert result.report.total_scan_time_ms > 0
        assert result.report.files_scanned > 0
        for detector_result in result.report.detector_results:
            assert detector_result.execution_time_ms >= 0

    def test_score_calculation(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_problematic_scorecard_repo(tmp_path))
        result = engine.scan()
        report = result.report

        if report.blocker_count > 0:
            assert report.score < 80

        if report.blocker_count == 0 and report.warning_count == 0:
            assert report.score >= 90

    def test_json_export(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()
        json_output = result.to_json()

        assert isinstance(json_output, str)

        data = json.loads(json_output)
        assert "scan_id" in data
        assert "score" in data
        assert "summary" in data
        assert "findings" in data

    def test_markdown_export(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()
        md_output = result.to_markdown()

        assert isinstance(md_output, str)
        assert "# AI-Readiness Scorecard Report" in md_output
        assert "## Overall Score" in md_output
        assert "## Summary" in md_output

    def test_finding_aggregation(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_problematic_scorecard_repo(tmp_path))
        result = engine.scan()
        report = result.report

        assert len(report.blockers) == report.blocker_count
        assert len(report.warnings) == report.warning_count
        assert len(report.info) == report.info_count
        assert report.blocker_count + report.warning_count + report.info_count == report.total_findings

    def test_detector_error_handling(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()

        for detector_result in result.report.detector_results:
            if detector_result.error:
                assert isinstance(detector_result.error, str)

    def test_tenant_isolation(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(
            build_good_scorecard_repo(tmp_path),
            repository_id="test-repo-123",
            tenant_id="test-tenant-456",
        )
        result = engine.scan()

        assert result.report.repository_id == "test-repo-123"
        assert result.report.tenant_id == "test-tenant-456"


class TestScanResult:
    """Test ScanResult class."""

    def test_scan_result_creation(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()

        assert result.scan_id is not None
        assert result.score >= 0
        assert result.report is not None

    def test_multiple_export_formats(self, tmp_path: Path) -> None:
        engine = ScorecardEngine(build_good_scorecard_repo(tmp_path))
        result = engine.scan()

        json_output = result.to_json()
        md_output = result.to_markdown()

        assert json_output != md_output
        assert len(json_output) > 0
        assert len(md_output) > 0
