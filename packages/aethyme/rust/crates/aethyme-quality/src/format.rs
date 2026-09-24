//! Report renderers for the legacy `ai-ready` scorecard.
//!
//! JSON is `serde_json` pretty output over borrowed, field-ordered
//! structs, so keys keep the documented order. Markdown layout is the
//! frozen scorecard layout.

use serde::Serialize;

use crate::model::{Finding, ScorecardReport};

#[derive(Serialize)]
struct JsonReport<'a> {
    scan_id: &'a str,
    repository: JsonRepository<'a>,
    timestamp: &'a str,
    score: i64,
    summary: JsonSummary,
    findings: JsonFindings<'a>,
    detectors: Vec<JsonDetector<'a>>,
    performance: JsonPerformance,
}

#[derive(Serialize)]
struct JsonRepository<'a> {
    path: &'a str,
    id: Option<&'a str>,
    tenant_id: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonSummary {
    total_findings: i64,
    blockers: i64,
    warnings: i64,
    info: i64,
}

#[derive(Serialize)]
struct JsonFindings<'a> {
    blockers: Vec<JsonFinding<'a>>,
    warnings: Vec<JsonFinding<'a>>,
    info: Vec<JsonFinding<'a>>,
}

#[derive(Serialize)]
struct JsonFinding<'a> {
    detector: &'a str,
    severity: &'a str,
    message: &'a str,
    file: &'a str,
    line: Option<i64>,
    evidence: Option<&'a str>,
    suggestion: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonDetector<'a> {
    name: &'a str,
    findings_count: usize,
    execution_time_ms: f64,
    error: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonPerformance {
    total_scan_time_ms: f64,
    files_scanned: i64,
}

fn findings_json(findings: &[Finding]) -> Vec<JsonFinding<'_>> {
    findings
        .iter()
        .map(|finding| JsonFinding {
            detector: &finding.detector,
            severity: finding.severity.as_str(),
            message: &finding.message,
            file: &finding.file_path,
            line: finding.line_number,
            evidence: finding.evidence.as_deref(),
            suggestion: finding.suggestion.as_deref(),
        })
        .collect()
}

/// The `--format json` scorecard report (2-space indent, no trailing
/// newline).
pub fn format_json(report: &ScorecardReport) -> String {
    let json = JsonReport {
        scan_id: &report.scan_id,
        repository: JsonRepository {
            path: &report.repository_path,
            id: report.repository_id.as_deref(),
            tenant_id: report.tenant_id.as_deref(),
        },
        timestamp: &report.timestamp_iso,
        score: report.score,
        summary: JsonSummary {
            total_findings: report.total_findings,
            blockers: report.blocker_count,
            warnings: report.warning_count,
            info: report.info_count,
        },
        findings: JsonFindings {
            blockers: findings_json(&report.blockers),
            warnings: findings_json(&report.warnings),
            info: findings_json(&report.info),
        },
        detectors: report
            .detector_results
            .iter()
            .map(|dr| JsonDetector {
                name: &dr.detector_name,
                findings_count: dr.findings.len(),
                execution_time_ms: dr.execution_time_ms,
                error: dr.error.as_deref(),
            })
            .collect(),
        performance: JsonPerformance {
            total_scan_time_ms: report.total_scan_time_ms,
            files_scanned: report.files_scanned,
        },
    };
    serde_json::to_string_pretty(&json).expect("scorecard report serializes")
}

/// Port of `MarkdownFormatter.format`.
pub fn format_markdown(report: &ScorecardReport) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push("# AI-Readiness Scorecard Report".to_string());
    lines.push(String::new());
    lines.push(format!("**Scan ID:** `{}`", report.scan_id));
    lines.push(format!("**Repository:** `{}`", report.repository_path));
    lines.push(format!("**Timestamp:** {}", report.timestamp_display));
    lines.push(String::new());

    let score_emoji = score_emoji(report.score);
    lines.push(format!(
        "## Overall Score: {}/100 {}",
        report.score, score_emoji
    ));
    lines.push(String::new());

    lines.push("## Summary".to_string());
    lines.push(String::new());
    lines.push(format!("- **Total Findings:** {}", report.total_findings));
    lines.push(format!("- **Blockers:** {} 🔴", report.blocker_count));
    lines.push(format!("- **Warnings:** {} 🟡", report.warning_count));
    lines.push(format!("- **Info:** {} 🔵", report.info_count));
    lines.push(format!("- **Files Scanned:** {}", report.files_scanned));
    lines.push(format!(
        "- **Scan Time:** {:.0}ms",
        report.total_scan_time_ms
    ));
    lines.push(String::new());

    if !report.blockers.is_empty() {
        lines.push("## 🔴 Blockers".to_string());
        lines.push(String::new());
        lines.push("These issues **must** be fixed before agent deployment:".to_string());
        lines.push(String::new());
        for finding in &report.blockers {
            format_finding(finding, &mut lines);
        }
        lines.push(String::new());
    }

    if !report.warnings.is_empty() {
        lines.push("## 🟡 Warnings".to_string());
        lines.push(String::new());
        lines
            .push("These issues **should** be addressed for better agent performance:".to_string());
        lines.push(String::new());
        for finding in &report.warnings {
            format_finding(finding, &mut lines);
        }
        lines.push(String::new());
    }

    if !report.info.is_empty() {
        lines.push("## 🔵 Info".to_string());
        lines.push(String::new());
        lines.push("These suggestions may improve agent effectiveness:".to_string());
        lines.push(String::new());
        for finding in &report.info {
            format_finding(finding, &mut lines);
        }
        lines.push(String::new());
    }

    lines.push("## Detector Performance".to_string());
    lines.push(String::new());
    lines.push("| Detector | Findings | Time (ms) | Status |".to_string());
    lines.push("|----------|----------|-----------|--------|".to_string());
    for dr in &report.detector_results {
        let status = if dr.error.is_none() { "✅" } else { "❌" };
        lines.push(format!(
            "| {} | {} | {:.0} | {} |",
            dr.detector_name,
            dr.findings.len(),
            dr.execution_time_ms,
            status
        ));
    }
    lines.push(String::new());

    lines.push("## Recommendations".to_string());
    lines.push(String::new());
    if report.score >= 90 {
        lines.push("✨ **Excellent!** Your repository is well-prepared for AI agents.".to_string());
    } else if report.score >= 70 {
        lines
            .push("👍 **Good** - Address the warnings to improve agent effectiveness.".to_string());
    } else if report.score >= 50 {
        lines.push(
            "⚠️ **Needs Improvement** - Address blockers and warnings before deploying agents."
                .to_string(),
        );
    } else {
        lines.push(
            "🚨 **Critical** - Significant issues detected. Fix blockers immediately.".to_string(),
        );
    }
    lines.push(String::new());

    lines.join("\n")
}

