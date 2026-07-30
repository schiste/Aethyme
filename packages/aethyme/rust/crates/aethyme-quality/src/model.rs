//! Data models for the AI-readiness scorecard (port of
//! `src/scorecard/models.py`, pydantic → plain Rust structs).
//!
//! Scoring is integer end-to-end — the plan forbids f64 anywhere in the
//! scoring path. The only floats in the report are performance
//! durations (`*_time_ms`), which are metrics, not score inputs, and
//! are normalized away by the parity harness.

/// Severity levels for findings. Serialized values match the Python
/// `StrEnum` values exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Blocker,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Blocker => "blocker",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// A single finding from a detector. Field-for-field the Python
/// `Finding` pydantic model (`use_enum_values=True` meant severity was
/// already a plain string on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub detector: String,
    pub severity: Severity,
    pub message: String,
    pub file_path: String,
    pub line_number: Option<i64>,
    pub evidence: Option<String>,
    pub suggestion: Option<String>,
}

/// Result from a single detector run.
#[derive(Debug, Clone)]
pub struct DetectorResult {
    pub detector_name: String,
    pub findings: Vec<Finding>,
    pub execution_time_ms: f64,
    pub error: Option<String>,
}

/// Complete scorecard report (port of `ScorecardReport`).
#[derive(Debug, Clone)]
pub struct ScorecardReport {
    pub scan_id: String,
    pub repository_path: String,
    pub repository_id: Option<String>,
    pub tenant_id: Option<String>,
    /// Python `datetime.now(UTC).isoformat()` shape
    /// (`YYYY-MM-DDTHH:MM:SS.ffffff+00:00`), captured at report build.
    pub timestamp_iso: String,
    /// Same instant rendered for the markdown header
    /// (`strftime('%Y-%m-%d %H:%M:%S UTC')`).
    pub timestamp_display: String,
    pub score: i64,

    pub blockers: Vec<Finding>,
    pub warnings: Vec<Finding>,
    pub info: Vec<Finding>,

    pub total_findings: i64,
    pub blocker_count: i64,
    pub warning_count: i64,
    pub info_count: i64,

    pub detector_results: Vec<DetectorResult>,

    pub total_scan_time_ms: f64,
    pub files_scanned: i64,
}

impl ScorecardReport {
    pub fn new(
        scan_id: String,
        repository_path: String,
        repository_id: Option<String>,
        tenant_id: Option<String>,
        timestamp_iso: String,
        timestamp_display: String,
    ) -> Self {
        ScorecardReport {
            scan_id,
            repository_path,
            repository_id,
            tenant_id,
            timestamp_iso,
            timestamp_display,
            score: 100,
            blockers: Vec::new(),
            warnings: Vec::new(),
            info: Vec::new(),
            total_findings: 0,
            blocker_count: 0,
            warning_count: 0,
            info_count: 0,
            detector_results: Vec::new(),
            total_scan_time_ms: 0.0,
            files_scanned: 0,
        }
    }

    /// Add a finding and update counts (port of `add_finding`).
    pub fn add_finding(&mut self, finding: Finding) {
        match finding.severity {
            Severity::Blocker => {
                self.blockers.push(finding);
                self.blocker_count += 1;
            }
            Severity::Warning => {
                self.warnings.push(finding);
                self.warning_count += 1;
            }
            Severity::Info => {
                self.info.push(finding);
                self.info_count += 1;
            }
        }
        self.total_findings += 1;
    }

    /// Calculate the overall score (0-100), integers only:
    /// 100 − min(blockers×20, 100) − min(warnings×5, 100) − min(info, 10),
    /// clamped at 0. Exactly the Python `calculate_score`.
    pub fn calculate_score(&mut self) -> i64 {
        let mut score: i64 = 100;
        score -= (self.blocker_count * 20).min(100);
        score -= (self.warning_count * 5).min(100);
        score -= self.info_count.min(10);
        self.score = score.max(0);
        self.score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity) -> Finding {
        Finding {
            detector: "test".to_string(),
            severity,
            message: "m".to_string(),
            file_path: "f".to_string(),
            line_number: None,
            evidence: None,
            suggestion: None,
        }
    }

    #[test]
    fn score_formula_matches_python() {
        // 1 blocker, 11 warnings, 1 info → 100 - 20 - 55 - 1 = 24
        // (observed from the Python CLI on the problematic fixture).
        let mut report = base();
        report.add_finding(finding(Severity::Blocker));
        for _ in 0..11 {
            report.add_finding(finding(Severity::Warning));
        }
        report.add_finding(finding(Severity::Info));
        assert_eq!(report.calculate_score(), 24);
    }

    #[test]
    fn score_caps_and_clamps() {
        let mut report = base();
        for _ in 0..6 {
            report.add_finding(finding(Severity::Blocker)); // min(120,100)=100
        }
        assert_eq!(report.calculate_score(), 0);

        let mut report = base();
        for _ in 0..25 {
            report.add_finding(finding(Severity::Info)); // min(25,10)=10
        }
        assert_eq!(report.calculate_score(), 90);

        let mut report = base();
        for _ in 0..21 {
            report.add_finding(finding(Severity::Warning)); // min(105,100)=100
        }
        assert_eq!(report.calculate_score(), 0);

        let report_score = {
            let mut report = base();
            report.calculate_score()
        };
        assert_eq!(report_score, 100);
    }

    #[test]
    fn counts_track_findings() {
        let mut report = base();
        report.add_finding(finding(Severity::Blocker));
        report.add_finding(finding(Severity::Warning));
        report.add_finding(finding(Severity::Info));
        assert_eq!(report.blocker_count, 1);
        assert_eq!(report.warning_count, 1);
        assert_eq!(report.info_count, 1);
        assert_eq!(report.total_findings, 3);
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.info.len(), 1);
    }

    fn base() -> ScorecardReport {
        ScorecardReport::new(
            "id".to_string(),
            "/repo".to_string(),
            None,
            None,
            "t".to_string(),
            "t".to_string(),
        )
    }
}
