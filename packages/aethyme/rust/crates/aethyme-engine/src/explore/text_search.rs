//! Source-text search via ripgrep — Rust port of
//! `_task_localization_text_items` from cli.py.
//!
//! Strategy: shell out to ripgrep for the heavy lifting (file walking,
//! suffix filtering, gitignore-respecting traversal, multi-pattern match)
//! and do the scoring + ranking in Rust. Ripgrep at 161ms on Mockup for a
//! single term means the whole multi-term pass lands well under 1s — fast
//! enough that we don't need to background it.
//!
//! What we do NOT port from the Python helper (yet, deferred to later
//! sessions): per-line symbol clustering, file-role classification, the
//! elaborate `_text_candidate_score` weighting heuristic. This port is a
//! correct-but-simpler version: per-file hit count × distinct term
//! coverage, with a cap on the line-ref preview list to keep the
//! response token-cheap.

use std::path::Path;

use super::{AnswerItem, extract_symbol_queries};

const RIPGREP_BIN: &str = "rg";
const SOURCE_TEXT_FILE_SIZE_CAP_BYTES: u64 = 750_000;

#[derive(Debug, Clone)]
struct TextHit {
    path: String,
    matched_terms: std::collections::BTreeSet<String>,
    hit_count: usize,
    line_refs: Vec<TextLineRef>,
}

#[derive(Debug, Clone)]
struct TextLineRef {
    line: u64,
    text: String,
    matched_terms: Vec<String>,
}

/// Build the term list for source-text search. Wider than
/// `extract_symbol_queries` — keeps behavioural words ("view", "seen")
/// that are too noisy for symbol search but useful for line-level
/// evidence. Mirrors `_request_text_search_terms` in cli.py.
pub(crate) fn extract_text_search_terms(request: &str) -> Vec<String> {
    let mut terms = extract_symbol_queries(request);
    let lowered = request.to_ascii_lowercase();
    let mut extras: Vec<&str> = Vec::new();
    if lowered.contains("watchlist") || lowered.contains("watchlisted") {
        extras.extend(["watchlist", "watchlisted", "watched", "notification"]);
    }
    if lowered.contains("seen") {
        extras.extend(["seen", "notification", "timestamp"]);
    }
    if lowered.contains("view") {
        extras.extend(["view", "viewed", "viewing"]);
    }
    if lowered.contains("diff") {
        extras.extend(["diff", "difference", "diffonly"]);
    }
    if lowered.contains("revision") || lowered.contains("oldid") {
        extras.extend(["revision", "revisions", "oldid"]);
    }
    let mut seen: std::collections::HashSet<String> = terms
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    for extra in extras {
        let lower = extra.to_ascii_lowercase();
        if seen.insert(lower) {
            terms.push(extra.to_string());
        }
    }
    terms
}

