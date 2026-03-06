"""Data models for AI-Readiness Scorecard."""

from datetime import UTC, datetime
from enum import StrEnum
from typing import cast

from pydantic import BaseModel, ConfigDict, Field


class Severity(StrEnum):
    """Severity levels for findings."""
    BLOCKER = "blocker"
    WARNING = "warning"
    INFO = "info"


class Finding(BaseModel):
    """A single finding from a detector."""
    model_config = ConfigDict(use_enum_values=True)

    detector: str = Field(..., description="Name of the detector that found this issue")
    severity: Severity = Field(..., description="Severity level")
    message: str = Field(..., description="Human-readable description")
    file_path: str = Field(..., description="File path where issue was found")
    line_number: int | None = Field(None, description="Line number if applicable")
    evidence: str | None = Field(None, description="Code snippet or evidence")
    suggestion: str | None = Field(None, description="Suggested fix")


class DetectorResult(BaseModel):
    """Result from a single detector."""
    detector_name: str
    findings: list[Finding] = Field(default_factory=lambda: cast(list[Finding], []))
    execution_time_ms: float
    error: str | None = None


class ScorecardReport(BaseModel):
    """Complete scorecard report."""
    scan_id: str
    repository_path: str
    repository_id: str | None = None
    tenant_id: str | None = None
    timestamp: datetime = Field(default_factory=lambda: datetime.now(UTC))
    score: int = Field(..., ge=0, le=100, description="Overall AI-readiness score")

    # Findings by severity
    blockers: list[Finding] = Field(default_factory=lambda: cast(list[Finding], []))
    warnings: list[Finding] = Field(default_factory=lambda: cast(list[Finding], []))
    info: list[Finding] = Field(default_factory=lambda: cast(list[Finding], []))

    # Summary statistics
    total_findings: int = 0
    blocker_count: int = 0
    warning_count: int = 0
    info_count: int = 0

    # Detector results
    detector_results: list[DetectorResult] = Field(default_factory=lambda: cast(list[DetectorResult], []))

    # Performance metrics
    total_scan_time_ms: float = 0.0
    files_scanned: int = 0

    def add_finding(self, finding: Finding):
        """Add a finding and update counts."""
        if finding.severity == Severity.BLOCKER:
            self.blockers.append(finding)
            self.blocker_count += 1
        elif finding.severity == Severity.WARNING:
            self.warnings.append(finding)
            self.warning_count += 1
        else:
            self.info.append(finding)
            self.info_count += 1
        self.total_findings += 1

    def calculate_score(self) -> int:
        """
        Calculate overall score (0-100).

        Algorithm:
        - Start with 100
        - Subtract 20 per blocker (max 5 blockers = -100)
        - Subtract 5 per warning (max 20 warnings impact)
        - Subtract 1 per info (max 10 info impact)
        """
        score = 100
        score -= min(self.blocker_count * 20, 100)
        score -= min(self.warning_count * 5, 100)
        score -= min(self.info_count * 1, 10)
        self.score = max(0, score)
        return self.score


class ScanSummary(BaseModel):
    """Summary view of a scan for API responses."""
    scan_id: str
    repository_id: str | None
    timestamp: datetime
    score: int
    blocker_count: int
    warning_count: int
    info_count: int
    total_findings: int
    files_scanned: int
    scan_time_ms: float
