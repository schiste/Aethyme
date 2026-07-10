//! Filename-token matching — Rust port of
//! `_task_localization_filesystem_items` from cli.py.
//!
//! Catches the case where the relevant file's NAME contains the request
//! terms but its symbols don't. Example from the Mockup measurement:
//! query "find suppliers grader" — `suppliers_grader.py` is the obvious
//! answer, but its functions are named `_default_graders` etc. Symbol
//! search misses the file; filename matching catches it.
//!
//! Output goes to `navigation_hints[]`, NOT `answer[]`: a filename-only
//! match is a hint to look at, not authoritative evidence. Confidence
//! stays low (0.28-0.38). Mirrors Python's
//! `_task_localization_filesystem_items` contract.

use std::path::Path;

use super::AnswerItem;

const FILENAME_ALLOWED_SUFFIXES: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx", "kt", "mjs", "php", "py", "rb",
    "rs", "swift", "ts", "tsx", "vue",
];

// Local copy of the ripgrep binary path. Also defined in
// explore::text_search; intentionally duplicated as a one-liner since
// both modules use ripgrep independently and a constant doesn't merit
// a shared module.
const RIPGREP_BIN: &str = "rg";

pub(super) fn filename_token_matches(
    repo: &Path,
    terms: &[String],
    max_items: usize,
) -> Vec<AnswerItem> {
    if terms.is_empty() || max_items == 0 {
        return Vec::new();
    }
    // `rg --files` walks the repo respecting gitignore, returns one
    // path per line. Way faster than std::fs traversal on 7K-file repos
    // and skips junk (.venv, node_modules, etc) by default.
    let output = match std::process::Command::new(RIPGREP_BIN)
        .arg("--files")
        .arg("--no-messages")
        .arg(repo)
        .output()
    {
        Ok(o) if o.status.success() || !o.stdout.is_empty() => o,
        _ => return Vec::new(),
    };
    let stdout = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let lowered_terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    let mut scored: Vec<(i32, Vec<String>, String)> = Vec::new();
    for abs_line in stdout.lines() {
        if abs_line.is_empty() {
            continue;
        }
        let abs = Path::new(abs_line);
        // Suffix gate: only consider source-code files.
        let suffix = abs
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        match suffix.as_deref() {
            Some(s) if FILENAME_ALLOWED_SUFFIXES.contains(&s) => {}
            _ => continue,
        }
        let rel_path = match abs.strip_prefix(repo) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let (score, matched) = filename_match_score(&rel_path, &lowered_terms);
        if score <= 0 {
            continue;
        }
        scored.push((score, matched, rel_path));
    }

    // Sort by score descending, prefer SHORTER paths within same score
    // (less-nested = more likely to be the canonical home of the
    // concept), then alphabetical for stability.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.2.len().cmp(&b.2.len()))
            .then_with(|| a.2.cmp(&b.2))
    });
    scored.truncate(max_items);

    scored
        .into_iter()
        .map(|(score, matched_terms, rel_path)| {
            let multi = matched_terms.len() > 1;
            let confidence = if multi { 0.38 } else { 0.28 };
            AnswerItem {
                kind: "filesystem_file".into(),
                target: rel_path.clone(),
                path: Some(rel_path),
                status: "navigation_hint".into(),
                confidence,
                reason: "Filename-only match. Use as a search/navigation hint, \
                         not as primary answer evidence."
                    .into(),
                role: "navigation_filename".into(),
                evidence: serde_json::json!({
                    "source": "filesystem-filename",
                    "matched_terms": matched_terms,
                    "score": score,
                }),
            }
        })
        .collect()
}

/// Score a path against query terms, mirroring Python's
/// `_filesystem_match_score`. Higher = stronger filename signal.
///   - exact stem match:    +20
///   - stem prefix:         +12
///   - substring in stem:    +8
///   - substring in basename:+5
///   - substring in path:    +2
fn filename_match_score(path: &str, lowered_terms: &[String]) -> (i32, Vec<String>) {
    let lowered_path = path.to_ascii_lowercase();
    let filename = Path::new(&lowered_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&lowered_path)
        .to_string();
    let stem = Path::new(&lowered_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&filename)
        .to_string();

    let mut score = 0;
    let mut matched: Vec<String> = Vec::new();
    for term in lowered_terms {
        if term == &stem {
            score += 20;
            matched.push(term.clone());
        } else if stem.starts_with(term) {
            score += 12;
            matched.push(term.clone());
        } else if stem.contains(term) {
            score += 8;
            matched.push(term.clone());
        } else if filename.contains(term) {
            score += 5;
            matched.push(term.clone());
        } else if lowered_path.contains(term) {
            score += 2;
            matched.push(term.clone());
        }
    }
    (score, matched)
}
