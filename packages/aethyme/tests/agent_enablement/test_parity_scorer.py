"""Tests for Parity Scorer"""

import pytest
from pathlib import Path
from src.agent_enablement.parity_scorer import ParityScorer, ParityReport
from src.agent_enablement.invariants import InvariantRulesEngine


class TestParityScorer:
    """Test parity scoring algorithm"""

    @pytest.fixture
    def scorer(self):
        return ParityScorer()

    @pytest.fixture
    def sample_repo(self, tmp_path):
        """Create sample repository"""
        repo = tmp_path / "sample"
        repo.mkdir()

        # Mix of compliant and non-compliant code
        (repo / "Good.tsx").write_text('<button data-ui="btn">Click</button>')
        (repo / "Bad.tsx").write_text('<button>Click</button>')
        (repo / "agent.md").write_text("# Manifest")

        return repo

    def test_scan_repository(self, scorer, sample_repo):
        """Test full repository scan"""
        report = scorer.scan_repository(sample_repo)

        assert isinstance(report, ParityReport)
        assert report.overall_score >= 0
        assert report.overall_score <= 100
        assert len(report.invariants) == 9

    def test_parity_report_serialization(self, scorer, sample_repo):
        """Test report serialization"""
        report = scorer.scan_repository(sample_repo)

        # Test to_dict
        report_dict = report.to_dict()
        assert "overall_score" in report_dict
        assert "invariants" in report_dict

        # Test to_json
        json_str = report.to_json()
        assert isinstance(json_str, str)
        assert "overall_score" in json_str

    def test_score_boundaries(self, scorer, sample_repo):
        """Test scores are within valid ranges"""
        report = scorer.scan_repository(sample_repo)

        assert 0 <= report.overall_score <= 100

        for inv_score in report.invariants.values():
            assert 0 <= inv_score.score <= 100

    def test_recommendations_generation(self, scorer, sample_repo):
        """Test recommendations are generated"""
        report = scorer.scan_repository(sample_repo)

        assert len(report.recommendations) > 0
        assert any("score" in r.lower() or "violations" in r.lower()
                  for r in report.recommendations)

    def test_save_report_json(self, scorer, sample_repo, tmp_path):
        """Test saving report to JSON"""
        report = scorer.scan_repository(sample_repo)
        output = tmp_path / "report.json"

        scorer.save_report(report, output)

        assert output.exists()
        content = output.read_text()
        assert "overall_score" in content

    def test_save_report_markdown(self, scorer, sample_repo, tmp_path):
        """Test saving report to Markdown"""
        report = scorer.scan_repository(sample_repo)
        output = tmp_path / "report.md"

        scorer.save_report(report, output)

        assert output.exists()
        content = output.read_text()
        assert "# Agent-Friendliness Parity Report" in content

    def test_parity_breakdown(self, scorer, sample_repo):
        """Test parity score breakdown"""
        report = scorer.scan_repository(sample_repo)
        breakdown = scorer.calculate_parity_score_breakdown(report)

        assert "overall" in breakdown
        assert "by_category" in breakdown
        assert "by_severity" in breakdown
        assert "fixability" in breakdown

    def test_empty_repository(self, scorer, tmp_path):
        """Test scoring empty repository"""
        empty_repo = tmp_path / "empty"
        empty_repo.mkdir()

        report = scorer.scan_repository(empty_repo)

        # Empty repo should still produce valid report
        assert isinstance(report, ParityReport)
        assert len(report.invariants) == 9
