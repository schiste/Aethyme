"""Tests for Gap Detector"""

import pytest
from pathlib import Path
from src.agent_enablement.gap_detector import GapDetector, GapPriority


class TestGapDetector:
    """Test gap detection and prioritization"""

    @pytest.fixture
    def detector(self):
        return GapDetector()

    @pytest.fixture
    def gapped_repo(self, tmp_path):
        """Repository with various gaps"""
        repo = tmp_path / "gapped"
        repo.mkdir()

        # Missing data-ui (blocker)
        (repo / "Component.tsx").write_text('<button>Click</button>')

        # Missing FOLDER.md (warning)
        src = repo / "src"
        src.mkdir()
        (src / "index.ts").write_text("export const foo = 'bar';")

        return repo

    def test_detect_gaps(self, detector, gapped_repo):
        """Test gap detection"""
        gaps = detector.detect_gaps(gapped_repo)

        assert len(gaps) > 0
        assert all(hasattr(g, 'gap_id') for g in gaps)
        assert all(hasattr(g, 'priority') for g in gaps)

    def test_gaps_prioritized(self, detector, gapped_repo):
        """Test gaps are prioritized"""
        gaps = detector.detect_gaps(gapped_repo)

        # Check gaps are sorted by priority
        priorities = [g.priority for g in gaps]
        priority_order = [GapPriority.CRITICAL, GapPriority.HIGH,
                         GapPriority.MEDIUM, GapPriority.LOW]

        for i in range(len(priorities) - 1):
            current_idx = priority_order.index(priorities[i])
            next_idx = priority_order.index(priorities[i + 1])
            assert current_idx <= next_idx

    def test_get_autofixable_gaps(self, detector, gapped_repo):
        """Test filtering autofixable gaps"""
        all_gaps = detector.detect_gaps(gapped_repo)
        autofixable = detector.get_autofixable_gaps(all_gaps)

        assert all(g.fix and g.fix.autofix_available for g in autofixable)

    def test_gap_report_generation(self, detector, gapped_repo):
        """Test gap report generation"""
        gaps = detector.detect_gaps(gapped_repo)
        report = detector.generate_gap_report(gaps)

        assert "total_gaps" in report
        assert "by_priority" in report
        assert "autofixable" in report
        assert "recommendations" in report
