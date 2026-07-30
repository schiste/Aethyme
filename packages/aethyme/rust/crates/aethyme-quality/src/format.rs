//! Report renderers (port of `src/scorecard/formatters.py`).
//!
//! JSON goes through `aethyme_enhance::pyjson` so the bytes match
//! CPython's `json.dumps(data, indent=2, default=str)` exactly
//! (insertion-ordered keys, `ensure_ascii` escapes, repr float
//! rendering). Markdown is a line-for-line port.

use aethyme_enhance::pyjson::{Value, dumps_indent2};

use crate::model::{Finding, ScorecardReport};

/// Port of `JSONFormatter.format`.
pub fn format_json(report: &ScorecardReport) -> String {
    let mut root = Value::object();
    root.set("scan_id", Value::str(report.scan_id.clone()));

    let mut repository = Value::object();
    repository.set("path", Value::str(report.repository_path.clone()));
    repository.set("id", opt_str(&report.repository_id));
    repository.set("tenant_id", opt_str(&report.tenant_id));
    root.set("repository", repository);

    root.set("timestamp", Value::str(report.timestamp_iso.clone()));
    root.set("score", Value::int(report.score as i128));

    let mut summary = Value::object();
    summary.set("total_findings", Value::int(report.total_findings as i128));
    summary.set("blockers", Value::int(report.blocker_count as i128));
    summary.set("warnings", Value::int(report.warning_count as i128));
    summary.set("info", Value::int(report.info_count as i128));
    root.set("summary", summary);

    let mut findings = Value::object();
    findings.set(
        "blockers",
        Value::Array(report.blockers.iter().map(finding_to_value).collect()),
    );
    findings.set(
        "warnings",
        Value::Array(report.warnings.iter().map(finding_to_value).collect()),
    );
    findings.set(
        "info",
        Value::Array(report.info.iter().map(finding_to_value).collect()),
    );
    root.set("findings", findings);

    let detectors: Vec<Value> = report
        .detector_results
        .iter()
        .map(|dr| {
            let mut d = Value::object();
            d.set("name", Value::str(dr.detector_name.clone()));
            d.set("findings_count", Value::int(dr.findings.len() as i128));
            d.set("execution_time_ms", Value::Float(dr.execution_time_ms));
            d.set("error", opt_str(&dr.error));
            d
        })
        .collect();
    root.set("detectors", Value::Array(detectors));

    let mut performance = Value::object();
    performance.set("total_scan_time_ms", Value::Float(report.total_scan_time_ms));
    performance.set("files_scanned", Value::int(report.files_scanned as i128));
    root.set("performance", performance);

    dumps_indent2(&root)
}

fn opt_str(value: &Option<String>) -> Value {
    match value {
        Some(s) => Value::str(s.clone()),
        None => Value::Null,
    }
}

fn finding_to_value(finding: &Finding) -> Value {
    let mut v = Value::object();
    v.set("detector", Value::str(finding.detector.clone()));
    v.set("severity", Value::str(finding.severity.as_str()));
    v.set("message", Value::str(finding.message.clone()));
    v.set("file", Value::str(finding.file_path.clone()));
    v.set(
        "line",
        match finding.line_number {
            Some(n) => Value::int(n as i128),
            None => Value::Null,
        },
    );
    v.set("evidence", opt_str(&finding.evidence));
    v.set("suggestion", opt_str(&finding.suggestion));
    v
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
    lines.push(format!("- **Scan Time:** {:.0}ms", report.total_scan_time_ms));
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
        lines.push(
            "These issues **should** be addressed for better agent performance:".to_string(),
        );
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
        lines.push("👍 **Good** - Address the warnings to improve agent effectiveness.".to_string());
    } else if report.score >= 50 {
        lines.push(
            "⚠️ **Needs Improvement** - Address blockers and warnings before deploying agents."
                .to_string(),
        );
    } else {
        lines.push("🚨 **Critical** - Significant issues detected. Fix blockers immediately.".to_string());
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
    fn json_shape_matches_python_layout() {
        let out = format_json(&sample_report());
        assert!(out.starts_with("{\n  \"scan_id\": \"abc-123\","));
        assert!(out.contains("\"repository\": {\n    \"path\": \"/repo\",\n    \"id\": null,\n    \"tenant_id\": null\n  }"));
        assert!(out.contains("\"severity\": \"blocker\""));
        assert!(out.contains("\"line\": 2"));
        assert!(out.contains("\"line\": null"));
        assert!(out.contains("\"execution_time_ms\": 1.5"));
        // ensure_ascii is irrelevant here (no non-ASCII in JSON output),
        // and no trailing newline, exactly like json.dumps.
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
        assert!(out.ends_with("👍 **Good** - Address the warnings to improve agent effectiveness.\n"));
    }
}
