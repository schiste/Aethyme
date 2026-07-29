//! Bounded source verification for `aethyme explore` output.
//!
//! `explore` ranks targets; this module only turns selected target rows into
//! small source spans so agents can verify without broad `rg`/`sed` loops.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

pub enum VerifyTargetsCliOutcome {
    Done,
    BadUsage(String),
    Failed(String),
}

#[derive(Debug, Serialize)]
struct VerifyTargetsReport {
    schema_version: &'static str,
    source: String,
    limits: VerifyTargetsLimits,
    targets: Vec<VerifiedTarget>,
    omitted_targets: usize,
    total_line_count: usize,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct VerifyTargetsLimits {
    max_targets: usize,
    max_lines: usize,
}

#[derive(Debug, Serialize)]
struct VerifiedTarget {
    rank: usize,
    source: String,
    kind: String,
    target: String,
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_span: Option<LineSpan>,
    matched_terms: Vec<String>,
    lines: Vec<SourceLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct LineSpan {
    start: usize,
    end: usize,
    line_count: usize,
}

#[derive(Debug, Serialize)]
struct SourceLine {
    line: usize,
    text: String,
}

#[derive(Debug, Clone)]
struct CandidateTarget {
    source: String,
    kind: String,
    target: String,
    path: String,
    reason: String,
    line_refs: Vec<usize>,
}

enum VerifyTargetsError {
    BadUsage(String),
    Failed(String),
}

pub fn run(args: &[String]) -> VerifyTargetsCliOutcome {
    match run_inner(args) {
        Ok(()) => VerifyTargetsCliOutcome::Done,
        Err(VerifyTargetsError::BadUsage(message)) => VerifyTargetsCliOutcome::BadUsage(message),
        Err(VerifyTargetsError::Failed(message)) => VerifyTargetsCliOutcome::Failed(message),
    }
}

fn run_inner(args: &[String]) -> Result<(), VerifyTargetsError> {
    let from = read_option(args, "--from").map_err(VerifyTargetsError::BadUsage)?;
    let repo = read_option(args, "--repo")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let max_targets = read_usize_option(args, "--max-targets", 2)?;
    let max_lines = read_usize_option(args, "--max-lines", 80)?;
    if max_targets == 0 {
        return Err(VerifyTargetsError::BadUsage(
            "verify-targets: --max-targets must be greater than 0".to_string(),
        ));
    }
    if max_lines == 0 {
        return Err(VerifyTargetsError::BadUsage(
            "verify-targets: --max-lines must be greater than 0".to_string(),
        ));
    }

    let repo = repo
        .canonicalize()
        .map_err(|error| VerifyTargetsError::Failed(format!("canonicalize --repo: {error}")))?;
    if !repo.is_dir() {
        return Err(VerifyTargetsError::BadUsage(format!(
            "verify-targets: --repo path is not a directory: {}",
            repo.display()
        )));
    }

    let raw = read_input(&from)?;
    let explore: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| VerifyTargetsError::BadUsage(format!("parse --from JSON: {error}")))?;
    let candidates = dedupe_candidates(collect_candidates(&explore));
    let selected = candidates
        .iter()
        .take(max_targets)
        .cloned()
        .collect::<Vec<_>>();

    let mut remaining_lines = max_lines;
    let mut targets = Vec::new();
    for (index, candidate) in selected.iter().enumerate() {
        if remaining_lines == 0 {
            break;
        }
        let verified = verify_candidate(&repo, &explore, candidate, index + 1, remaining_lines);
        remaining_lines = remaining_lines.saturating_sub(verified.lines.len());
        targets.push(verified);
    }

    let total_line_count = targets
        .iter()
        .map(|target| target.lines.len())
        .sum::<usize>();
    let omitted_targets = candidates.len().saturating_sub(targets.len());
    let report = VerifyTargetsReport {
        schema_version: "aethyme-verify-targets-v1",
        source: from,
        limits: VerifyTargetsLimits {
            max_targets,
            max_lines,
        },
        targets,
        omitted_targets,
        total_line_count,
        truncated: omitted_targets > 0 || total_line_count >= max_lines,
    };
    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| VerifyTargetsError::Failed(format!("serialize report: {error}")))?;
    println!("{json}");
    Ok(())
}

