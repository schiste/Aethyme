use std::cmp::Ordering;
use std::path::PathBuf;

use aethyme_enhance::pyjson::{Value, dumps_indent2};

use crate::engine::ScorecardEngine;
use crate::model::{Finding, QualityInspection, Severity};

const DEFAULT_FINDING_LIMIT: usize = 100;

const HELP: &str = "Usage: aethyme quality inspect [OPTIONS]

  Run bounded, optional repository-quality analysis.

  This report is advisory. Use `aethyme readiness` for authoritative
  operational readiness.

Options:
  --repo PATH             Git working tree (defaults to current directory)
  -f, --format [json|md]  Output format (default: md)
  -o, --output PATH       Output file (defaults to stdout)
  --detectors TEXT        Comma-separated detector names
  --limit N               Maximum rendered findings (default: 100)
  --full                  Render every finding
  --help                  Show this message and exit.
";

#[derive(Debug)]
struct Args {
    repo: Option<String>,
    format: String,
    output: Option<String>,
    detectors: Option<Vec<String>>,
    limit: usize,
    full: bool,
}

pub fn run(args: &[String]) -> u8 {
    if args.is_empty() || args == ["--help"] {
        print!("{HELP}");
        return 0;
    }
    if args.first().map(String::as_str) != Some("inspect") {
        eprintln!("Error: expected `aethyme quality inspect`");
        return 2;
    }
    let parsed = match parse_args(&args[1..]) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{HELP}");
            return 0;
        }
        Err(message) => {
            eprintln!("Error: {message}");
            return 2;
        }
    };

    let repo_path = match parsed.repo {
        Some(repo) => PathBuf::from(repo),
        None => match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("Error: could not resolve current directory: {error}");
                return 2;
            }
        },
    };
    let engine = match ScorecardEngine::new(&repo_path, None, None) {
        Ok(engine) => engine,
        Err(message) => {
            eprintln!("Error: {message}");
            return 2;
        }
    };
    let inspection = match engine.inspect_tracked(parsed.detectors.as_deref()) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("Error: {message}");
            return 2;
        }
    };
    let rendered = rendered_findings(&inspection, parsed.limit, parsed.full);
    let output = if parsed.format == "json" {
        format_json(&inspection, &rendered, parsed.full)
    } else {
        format_markdown(&inspection, &rendered, parsed.full)
    };
    if let Some(path) = parsed.output {
        if let Err(error) = std::fs::write(&path, output) {
            eprintln!("Error: could not write quality report to {path}: {error}");
            return 2;
        }
    } else {
        println!("{output}");
    }
    0
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        repo: None,
        format: "md".to_string(),
        output: None,
        detectors: None,
        limit: DEFAULT_FINDING_LIMIT,
        full: false,
    };
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        let value = |name: &str| {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("option '{name}' requires an argument"))
        };
        match argument {
            "--help" => return Ok(None),
            "--repo" => {
                parsed.repo = Some(value("--repo")?);
                index += 2;
            }
            "--format" | "-f" => {
                parsed.format = value(argument)?;
                index += 2;
            }
            "--output" | "-o" => {
                parsed.output = Some(value(argument)?);
                index += 2;
            }
            "--detectors" => {
                parsed.detectors = Some(split_detectors(&value("--detectors")?));
                index += 2;
            }
            "--limit" => {
                parsed.limit = parse_limit(&value("--limit")?)?;
                index += 2;
            }
            "--full" => {
                parsed.full = true;
                index += 1;
            }
            other => {
                if let Some(value) = other.strip_prefix("--repo=") {
                    parsed.repo = Some(value.to_string());
                } else if let Some(value) = other.strip_prefix("--format=") {
                    parsed.format = value.to_string();
                } else if let Some(value) = other.strip_prefix("--output=") {
                    parsed.output = Some(value.to_string());
                } else if let Some(value) = other.strip_prefix("--detectors=") {
                    parsed.detectors = Some(split_detectors(value));
                } else if let Some(value) = other.strip_prefix("--limit=") {
                    parsed.limit = parse_limit(value)?;
                } else {
                    return Err(format!("no such option '{other}'"));
                }
                index += 1;
            }
        }
    }
    if !["json", "md"].contains(&parsed.format.as_str()) {
        return Err("--format must be 'json' or 'md'".to_string());
    }
    Ok(Some(parsed))
}