/// Walk the repo with ripgrep across all terms in one pass, score each
/// matching file by hit count × distinct-term coverage, return up to
/// `max_files` candidates.
pub(super) fn source_text_files(
    repo: &Path,
    terms: &[String],
    max_files: usize,
    max_line_refs: usize,
) -> Vec<AnswerItem> {
    if terms.is_empty() || max_files == 0 {
        return Vec::new();
    }

    let mut hits_by_file: std::collections::BTreeMap<String, TextHit> =
        std::collections::BTreeMap::new();

    for chunk in terms.chunks(8) {
        let pattern = chunk
            .iter()
            .map(|t| regex::escape(t))
            .collect::<Vec<_>>()
            .join("|");
        if pattern.is_empty() {
            continue;
        }
        let lowered_terms: Vec<String> =
            chunk.iter().map(|t| t.to_ascii_lowercase()).collect();
        let output = match std::process::Command::new(RIPGREP_BIN)
            .arg("-i")
            .arg("--no-heading")
            .arg("--with-filename")
            .arg("--line-number")
            .arg("--max-filesize")
            .arg(SOURCE_TEXT_FILE_SIZE_CAP_BYTES.to_string())
            .arg("--no-messages")
            .arg("-e")
            .arg(&pattern)
            .arg(repo)
            .output()
        {
            Ok(o) => o,
            Err(_) => continue,
        };
        if !output.stdout.is_empty() {
            ingest_rg_output(
                &output.stdout,
                repo,
                &lowered_terms,
                &mut hits_by_file,
            );
        }
    }

    let mut ranked: Vec<TextHit> = hits_by_file.into_values().collect();
    // Sort by composite score: (suffix_class_rank desc, distinct_terms desc,
    // hit_count desc, path asc). suffix_class_rank pushes executable source
    // ahead of locale data / changelogs so the agent doesn't see a wall of
    // i18n JSON files when their query terms happen to be common words.
    // Mirrors the Python helper's role-aware penalty without porting the
    // full heuristic (deferred to session 4+).
    ranked.sort_by(|a, b| {
        suffix_class_rank(&b.path)
            .cmp(&suffix_class_rank(&a.path))
            .then_with(|| b.matched_terms.len().cmp(&a.matched_terms.len()))
            .then_with(|| b.hit_count.cmp(&a.hit_count))
            .then_with(|| a.path.cmp(&b.path))
    });

    ranked
        .into_iter()
        .take(max_files)
        .map(|hit| {
            let matched_count = hit.matched_terms.len();
            let confidence = if matched_count >= 3 {
                0.84
            } else if matched_count == 2 {
                0.78
            } else {
                0.70
            };
            let reason = if matched_count >= 2 {
                "Source text matched multiple request terms in executable code; \
                 line refs are evidence, not filename-only hints."
            } else {
                "Source text matched one request term; verify the line context \
                 before treating as authoritative."
            };
            let mut line_refs: Vec<&TextLineRef> = hit.line_refs.iter().collect();
            // Highest-scoring lines = most distinct matched terms,
            // then earliest line number for stability.
            line_refs.sort_by(|a, b| {
                b.matched_terms
                    .len()
                    .cmp(&a.matched_terms.len())
                    .then_with(|| a.line.cmp(&b.line))
            });
            let line_refs_json: Vec<serde_json::Value> = line_refs
                .into_iter()
                .take(max_line_refs)
                .map(|r| {
                    serde_json::json!({
                        "line": r.line,
                        "text": r.text,
                        "matched_terms": r.matched_terms,
                    })
                })
                .collect();
            AnswerItem {
                kind: "source_text_file".into(),
                target: hit.path.clone(),
                path: Some(hit.path),
                status: "candidate".into(),
                confidence,
                reason: reason.into(),
                role: "candidate".into(),
                evidence: serde_json::json!({
                    "source": "source-text-search",
                    "matched_terms": hit.matched_terms.iter().collect::<Vec<_>>(),
                    "hit_count": hit.hit_count,
                    "line_refs": line_refs_json,
                }),
            }
        })
        .collect()
}

/// Coarse file-class ranking for source-text matches. Higher is better.
///
/// The query "find logic" matches lots of locale JSON files because every
/// translated string contains "logic". Without a class signal, those
/// files swamp `answer[]`. We rank executable source highest, then docs,
/// then changelogs/data lowest. Inside a class, finer ranking falls back
/// to term coverage and hit count.
///
/// This is intentionally simpler than the Python helper's weighting (which
/// combines file role, path patterns, enclosing-symbol presence, etc).
/// Captures the 80% case at 20% of the code.
fn suffix_class_rank(path: &str) -> i32 {
    let lower = path.to_ascii_lowercase();
    // Strong demote: locale/translation files. The `/locales/` segment is
    // the canonical pattern across most monorepos.
    if lower.contains("/locales/")
        || lower.contains("/locale/")
        || lower.contains("/i18n/")
        || lower.contains("/translations/")
    {
        return 0;
    }
    // Top-level data / metadata files. Match common request terms but
    // rarely the actual answer.
    let basename = lower.rsplit('/').next().unwrap_or(&lower);
    if basename == "changelog.md"
        || basename == "history.md"
        || basename == "package-lock.json"
        || basename == "yarn.lock"
        || basename == "pnpm-lock.yaml"
    {
        return 1;
    }
    // Test-file demote MUST be checked BEFORE the source-code arm —
    // a test file in a source language (test_foo.py, auth.spec.ts)
    // would otherwise hit the rank-5 source arm and ignore the
    // "tests rank slightly lower" intent.
    let is_test = lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.");
    let suffix = lower.rsplit('.').next().unwrap_or("");
    let is_source = matches!(
        suffix,
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "mjs"
            | "cjs"
            | "vue"
            | "svelte"
    );
    if is_source && is_test {
        return 4;
    }
    if is_source {
        return 5;
    }
    if is_test {
        // Non-source test fixture (.json, .yaml, etc) — weaker than
        // tests in source languages but still has signal.
        return 3;
    }
    match suffix {
        "md" | "rst" | "adoc" | "txt" => 3,
        "yml" | "yaml" | "toml" | "ini" | "conf" | "config" => 2,
        // Generic JSON / data — common text matches but weak signal.
        _ => 1,
    }
}

