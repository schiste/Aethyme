"""Tests for scorecard detectors."""

from pathlib import Path

from src.scorecard.detectors.ability_coverage import AbilityCoverageDetector
from src.scorecard.detectors.data_ui_coverage import DataUICoverageDetector
from src.scorecard.detectors.folder_docs import FolderDocsDetector
from src.scorecard.detectors.generated_files import GeneratedFilesDetector
from src.scorecard.detectors.i18n_gaps import I18nGapsDetector
from src.scorecard.detectors.relative_links import RelativeLinksDetector
from src.scorecard.detectors.route_coverage import RouteCoverageDetector
from src.scorecard.detectors.schema_drift import SchemaDriftDetector
from src.scorecard.models import Finding, Severity
from tests.support.repo_builders import (
    build_good_scorecard_repo,
    build_problematic_scorecard_repo,
)


class TestDataUICoverageDetector:
    def test_detects_missing_selectors(self, tmp_path: Path) -> None:
        detector = DataUICoverageDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("data-ui" in f.message.lower() for f in findings)

    def test_no_findings_in_good_repo(self, tmp_path: Path) -> None:
        detector = DataUICoverageDetector(build_good_scorecard_repo(tmp_path))
        assert detector.detect() == []


class TestFolderDocsDetector:
    def test_detects_missing_folder_docs(self, tmp_path: Path) -> None:
        detector = FolderDocsDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("FOLDER.md" in f.message for f in findings)

    def test_good_repo_has_docs(self, tmp_path: Path) -> None:
        detector = FolderDocsDetector(build_good_scorecard_repo(tmp_path))
        assert detector.detect() == []


class TestRelativeLinksDetector:
    def test_detects_absolute_paths(self, tmp_path: Path) -> None:
        detector = RelativeLinksDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("absolute" in f.message.lower() for f in findings)

    def test_relative_links_ok(self, tmp_path: Path) -> None:
        detector = RelativeLinksDetector(build_good_scorecard_repo(tmp_path))
        assert detector.detect() == []


class TestI18nGapsDetector:
    def test_detects_hardcoded_strings(self, tmp_path: Path) -> None:
        detector = I18nGapsDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any("hardcoded" in f.message.lower() or "text" in f.message.lower() for f in findings)

    def test_i18n_usage_ok(self, tmp_path: Path) -> None:
        detector = I18nGapsDetector(build_good_scorecard_repo(tmp_path))
        assert detector.detect() == []


class TestGeneratedFilesDetector:
    def test_detects_generated_files(self, tmp_path: Path) -> None:
        detector = GeneratedFilesDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any(f.severity == Severity.BLOCKER for f in findings)
        assert any("generated" in f.message.lower() for f in findings)

    def test_no_generated_files(self, tmp_path: Path) -> None:
        detector = GeneratedFilesDetector(build_good_scorecard_repo(tmp_path))
        assert detector.detect() == []


class TestSchemaDriftDetector:
    def test_detects_any_types(self, tmp_path: Path) -> None:
        detector = SchemaDriftDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any("any" in f.message.lower() for f in findings)

    def test_proper_types(self, tmp_path: Path) -> None:
        detector = SchemaDriftDetector(build_good_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert all(f.severity != Severity.BLOCKER for f in findings)


class TestRouteCoverageDetector:
    def test_detects_undocumented_routes(self, tmp_path: Path) -> None:
        detector = RouteCoverageDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings
        assert any(f.severity == Severity.WARNING for f in findings)
        assert any("undocumented" in f.message.lower() for f in findings)


class TestAbilityCoverageDetector:
    def test_detects_missing_permissions(self, tmp_path: Path) -> None:
        detector = AbilityCoverageDetector(build_problematic_scorecard_repo(tmp_path))
        findings = detector.detect()

        assert findings


def test_detector_precision_problematic_repo(tmp_path: Path) -> None:
    from src.scorecard.detectors import ALL_DETECTORS

    repo_path = build_problematic_scorecard_repo(tmp_path)
    all_findings: list[Finding] = []
    for detector_class in ALL_DETECTORS:
        all_findings.extend(detector_class(repo_path).detect())

    assert all_findings
    for finding in all_findings:
        assert finding.detector
        assert finding.severity in [Severity.BLOCKER, Severity.WARNING, Severity.INFO]
        assert finding.message
        assert finding.file_path


def test_detector_recall_good_repo(tmp_path: Path) -> None:
    from src.scorecard.detectors import ALL_DETECTORS

    repo_path = build_good_scorecard_repo(tmp_path)
    all_findings: list[Finding] = []
    for detector_class in ALL_DETECTORS:
        all_findings.extend(detector_class(repo_path).detect())

    assert len(all_findings) <= 2
    blockers = [f for f in all_findings if f.severity == Severity.BLOCKER]
    assert blockers == []
