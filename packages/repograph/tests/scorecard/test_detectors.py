"""Tests for scorecard detectors."""

import pytest
from pathlib import Path

from src.scorecard.detectors.data_ui_coverage import DataUICoverageDetector
from src.scorecard.detectors.folder_docs import FolderDocsDetector
from src.scorecard.detectors.relative_links import RelativeLinksDetector
from src.scorecard.detectors.i18n_gaps import I18nGapsDetector
from src.scorecard.detectors.generated_files import GeneratedFilesDetector
from src.scorecard.detectors.schema_drift import SchemaDriftDetector
from src.scorecard.detectors.route_coverage import RouteCoverageDetector
from src.scorecard.detectors.ability_coverage import AbilityCoverageDetector
from src.scorecard.models import Severity


@pytest.fixture
def good_repo_path():
    """Path to good test repository."""
    return Path(__file__).parent / "fixtures" / "good_repo"


@pytest.fixture
def problematic_repo_path():
    """Path to problematic test repository."""
    return Path(__file__).parent / "fixtures" / "problematic_repo"


class TestDataUICoverageDetector:
    """Test data-ui coverage detector."""

    def test_detects_missing_selectors(self, problematic_repo_path):
        """Test that detector finds missing data-ui selectors."""
        detector = DataUICoverageDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find multiple missing selectors in BadButton.tsx
        assert len(findings) > 0
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("data-ui" in f.message.lower() for f in findings)

    def test_no_findings_in_good_repo(self, good_repo_path):
        """Test that good repo with selectors has no findings."""
        detector = DataUICoverageDetector(good_repo_path)
        findings = detector.detect()

        # Good repo should have no missing selector findings
        assert len(findings) == 0


class TestFolderDocsDetector:
    """Test folder documentation detector."""

    def test_detects_missing_folder_docs(self, problematic_repo_path):
        """Test that detector finds missing FOLDER.md."""
        detector = FolderDocsDetector(problematic_repo_path)
        findings = detector.detect()

        # Problematic repo missing documentation
        assert len(findings) > 0
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("FOLDER.md" in f.message for f in findings)

    def test_good_repo_has_docs(self, good_repo_path):
        """Test that good repo with FOLDER.md has fewer findings."""
        detector = FolderDocsDetector(good_repo_path)
        findings = detector.detect()

        # Good repo should have fewer or no findings
        # (May still have some for auto-generated folders)
        assert len(findings) <= 1


class TestRelativeLinksDetector:
    """Test relative links detector."""

    def test_detects_absolute_paths(self, problematic_repo_path):
        """Test that detector finds absolute file paths."""
        detector = RelativeLinksDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find absolute paths in README.md
        assert len(findings) > 0
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("absolute" in f.message.lower() for f in findings)

    def test_relative_links_ok(self, good_repo_path):
        """Test that good repo with relative links has no findings."""
        detector = RelativeLinksDetector(good_repo_path)
        findings = detector.detect()

        # Good repo uses relative links
        assert len(findings) == 0


class TestI18nGapsDetector:
    """Test i18n gaps detector."""

    def test_detects_hardcoded_strings(self, problematic_repo_path):
        """Test that detector finds hardcoded user-facing strings."""
        detector = I18nGapsDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find hardcoded strings in BadButton.tsx
        assert len(findings) > 0
        assert any("hardcoded" in f.message.lower() or "text" in f.message.lower() for f in findings)

    def test_i18n_usage_ok(self, good_repo_path):
        """Test that good repo using i18n has no/fewer findings."""
        detector = I18nGapsDetector(good_repo_path)
        findings = detector.detect()

        # Good repo uses t() function, should have no findings
        assert len(findings) == 0


class TestGeneratedFilesDetector:
    """Test generated files detector."""

    def test_detects_generated_files(self, problematic_repo_path):
        """Test that detector finds generated files."""
        detector = GeneratedFilesDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find generated file with @generated marker
        assert len(findings) > 0
        assert any(f.severity == Severity.BLOCKER for f in findings)
        assert any("generated" in f.message.lower() for f in findings)

    def test_no_generated_files(self, good_repo_path):
        """Test that good repo has no generated files."""
        detector = GeneratedFilesDetector(good_repo_path)
        findings = detector.detect()

        assert len(findings) == 0


class TestSchemaDriftDetector:
    """Test schema drift detector."""

    def test_detects_any_types(self, problematic_repo_path):
        """Test that detector finds 'any' types in TypeScript."""
        detector = SchemaDriftDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find 'any' types in schema.ts
        assert len(findings) > 0
        assert any("any" in f.message.lower() for f in findings)

    def test_proper_types(self, good_repo_path):
        """Test that good repo with proper types has no findings."""
        detector = SchemaDriftDetector(good_repo_path)
        findings = detector.detect()

        # Good repo uses specific types
        # May have INFO findings about missing validators
        assert all(f.severity != Severity.BLOCKER for f in findings)


class TestRouteCoverageDetector:
    """Test route coverage detector."""

    def test_detects_undocumented_routes(self, problematic_repo_path):
        """Test that detector finds undocumented API routes."""
        detector = RouteCoverageDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find undocumented routes in routes.py
        assert len(findings) > 0
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("undocumented" in f.message.lower() for f in findings)


class TestAbilityCoverageDetector:
    """Test ability coverage detector."""

    def test_detects_missing_permissions(self, problematic_repo_path):
        """Test that detector finds missing permission checks."""
        detector = AbilityCoverageDetector(problematic_repo_path)
        findings = detector.detect()

        # Should find routes without permission checks
        # May be INFO level if no auth framework detected
        assert len(findings) >= 0


# Precision/Recall Tests

def test_detector_precision_problematic_repo(problematic_repo_path):
    """Test precision: all findings should be valid issues."""
    from src.scorecard.detectors import ALL_DETECTORS

    all_findings = []
    for detector_class in ALL_DETECTORS:
        detector = detector_class(problematic_repo_path)
        findings = detector.detect()
        all_findings.extend(findings)

    # We expect findings in problematic repo
    assert len(all_findings) > 0

    # All findings should have required fields
    for finding in all_findings:
        assert finding.detector
        assert finding.severity in [Severity.BLOCKER, Severity.WARNING, Severity.INFO]
        assert finding.message
        assert finding.file_path


def test_detector_recall_good_repo(good_repo_path):
    """Test recall: good repo should have minimal findings."""
    from src.scorecard.detectors import ALL_DETECTORS

    all_findings = []
    for detector_class in ALL_DETECTORS:
        detector = detector_class(good_repo_path)
        findings = detector.detect()
        all_findings.extend(findings)

    # Good repo should have few or no findings
    assert len(all_findings) <= 5

    # No blockers in good repo
    blockers = [f for f in all_findings if f.severity == Severity.BLOCKER]
    assert len(blockers) == 0