fn ingest_rg_output(
    stdout: &[u8],
    repo: &Path,
    lowered_terms: &[String],
    hits_by_file: &mut std::collections::BTreeMap<String, TextHit>,
) {
    let text = match std::str::from_utf8(stdout) {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in text.lines() {
        // ripgrep default format: <path>:<line>:<text>
        let mut parts = line.splitn(3, ':');
        let abs_path = match parts.next() {
            Some(p) => p,
            None => continue,
        };
        let line_no: u64 = match parts.next().and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None => continue,
        };
        let line_text = parts.next().unwrap_or("");
        let rel_path = match Path::new(abs_path).strip_prefix(repo) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => abs_path.to_string(),
        };
        let lower = line_text.to_ascii_lowercase();
        let matched: Vec<String> = lowered_terms
            .iter()
            .filter(|t| lower.contains(t.as_str()))
            .cloned()
            .collect();
        if matched.is_empty() {
            // `rg` matched but our local lowercase scan missed (rare —
            // could happen with regex metachar quirks). Skip.
            continue;
        }
        let entry = hits_by_file
            .entry(rel_path.clone())
            .or_insert_with(|| TextHit {
                path: rel_path,
                matched_terms: std::collections::BTreeSet::new(),
                hit_count: 0,
                line_refs: Vec::new(),
            });
        for term in &matched {
            entry.matched_terms.insert(term.clone());
        }
        entry.hit_count += 1;
        // Cap stored line refs per file to bound memory; the ranking
        // step picks the top N by term coverage afterwards.
        if entry.line_refs.len() < 32 {
            entry.line_refs.push(TextLineRef {
                line: line_no,
                text: line_text.trim().chars().take(220).collect(),
                matched_terms: matched,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for `suffix_class_rank` — moved here from
    //! explore.rs alongside the function in the 2026-05-08 split.

    use super::suffix_class_rank;

    #[test]
    fn suffix_class_rank_demotes_source_language_tests() {
        // Source-language test files should rank BELOW non-test source
        // (4 vs 5). Pre-bugfix, the test arm was unreachable for these.
        assert!(suffix_class_rank("src/auth.rs") > suffix_class_rank("tests/auth_test.rs"));
        assert!(suffix_class_rank("backend/grader.py") > suffix_class_rank("backend/tests/test_grader.py"));
        assert!(suffix_class_rank("packages/auth/src/login.ts") > suffix_class_rank("packages/auth/src/login.test.ts"));
    }

    #[test]
    fn suffix_class_rank_orders_categories_correctly() {
        let source = suffix_class_rank("src/foo.rs");
        let test = suffix_class_rank("tests/foo_test.rs");
        let docs = suffix_class_rank("README.md");
        let config = suffix_class_rank("config.yml");
        let data = suffix_class_rank("data/users.json");
        let locale = suffix_class_rank("packages/app/locales/en.json");
        let lockfile = suffix_class_rank("package-lock.json");
        assert!(source > test);
        assert!(test > docs);
        assert!(docs > config);
        assert!(config > data);
        assert!(data >= lockfile);
        assert!(lockfile > locale);
    }
}