fn split_detectors(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "--limit must be a positive integer".to_string())?;
    if limit == 0 {
        return Err("--limit must be a positive integer".to_string());
    }
    Ok(limit)
}

fn rendered_findings(report: &QualityInspection, limit: usize, full: bool) -> Vec<Finding> {
    let mut findings = report.findings.clone();
    findings.sort_by(compare_findings);
    if !full {
        findings.truncate(limit);
    }
    findings
}

fn compare_findings(left: &Finding, right: &Finding) -> Ordering {
    severity_rank(left.severity)
        .cmp(&severity_rank(right.severity))
        .then_with(|| left.detector.cmp(&right.detector))
        .then_with(|| left.file_path.cmp(&right.file_path))
        .then_with(|| left.line_number.cmp(&right.line_number))
        .then_with(|| left.message.cmp(&right.message))
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Blocker => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

fn quality_level(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker => "high",
        Severity::Warning => "medium",
        Severity::Info => "low",
    }
}

fn format_json(report: &QualityInspection, rendered: &[Finding], full: bool) -> String {
    let mut root = Value::object();
    root.set("schema_version", Value::int(1));
    root.set("kind", Value::str("repository_quality"));
    root.set("advisory_only", Value::Bool(true));

    let mut readiness = Value::object();
    readiness.set("authoritative", Value::Bool(false));
    readiness.set("affects_readiness", Value::Bool(false));
    readiness.set("operational_blockers", Value::Array(Vec::new()));
    readiness.set("command", Value::str("aethyme readiness"));
    root.set("operational_readiness", readiness);

    let mut repository = Value::object();
    repository.set("path", Value::str(report.repository_path.clone()));
    repository.set("source", Value::str("tracked_relevant_files"));
    repository.set(
        "tracked_files",
        Value::int(report.tracked_file_count as i128),
    );
    repository.set(
        "relevant_files",
        Value::int(report.relevant_file_count as i128),
    );
    repository.set(
        "excluded_vendored",
        Value::int(report.excluded_vendored_count as i128),
    );
    repository.set(
        "excluded_generated",
        Value::int(report.excluded_generated_count as i128),
    );
    repository.set(
        "excluded_non_regular",
        Value::int(report.excluded_non_regular_count as i128),
    );
    root.set("repository", repository);

    let mut totals = Value::object();
    totals.set("findings", Value::int(report.total_findings as i128));
    totals.set("high", Value::int(report.high_count as i128));
    totals.set("medium", Value::int(report.medium_count as i128));
    totals.set("low", Value::int(report.low_count as i128));
    totals.set("rendered", Value::int(rendered.len() as i128));
    totals.set(
        "omitted",
        Value::int(report.total_findings.saturating_sub(rendered.len()) as i128),
    );
    totals.set("full_output", Value::Bool(full));
    root.set("totals", totals);

    root.set(
        "detectors",
        Value::Array(
            report
                .detector_results
                .iter()
                .map(|result| {
                    let mut value = Value::object();
                    value.set("name", Value::str(result.detector_name.clone()));
                    value.set("description", Value::str(result.description.clone()));
                    value.set(
                        "status",
                        Value::str(if result.applicability.is_applicable() {
                            "executed"
                        } else {
                            "skipped"
                        }),
                    );
                    value.set(
                        "applicability_reason",
                        Value::str(result.applicability.reason()),
                    );
                    value.set("findings", Value::int(result.findings.len() as i128));
                    value.set(
                        "error",
                        result
                            .error
                            .as_ref()
                            .map_or(Value::Null, |error| Value::str(error.clone())),
                    );
                    value
                })
                .collect(),
        ),
    );
    root.set(
        "findings",
        Value::Array(rendered.iter().map(finding_json).collect()),
    );
    dumps_indent2(&root)
}