fn collect_candidates(explore: &serde_json::Value) -> Vec<CandidateTarget> {
    let mut out = Vec::new();
    if let Some(subsystems) = explore.get("subsystems").and_then(|value| value.as_array()) {
        let subsystem_targets = subsystems
            .iter()
            .map(|subsystem| {
                subsystem
                    .get("top_verification_targets")
                    .and_then(|value| value.as_array())
            })
            .collect::<Vec<_>>();
        let max_targets_per_subsystem = subsystem_targets
            .iter()
            .filter_map(|targets| targets.map(Vec::len))
            .max()
            .unwrap_or(0);
        for target_index in 0..max_targets_per_subsystem {
            for (subsystem_index, targets) in subsystem_targets.iter().enumerate() {
                let Some(target) = targets.and_then(|targets| targets.get(target_index)) else {
                    continue;
                };
                if let Some(candidate) = candidate_from_target_value(
                    target,
                    format!(
                        "subsystems[{subsystem_index}].top_verification_targets[{target_index}]"
                    ),
                ) {
                    out.push(candidate);
                }
            }
        }
    }

    for key in ["answer", "navigation_hints"] {
        if let Some(items) = explore.get(key).and_then(|value| value.as_array()) {
            for (index, item) in items.iter().enumerate() {
                if let Some(candidate) =
                    candidate_from_target_value(item, format!("{key}[{index}]"))
                {
                    out.push(candidate);
                }
            }
        }
    }
    out
}

fn candidate_from_target_value(
    value: &serde_json::Value,
    source: String,
) -> Option<CandidateTarget> {
    let path = value.get("path").and_then(|path| path.as_str())?;
    if path.trim().is_empty() {
        return None;
    }
    Some(CandidateTarget {
        source,
        kind: value
            .get("kind")
            .and_then(|kind| kind.as_str())
            .unwrap_or("target")
            .to_string(),
        target: value
            .get("target")
            .and_then(|target| target.as_str())
            .unwrap_or(path)
            .to_string(),
        path: path.to_string(),
        reason: value
            .get("reason")
            .and_then(|reason| reason.as_str())
            .unwrap_or("")
            .to_string(),
        line_refs: line_refs_from_value(value),
    })
}

