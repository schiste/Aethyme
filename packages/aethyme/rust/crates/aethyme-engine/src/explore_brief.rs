//! `aethyme explore --format brief`: one call that prints the compact
//! decision surface and the top verified source spans.
//!
//! The deployed guidance used to prescribe three commands over a temp file:
//! `explore --format answer-json > "$F"`, `explore-summary --from "$F"`, and
//! `verify-targets --from "$F" --max-targets 2 --max-lines 80`. Each one was
//! an agent turn, and turns — not bytes — are what dominate an agent's cost.
//! This format folds the three into the one call an agent actually needs for
//! a "where is X" question (recovery plan P4.6).
//!
//! It is text for an agent to read, not a data contract: `answer-json`
//! remains the default format and the machine surface, and `explore-summary`
//! and `verify-targets` keep working over a saved answer-json for callers
//! that want the JSON projections. The brief reads the SAME answer document
//! the JSON format prints, so the two cannot disagree about what was found.
//!
//! Every brief opens by saying the result is a navigation aid that must be
//! verified. That line is deliberate and must not be softened: Explore's
//! ranking is not authoritative, and the spans below it are what the agent
//! verifies against.

use std::path::Path;

use serde_json::Value;

use crate::verify_targets_cli::{VerifiedTarget, verify_top_targets};

/// Spans printed after the summary.
pub const BRIEF_MAX_TARGETS: usize = 2;
/// Source lines per printed span.
pub const BRIEF_MAX_LINES_PER_TARGET: usize = 80;
/// Subsystem lanes listed in the summary.
const BRIEF_MAX_LANES: usize = 3;
/// Verification steps listed in the summary.
const BRIEF_MAX_STEPS: usize = 2;
/// Longest reason text quoted on one summary line.
const REASON_PREVIEW_CHARS: usize = 160;