fn finding_json(finding: &Finding) -> Value {
    let mut value = Value::object();
    value.set("detector", Value::str(finding.detector.clone()));
    value.set("quality_level", Value::str(quality_level(finding.severity)));
    value.set("operational_blocker", Value::Bool(false));
    value.set("message", Value::str(finding.message.clone()));
    value.set("file", Value::str(finding.file_path.clone()));
    value.set(
        "line",
        finding
            .line_number
            .map_or(Value::Null, |line| Value::int(line as i128)),
    );
    value.set(
        "evidence",
        finding
            .evidence
            .as_ref()
            .map_or(Value::Null, |evidence| Value::str(evidence.clone())),
    );
    value.set(
        "suggestion",
        finding
            .suggestion
            .as_ref()
            .map_or(Value::Null, |suggestion| Value::str(suggestion.clone())),
    );
    value
}

fn format_markdown(report: &QualityInspection, rendered: &[Finding], full: bool) -> String {
    let mut lines = vec![
        "# Repository Quality Inspection".to_string(),
        String::new(),
        "> Advisory repository-quality suggestions only. Run `aethyme readiness` for operational readiness.".to_string(),
        String::new(),
        format!("Repository: `{}`", report.repository_path),
        format!("Tracked/relevant files: {}/{}", report.tracked_file_count, report.relevant_file_count),
        format!("Findings: {} total ({} high, {} medium, {} low)", report.total_findings, report.high_count, report.medium_count, report.low_count),
        format!("Rendered: {}; omitted: {}", rendered.len(), report.total_findings.saturating_sub(rendered.len())),
        String::new(),
        "## Detector applicability".to_string(),
        String::new(),
    ];
    for detector in &report.detector_results {
        let status = if detector.applicability.is_applicable() {
            "executed"
        } else {
            "skipped"
        };
        lines.push(format!(
            "- `{}`: {} — {}",
            detector.detector_name,
            status,
            detector.applicability.reason()
        ));
    }
    lines.extend([
        String::new(),
        "## Quality suggestions".to_string(),
        String::new(),
    ]);
    for finding in rendered {
        let location = finding.line_number.map_or_else(
            || finding.file_path.clone(),
            |line| format!("{}:{line}", finding.file_path),
        );
        lines.push(format!(
            "- **{}** `{}` — {}",
            quality_level(finding.severity),
            location,
            finding.message
        ));
    }
    if rendered.is_empty() {
        lines.push("No suggestions from applicable detectors.".to_string());
    }
    let omitted = report.total_findings.saturating_sub(rendered.len());
    if omitted > 0 && !full {
        lines.extend([
            String::new(),
            format!("{omitted} findings omitted. Rerun with `--full` to render all findings."),
        ]);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InspectionDetectorResult, QualityInspection};

    fn finding(index: usize) -> Finding {
        Finding {
            detector: "test".to_string(),
            severity: Severity::Warning,
            message: format!("finding {index}"),
            file_path: format!("src/{index}.rs"),
            line_number: Some(1),
            evidence: None,
            suggestion: None,
        }
    }

    #[test]
    fn bounded_json_preserves_complete_totals() {
        let findings: Vec<_> = (0..12).map(finding).collect();
        let report = QualityInspection {
            repository_path: "/repo".to_string(),
            tracked_file_count: 12,
            relevant_file_count: 12,
            excluded_vendored_count: 0,
            excluded_generated_count: 0,
            excluded_non_regular_count: 0,
            total_findings: 12,
            high_count: 0,
            medium_count: 12,
            low_count: 0,
            findings: findings.clone(),
            detector_results: vec![InspectionDetectorResult {
                detector_name: "test".to_string(),
                description: "test".to_string(),
                applicability: crate::detectors::DetectorApplicability::Applicable {
                    evidence: "test inputs".to_string(),
                },
                findings,
                execution_time_ms: 0.0,
                error: None,
            }],
            total_scan_time_ms: 0.0,
        };
        let rendered = rendered_findings(&report, 3, false);
        let json = format_json(&report, &rendered, false);
        assert!(json.contains("\"findings\": 12"));
        assert!(json.contains("\"rendered\": 3"));
        assert!(json.contains("\"omitted\": 9"));
        assert!(json.contains("\"operational_blockers\": []"));
    }
}