/// Port of `MarkdownFormatter._format_finding`.
fn format_finding(finding: &Finding, lines: &mut Vec<String>) {
    let mut location = finding.file_path.clone();
    // Python truthiness: `if finding.line_number:` — line 0 (never
    // produced) would be falsy; Some(0) matched for exactness.
    if let Some(n) = finding.line_number
        && n != 0
    {
        location.push_str(&format!(":{n}"));
    }

    lines.push(format!("### {}", finding.message));
    lines.push(String::new());
    lines.push(format!("- **Location:** `{location}`"));
    lines.push(format!("- **Detector:** `{}`", finding.detector));

    if let Some(evidence) = finding.evidence.as_deref()
        && !evidence.is_empty()
    {
        lines.push("- **Evidence:**".to_string());
        lines.push("  ```".to_string());
        lines.push(format!("  {evidence}"));
        lines.push("  ```".to_string());
    }

    if let Some(suggestion) = finding.suggestion.as_deref()
        && !suggestion.is_empty()
    {
        lines.push(format!("- **Suggestion:** {suggestion}"));
    }

    lines.push(String::new());
}

fn score_emoji(score: i64) -> &'static str {
    if score >= 90 {
        "🌟"
    } else if score >= 70 {
        "✅"
    } else if score >= 50 {
        "⚠️"
    } else {
        "🚨"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DetectorResult, Severity};

    fn sample_report() -> ScorecardReport {
        let mut report = ScorecardReport::new(
            "abc-123".to_string(),
            "/repo".to_string(),
            None,
            None,
            "2026-07-30T08:13:06.462275+00:00".to_string(),
            "2026-07-30 08:13:06 UTC".to_string(),
        );
        report.add_finding(Finding {
            detector: "generated-files".to_string(),
            severity: Severity::Blocker,
            message: "File marked as generated - manual edits will be overwritten".to_string(),
            file_path: "src/generated/api_client.py".to_string(),
            line_number: None,
            evidence: Some("File contains: @generated".to_string()),
            suggestion: Some(
                "Do not edit generated files. Modify the template/generator instead.".to_string(),
            ),
        });
        report.add_finding(Finding {
            detector: "data-ui-coverage".to_string(),
            severity: Severity::Warning,
            message: "Missing data-ui attribute on button".to_string(),
            file_path: "src/components/BadButton.tsx".to_string(),
            line_number: Some(2),
            evidence: Some("return <button>x</button>;".to_string()),
            suggestion: Some("Add data-ui attribute".to_string()),
        });
        report.detector_results.push(DetectorResult {
            detector_name: "data-ui-coverage".to_string(),
            findings: vec![report.warnings[0].clone()],
            execution_time_ms: 1.5,
            error: None,
        });
        report.calculate_score();
        report.total_scan_time_ms = 8.6;
        report.files_scanned = 5;
        report
    }

    #[test]
    fn json_shape_keeps_documented_layout() {
        let out = format_json(&sample_report());
        assert!(out.starts_with("{\n  \"scan_id\": \"abc-123\","));
        assert!(out.contains("\"repository\": {\n    \"path\": \"/repo\",\n    \"id\": null,\n    \"tenant_id\": null\n  }"));
        assert!(out.contains("\"severity\": \"blocker\""));
        assert!(out.contains("\"line\": 2"));
        assert!(out.contains("\"line\": null"));
        assert!(out.contains("\"execution_time_ms\": 1.5"));
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn markdown_layout_matches_python() {
        let report = sample_report();
        let out = format_markdown(&report);
        assert!(out.starts_with("# AI-Readiness Scorecard Report\n\n**Scan ID:** `abc-123`\n"));
        assert!(out.contains("## Overall Score: 75/100 ✅\n"));
        assert!(out.contains("- **Scan Time:** 9ms\n"));
        assert!(out.contains("## 🔴 Blockers\n"));
        assert!(out.contains("- **Location:** `src/components/BadButton.tsx:2`\n"));
        assert!(out.contains("| data-ui-coverage | 1 | 2 | ✅ |\n"));
        assert!(
            out.ends_with("👍 **Good** - Address the warnings to improve agent effectiveness.\n")
        );
    }
}