fn line_refs_from_value(value: &serde_json::Value) -> Vec<usize> {
    value
        .get("evidence")
        .and_then(|evidence| evidence.get("line_refs"))
        .and_then(|refs| refs.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|line_ref| {
                    line_ref
                        .get("line")
                        .and_then(|line| line.as_u64())
                        .map(|line| line as usize)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn dedupe_candidates(candidates: Vec<CandidateTarget>) -> Vec<CandidateTarget> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let key = format!("{}\u{1f}{}", candidate.path, candidate.target);
        if seen.insert(key) {
            out.push(candidate);
        }
    }
    out
}

fn verify_candidate(
    repo: &Path,
    explore: &serde_json::Value,
    candidate: &CandidateTarget,
    rank: usize,
    max_lines: usize,
) -> VerifiedTarget {
    let Some(full_path) = resolve_repo_path(repo, &candidate.path) else {
        return unresolved_target(
            candidate,
            rank,
            "target path is outside the repo, generated, or missing".to_string(),
        );
    };
    let Ok(content) = fs::read_to_string(&full_path) else {
        return unresolved_target(
            candidate,
            rank,
            "target file is not valid UTF-8".to_string(),
        );
    };
    let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    if lines.is_empty() {
        return unresolved_target(candidate, rank, "target file is empty".to_string());
    }

    let request = explore
        .get("request")
        .and_then(|request| request.get("raw"))
        .and_then(|raw| raw.as_str())
        .unwrap_or("");
    let terms = verification_terms(candidate, request);
    let anchor = candidate
        .line_refs
        .iter()
        .find_map(|line| line.checked_sub(1))
        .filter(|index| *index < lines.len())
        .or_else(|| best_anchor_line(&lines, &terms));
    let Some(anchor) = anchor else {
        return unresolved_target(
            candidate,
            rank,
            "no source line matched the target label or request terms".to_string(),
        );
    };

    let (start, end) = span_for_anchor(&lines, anchor, &candidate.path, max_lines);
    let span_lines = (start..end)
        .map(|index| SourceLine {
            line: index + 1,
            text: lines[index].clone(),
        })
        .collect::<Vec<_>>();
    let matched_terms = matched_terms_in_span(&span_lines, &terms);
    VerifiedTarget {
        rank,
        source: candidate.source.clone(),
        kind: candidate.kind.clone(),
        target: candidate.target.clone(),
        path: candidate.path.clone(),
        status: "verified_span",
        line_span: Some(LineSpan {
            start: start + 1,
            end,
            line_count: span_lines.len(),
        }),
        matched_terms,
        lines: span_lines,
        note: None,
    }
}

fn unresolved_target(candidate: &CandidateTarget, rank: usize, note: String) -> VerifiedTarget {
    VerifiedTarget {
        rank,
        source: candidate.source.clone(),
        kind: candidate.kind.clone(),
        target: candidate.target.clone(),
        path: candidate.path.clone(),
        status: "unresolved",
        line_span: None,
        matched_terms: Vec::new(),
        lines: Vec::new(),
        note: Some(note),
    }
}

fn verification_terms(candidate: &CandidateTarget, request: &str) -> Vec<String> {
    let mut terms = Vec::new();
    push_terms_from_text(&mut terms, &candidate.target);
    push_terms_from_text(&mut terms, &candidate.reason);
    for part in candidate.target.split("::").skip(1) {
        for cleaned in [
            part,
            part.strip_prefix("def ").unwrap_or(part),
            part.strip_prefix("class ").unwrap_or(part),
        ] {
            push_term(&mut terms, cleaned);
        }
        if let Some(name) = symbol_name_from_declaration(part) {
            push_term(&mut terms, &name);
        }
        if let Some(route) = part.strip_prefix("route:") {
            push_term(&mut terms, route);
        }
    }

    let lower_request = request.to_ascii_lowercase();
    if contains_any(
        &lower_request,
        &["auth", "token", "credential", "api key", "api-key"],
    ) {
        for term in [
            "Authorization",
            "Bearer",
            "token",
            "credential",
            "api_key",
            "api-key",
            "pk_",
            "authenticate",
            "validate",
        ] {
            push_term(&mut terms, term);
        }
    }
    if contains_any(
        &candidate.path.to_ascii_lowercase(),
        &["proxy", "worker", "edge"],
    ) {
        for term in ["fetch", "headers", "Authorization", "Bearer"] {
            push_term(&mut terms, term);
        }
    }
    if contains_any(
        &candidate.path.to_ascii_lowercase(),
        &["api_keys", "auth", "middleware"],
    ) {
        for term in [
            "validate",
            "authenticate",
            "permission",
            "scope",
            "Authorization",
        ] {
            push_term(&mut terms, term);
        }
    }
    terms
}

fn push_terms_from_text(terms: &mut Vec<String>, text: &str) {
    for raw in text
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '/'))
        .filter(|part| part.len() >= 3)
    {
        push_term(terms, raw);
    }
}

fn push_term(terms: &mut Vec<String>, raw: &str) {
    let term = raw
        .trim()
        .trim_matches(|ch: char| ch == ':' || ch == '"' || ch == '\'' || ch == '`')
        .to_string();
    if term.len() < 3 {
        return;
    }
    if terms
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&term))
    {
        return;
    }
    terms.push(term);
}

fn symbol_name_from_declaration(text: &str) -> Option<String> {
    let trimmed = text.trim();
    for prefix in ["def ", "async def ", "class ", "function "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name = rest
                .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn best_anchor_line(lines: &[String], terms: &[String]) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let lower = line.to_ascii_lowercase();
            let score = terms
                .iter()
                .filter(|term| lower.contains(&term.to_ascii_lowercase()))
                .map(|term| {
                    if term.contains('_') || term.contains('/') {
                        8
                    } else {
                        4
                    }
                })
                .sum::<usize>();
            (score > 0).then_some((index, score))
        })
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(index, _)| index)
}