/// Render the brief for one answer document.
pub fn render(repo: &Path, answer: &Value) -> String {
    let mut out = Vec::new();
    out.push(header(answer));
    if let Some(reason) = answer
        .pointer("/trust_policy/reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
    {
        out.push(format!("Why: {}", preview(reason)));
    }

    let lanes = answer
        .get("subsystems")
        .and_then(Value::as_array)
        .map(|lanes| lanes.iter().take(BRIEF_MAX_LANES).collect::<Vec<_>>())
        .unwrap_or_default();
    if !lanes.is_empty() {
        out.push("Subsystems:".to_string());
        for lane in lanes {
            out.push(format!("  {}", lane_line(lane)));
        }
    }

    let steps = answer
        .get("verification_steps")
        .and_then(Value::as_array)
        .map(|steps| steps.iter().take(BRIEF_MAX_STEPS).collect::<Vec<_>>())
        .unwrap_or_default();
    for step in steps {
        let kind = step.get("kind").and_then(Value::as_str).unwrap_or("step");
        let reason = step.get("reason").and_then(Value::as_str).unwrap_or("");
        out.push(format!("Verify: {kind} — {}", preview(reason)));
    }

    let (targets, omitted) =
        verify_top_targets(repo, answer, BRIEF_MAX_TARGETS, BRIEF_MAX_LINES_PER_TARGET);
    if targets.is_empty() {
        out.push(
            "No verifiable target: Explore found nothing to check. Fall back to a narrow \
             search, and do not treat this as evidence of absence."
                .to_string(),
        );
    }
    for target in &targets {
        out.push(String::new());
        out.extend(span_block(target));
    }
    if omitted > 0 {
        out.push(String::new());
        out.push(format!(
            "{omitted} more ranked target(s) not shown; `--format answer-json` lists them all."
        ));
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn header(answer: &Value) -> String {
    let safe = answer
        .get("safe_to_use_as_answer")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let trust = answer
        .pointer("/trust_policy/trust_policy")
        .and_then(Value::as_str)
        .unwrap_or("verify_before_use");
    let readiness = answer
        .pointer("/observability/readiness/status")
        .and_then(Value::as_str);
    let mode = answer
        .pointer("/observability/readiness/mode")
        .and_then(Value::as_str);
    let status = answer.get("status").and_then(Value::as_str);
    let mut facts = vec![
        format!("trust={trust}"),
        format!("safe_to_use_as_answer={safe}"),
    ];
    if let Some(status) = status {
        facts.push(format!("status={status}"));
    }
    match (readiness, mode) {
        (Some(readiness), Some(mode)) => facts.push(format!("readiness={readiness} ({mode})")),
        (Some(readiness), None) => facts.push(format!("readiness={readiness}")),
        _ => {}
    }
    format!(
        "Explore — a navigation aid, not an answer; verify the spans below before relying on them. {}",
        facts.join(", ")
    )
}

fn lane_line(lane: &Value) -> String {
    let rank = lane
        .get("rank")
        .and_then(Value::as_u64)
        .map(|rank| format!("{rank}. "))
        .unwrap_or_default();
    let label = lane
        .get("label")
        .and_then(Value::as_str)
        .or_else(|| lane.get("id").and_then(Value::as_str))
        .unwrap_or("(unnamed)");
    let mut facts = Vec::new();
    if let Some(role) = lane.get("role").and_then(Value::as_str) {
        facts.push(role.to_string());
    }
    if let Some(confidence) = lane.get("confidence").and_then(Value::as_f64) {
        facts.push(format!("confidence {confidence:.2}"));
    }
    let warnings = lane
        .get("missing_coverage_warnings")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if warnings > 0 {
        facts.push(format!("{warnings} coverage warning(s)"));
    }
    if facts.is_empty() {
        format!("{rank}{label}")
    } else {
        format!("{rank}{label} [{}]", facts.join(", "))
    }
}

fn span_block(target: &VerifiedTarget) -> Vec<String> {
    let mut lines = Vec::new();
    match &target.line_span {
        Some(span) => {
            let matched = if target.matched_terms.is_empty() {
                String::new()
            } else {
                format!("; matched: {}", target.matched_terms.join(", "))
            };
            lines.push(format!(
                "── {}. {}:{}-{} ({}{matched})",
                target.rank, target.path, span.start, span.end, target.status
            ));
            let width = span.end.to_string().len();
            for source in &target.lines {
                lines.push(format!("{:>width$}  {}", source.line, source.text));
            }
        }
        None => {
            lines.push(format!(
                "── {}. {} ({}: {})",
                target.rank,
                target.path,
                target.status,
                target.note.as_deref().unwrap_or("no span")
            ));
        }
    }
    lines
}

fn preview(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= REASON_PREVIEW_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(REASON_PREVIEW_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let body = (1..=200)
            .map(|n| format!("// filler line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            dir.path().join("src/hook.ts"),
            format!("function onSessionStart() {{\n{body}\n}}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/other.rs"),
            "fn session_start_entry() {}\n",
        )
        .unwrap();
        dir
    }

    fn answer() -> Value {
        serde_json::json!({
            "status": "degraded",
            "request": {"raw": "where is session start"},
            "safe_to_use_as_answer": false,
            "trust_policy": {
                "trust_policy": "verify_before_use",
                "reason": "The graph is unavailable. Ranked source-search hints are navigation only."
            },
            "subsystems": [{
                "rank": 1,
                "id": "bounded_source_fallback",
                "label": "Source locations to verify",
                "role": "navigation_only",
                "confidence": 0.7,
                "missing_coverage_warnings": ["No graph"],
                "top_verification_targets": [
                    {"kind": "source_file", "target": "src/hook.ts", "path": "src/hook.ts",
                     "reason": "defines `onSessionStart` (function) at line 1"},
                    {"kind": "source_file", "target": "src/other.rs", "path": "src/other.rs",
                     "reason": "defines `session_start_entry` (function) at line 1"},
                    {"kind": "source_file", "target": "src/missing.rs", "path": "src/missing.rs",
                     "reason": "third"}
                ]
            }],
            "verification_steps": [
                {"kind": "manual_source_inspection", "reason": "Verify the ranked source spans"}
            ],
            "observability": {"readiness": {"status": "ready", "mode": "bounded_content_search"}}
        })
    }

    #[test]
    fn brief_says_it_is_not_authoritative_and_prints_two_spans() {
        let repo = fixture_repo();
        let text = render(&repo.path().canonicalize().unwrap(), &answer());
        let first = text.lines().next().unwrap();
        assert!(first.contains("navigation aid, not an answer"), "{text}");
        assert!(first.contains("verify the spans"), "{text}");
        assert!(first.contains("safe_to_use_as_answer=false"), "{text}");
        assert!(
            first.contains("readiness=ready (bounded_content_search)"),
            "{text}"
        );
        assert!(text.contains("Why: The graph is unavailable."), "{text}");
        assert!(
            text.contains("  1. Source locations to verify [navigation_only, confidence 0.70, 1 coverage warning(s)]"),
            "{text}"
        );
        assert!(text.contains("Verify: manual_source_inspection"), "{text}");
        let spans = text.lines().filter(|line| line.starts_with("── ")).count();
        assert_eq!(spans, BRIEF_MAX_TARGETS, "{text}");
        assert!(text.contains("── 1. src/hook.ts:1-"), "{text}");
        assert!(text.contains("function onSessionStart()"), "{text}");
        assert!(text.contains("more ranked target(s) not shown"), "{text}");
    }

    #[test]
    fn each_span_is_capped_per_target() {
        let repo = fixture_repo();
        let text = render(&repo.path().canonicalize().unwrap(), &answer());
        let mut current = 0usize;
        let mut longest = 0usize;
        for line in text.lines() {
            if line.starts_with("── ") || line.is_empty() {
                longest = longest.max(current);
                current = 0;
            } else if line
                .split_whitespace()
                .next()
                .is_some_and(|token| token.parse::<usize>().is_ok())
            {
                current += 1;
            }
        }
        longest = longest.max(current);
        // The fixture function is 202 lines long, so the cap is what ends it.
        assert_eq!(
            longest, BRIEF_MAX_LINES_PER_TARGET,
            "{longest} lines\n{text}"
        );
    }

    #[test]
    fn an_empty_answer_still_warns_and_does_not_claim_absence() {
        let repo = fixture_repo();
        let text = render(repo.path(), &serde_json::json!({}));
        assert!(text.contains("navigation aid, not an answer"), "{text}");
        assert!(
            text.contains("do not treat this as evidence of absence"),
            "{text}"
        );
    }
}
