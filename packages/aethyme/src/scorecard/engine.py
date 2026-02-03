"""Scoring engine for AI-Readiness Scorecard."""

import time
import uuid
from pathlib import Path
from typing import List, Optional
import structlog

from .models import ScorecardReport, DetectorResult, Finding
from .detectors import ALL_DETECTORS
from .formatters import JSONFormatter, MarkdownFormatter

logger = structlog.get_logger(__name__)


class ScanResult:
    """Result of a scorecard scan."""

    def __init__(self, report: ScorecardReport):
        self.report = report
        self.scan_id = report.scan_id
        self.score = report.score

    def to_json(self) -> str:
        """Export report as JSON."""
        formatter = JSONFormatter()
        return formatter.format(self.report)

    def to_markdown(self) -> str:
        """Export report as Markdown."""
        formatter = MarkdownFormatter()
        return formatter.format(self.report)


class ScorecardEngine:
    """Engine for running AI-readiness scorecard scans."""

    def __init__(self, repo_path: Path, repository_id: Optional[str] = None, tenant_id: Optional[str] = None):
        """
        Initialize scorecard engine.

        Args:
            repo_path: Path to repository to scan
            repository_id: Optional repository ID from database
            tenant_id: Optional tenant ID for multi-tenancy
        """
        self.repo_path = Path(repo_path)
        self.repository_id = repository_id
        self.tenant_id = tenant_id
        self.logger = logger.bind(repo_path=str(repo_path))

        if not self.repo_path.exists():
            raise ValueError(f"Repository path does not exist: {repo_path}")

    def scan(self, detectors: Optional[List[str]] = None) -> ScanResult:
        """
        Run scorecard scan.

        Args:
            detectors: Optional list of detector names to run. If None, runs all.

        Returns:
            ScanResult with findings and score
        """
        scan_id = str(uuid.uuid4())
        start_time = time.time()

        self.logger.info("Starting scorecard scan", scan_id=scan_id)

        # Initialize report
        report = ScorecardReport(
            scan_id=scan_id,
            repository_path=str(self.repo_path),
            repository_id=self.repository_id,
            tenant_id=self.tenant_id,
            score=100,  # Will be calculated
        )

        # Determine which detectors to run
        detectors_to_run = self._get_detectors(detectors)

        # Run each detector
        for detector_class in detectors_to_run:
            detector_result = self._run_detector(detector_class)
            report.detector_results.append(detector_result)

            # Add findings to report
            for finding in detector_result.findings:
                report.add_finding(finding)

        # Calculate final score
        report.calculate_score()

        # Calculate scan metrics
        end_time = time.time()
        report.total_scan_time_ms = (end_time - start_time) * 1000
        report.files_scanned = self._count_files()

        self.logger.info(
            "Scorecard scan completed",
            scan_id=scan_id,
            score=report.score,
            total_findings=report.total_findings,
            blockers=report.blocker_count,
            warnings=report.warning_count,
            scan_time_ms=report.total_scan_time_ms,
        )

        return ScanResult(report)

    def _get_detectors(self, detector_names: Optional[List[str]]) -> List[type]:
        """
        Get detector classes to run.

        Args:
            detector_names: Optional list of detector names

        Returns:
            List of detector classes
        """
        if detector_names is None:
            return ALL_DETECTORS

        # Filter detectors by name
        detectors = []
        for detector_class in ALL_DETECTORS:
            detector = detector_class(self.repo_path)
            if detector.name in detector_names:
                detectors.append(detector_class)

        return detectors

    def _run_detector(self, detector_class: type) -> DetectorResult:
        """
        Run a single detector.

        Args:
            detector_class: Detector class to instantiate and run

        Returns:
            DetectorResult with findings
        """
        detector = detector_class(self.repo_path)

        self.logger.debug("Running detector", detector=detector.name)

        start_time = time.time()
        error = None
        findings = []

        try:
            findings = detector.detect()
        except Exception as e:
            error = str(e)
            self.logger.error(
                "Detector failed",
                detector=detector.name,
                error=error,
                exc_info=True,
            )

        end_time = time.time()
        execution_time_ms = (end_time - start_time) * 1000

        self.logger.debug(
            "Detector completed",
            detector=detector.name,
            findings_count=len(findings),
            execution_time_ms=execution_time_ms,
        )

        return DetectorResult(
            detector_name=detector.name,
            findings=findings,
            execution_time_ms=execution_time_ms,
            error=error,
        )

    def _count_files(self) -> int:
        """Count total files scanned."""
        count = 0
        extensions = {'.py', '.ts', '.tsx', '.jsx', '.js', '.md', '.json', '.yaml', '.yml', '.html', '.vue'}

        for file_path in self.repo_path.rglob('*'):
            if file_path.is_file() and file_path.suffix in extensions:
                # Use same skip logic as detectors
                skip_dirs = {
                    'node_modules', '__pycache__', '.git', 'venv', '.venv',
                    'dist', 'build', '.pytest_cache', '.mypy_cache',
                    'site-packages', '.next', 'coverage', '.tox',
                }
                parts = set(file_path.parts)
                if not (parts & skip_dirs):
                    count += 1

        return count
