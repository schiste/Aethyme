"""Tests for scorecard engine."""

import pytest
from pathlib import Path

from src.scorecard.engine import ScorecardEngine, ScanResult
from src.scorecard.models import Severity


@pytest.fixture
def good_repo_path():
    """Path to good test repository."""
    return Path(__file__).parent / "fixtures" / "good_repo"


@pytest.fixture
def problematic_repo_path():
    """Path to problematic test repository."""
    return Path(__file__).parent / "fixtures" / "problematic_repo"


class TestScorecardEngine:
    """Test scorecard scanning engine."""

    def test_engine_initialization(self, good_repo_path):
        """Test engine can be initialized."""
        engine = ScorecardEngine(good_repo_path)
        assert engine.repo_path == good_repo_path

    def test_engine_rejects_invalid_path(self):
        """Test engine rejects non-existent paths."""
        with pytest.raises(ValueError, match="does not exist"):
            ScorecardEngine(Path("/nonexistent/path"))

    def test_full_scan_problematic_repo(self, problematic_repo_path):
        """Test complete scan on problematic repository."""
        engine = ScorecardEngine(problematic_repo_path)
        result = engine.scan()

        assert isinstance(result, ScanResult)
        assert result.report is not None

        # Should have findings
        assert result.report.total_findings > 0

        # Should have calculated score
        assert 0 <= result.score <= 100

        # Should have run all detectors
        assert len(result.report.detector_results) == 8

        # Should have some blockers or warnings
        assert result.report.blocker_count > 0 or result.report.warning_count > 0

    def test_full_scan_good_repo(self, good_repo_path):
        """Test complete scan on good repository."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        assert isinstance(result, ScanResult)

        # Good repo should score high
        assert result.score >= 85

        # Should have no blockers
        assert result.report.blocker_count == 0

        # Should have run all detectors
        assert len(result.report.detector_results) == 8

    def test_selective_detector_run(self, problematic_repo_path):
        """Test running only specific detectors."""
        engine = ScorecardEngine(problematic_repo_path)
        result = engine.scan(detectors=["data-ui-coverage", "relative-links"])

        # Should only run requested detectors
        assert len(result.report.detector_results) == 2

        detector_names = [dr.detector_name for dr in result.report.detector_results]
        assert "data-ui-coverage" in detector_names
        assert "relative-links" in detector_names

    def test_scan_performance_metrics(self, good_repo_path):
        """Test that scan records performance metrics."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        # Should record timing
        assert result.report.total_scan_time_ms > 0

        # Should count files
        assert result.report.files_scanned > 0

        # Each detector should record time
        for dr in result.report.detector_results:
            assert dr.execution_time_ms >= 0

    def test_score_calculation(self, problematic_repo_path):
        """Test score calculation logic."""
        engine = ScorecardEngine(problematic_repo_path)
        result = engine.scan()

        report = result.report

        # Score should be inverse to findings
        if report.blocker_count > 0:
            assert report.score < 80

        if report.blocker_count == 0 and report.warning_count == 0:
            assert report.score >= 90

    def test_json_export(self, good_repo_path):
        """Test JSON export functionality."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        json_output = result.to_json()

        assert json_output is not None
        assert isinstance(json_output, str)

        # Should be valid JSON
        import json
        data = json.loads(json_output)

        assert "scan_id" in data
        assert "score" in data
        assert "summary" in data
        assert "findings" in data

    def test_markdown_export(self, good_repo_path):
        """Test Markdown export functionality."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        md_output = result.to_markdown()

        assert md_output is not None
        assert isinstance(md_output, str)

        # Should contain expected sections
        assert "# AI-Readiness Scorecard Report" in md_output
        assert "## Overall Score" in md_output
        assert "## Summary" in md_output

    def test_finding_aggregation(self, problematic_repo_path):
        """Test that findings are properly aggregated by severity."""
        engine = ScorecardEngine(problematic_repo_path)
        result = engine.scan()

        report = result.report

        # Counts should match lists
        assert len(report.blockers) == report.blocker_count
        assert len(report.warnings) == report.warning_count
        assert len(report.info) == report.info_count

        total = report.blocker_count + report.warning_count + report.info_count
        assert total == report.total_findings

    def test_detector_error_handling(self, good_repo_path):
        """Test that detector errors are captured."""
        engine = ScorecardEngine(good_repo_path)

        # This should complete even if individual detectors might fail
        result = engine.scan()

        # Check that errors are recorded if any
        for dr in result.report.detector_results:
            if dr.error:
                assert isinstance(dr.error, str)

    def test_tenant_isolation(self, good_repo_path):
        """Test that tenant_id and repository_id are preserved."""
        engine = ScorecardEngine(
            good_repo_path,
            repository_id="test-repo-123",
            tenant_id="test-tenant-456"
        )
        result = engine.scan()

        assert result.report.repository_id == "test-repo-123"
        assert result.report.tenant_id == "test-tenant-456"


class TestScanResult:
    """Test ScanResult class."""

    def test_scan_result_creation(self, good_repo_path):
        """Test ScanResult is properly created."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        assert result.scan_id is not None
        assert result.score >= 0
        assert result.report is not None

    def test_multiple_export_formats(self, good_repo_path):
        """Test that both JSON and MD can be exported."""
        engine = ScorecardEngine(good_repo_path)
        result = engine.scan()

        json_output = result.to_json()
        md_output = result.to_markdown()

        assert json_output != md_output
        assert len(json_output) > 0
        assert len(md_output) > 0