fn span_for_anchor(
    lines: &[String],
    anchor: usize,
    path: &str,
    max_lines: usize,
) -> (usize, usize) {
    let lower_path = path.to_ascii_lowercase();
    let (mut start, mut end) = if lower_path.ends_with(".py") {
        python_span(lines, anchor)
    } else if contains_any(&lower_path, &[".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]) {
        brace_span(lines, anchor)
    } else {
        context_span(lines, anchor, 3, 8)
    };
    if end <= start {
        (start, end) = context_span(lines, anchor, 2, 4);
    }
    if end - start > max_lines {
        end = start + max_lines;
    }
    (start, end.min(lines.len()))
}

fn python_span(lines: &[String], anchor: usize) -> (usize, usize) {
    let mut start = anchor;
    while start > 0 && lines[start - 1].trim_start().starts_with('@') {
        start -= 1;
    }
    let indent = leading_spaces(&lines[anchor]);
    let mut end = anchor + 1;
    while end < lines.len() {
        let trimmed = lines[end].trim();
        if !trimmed.is_empty()
            && leading_spaces(&lines[end]) <= indent
            && (trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with('@'))
        {
            break;
        }
        end += 1;
    }
    (start, end)
}

fn brace_span(lines: &[String], anchor: usize) -> (usize, usize) {
    let start = (0..=anchor)
        .rev()
        .find(|index| {
            let line = lines[*index].trim();
            line.contains("async fetch")
                || line.contains("function ")
                || line.contains("export default")
                || line.ends_with("=> {")
        })
        .unwrap_or(anchor.saturating_sub(2));
    let mut depth = 0isize;
    let mut saw_open = false;
    let mut end = start + 1;
    for (index, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    saw_open = true;
                    depth += 1;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        end = index + 1;
        if saw_open && depth <= 0 && index >= anchor {
            break;
        }
    }
    (start, end)
}

fn context_span(lines: &[String], anchor: usize, before: usize, after: usize) -> (usize, usize) {
    (
        anchor.saturating_sub(before),
        (anchor + after + 1).min(lines.len()),
    )
}

fn matched_terms_in_span(lines: &[SourceLine], terms: &[String]) -> Vec<String> {
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| text.contains(&term.to_ascii_lowercase()))
        .take(12)
        .cloned()
        .collect()
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn resolve_repo_path(repo: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    if rel_path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| matches!(part, ".aethyme" | ".chau7"))
    }) {
        return None;
    }
    let full = repo.join(rel_path).canonicalize().ok()?;
    full.starts_with(repo).then_some(full)
}

fn read_input(from: &str) -> Result<String, VerifyTargetsError> {
    if from == "-" {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| VerifyTargetsError::Failed(format!("read stdin: {error}")))?;
        return Ok(input);
    }
    fs::read_to_string(from)
        .map_err(|error| VerifyTargetsError::Failed(format!("read --from {from}: {error}")))
}

fn read_usize_option(
    args: &[String],
    flag: &str,
    default: usize,
) -> Result<usize, VerifyTargetsError> {
    match read_option(args, flag) {
        Ok(raw) => raw.parse::<usize>().map_err(|error| {
            VerifyTargetsError::BadUsage(format!("{flag} must be a number: {error}"))
        }),
        Err(_) => Ok(default),
    }
}

fn read_option(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing required option: {flag}"))
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_python_block_from_target_label() {
        let lines = vec![
            "import os".to_string(),
            "".to_string(),
            "@decorator".to_string(),
            "def validate_publishable_key(raw_token):".to_string(),
            "    if not raw_token.startswith(\"pk_\"):".to_string(),
            "        raise PermissionError()".to_string(),
            "    return raw_token".to_string(),
            "".to_string(),
            "def other():".to_string(),
            "    pass".to_string(),
        ];
        let (start, end) = python_span(&lines, 3);
        assert_eq!((start, end), (2, 8));
    }

    #[test]
    fn rejects_generated_artifact_paths() {
        let repo = Path::new("/tmp/repo");
        assert!(resolve_repo_path(repo, ".aethyme/graph_store.redb").is_none());
        assert!(resolve_repo_path(repo, "../outside.py").is_none());
    }
}
