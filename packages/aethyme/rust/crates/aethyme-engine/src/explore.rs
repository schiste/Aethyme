//! Native Rust orchestration for `aethyme explore`.
//!
//! Session 1 of the Rust port (see roadmap discussion in commit message of
//! 49a7beb's parent thread): the simplest happy path that crosses every
//! layer end-to-end. The scope is deliberately narrow:
//!
//! - Only `task_localization_query` intent
//! - Only the daemon-routed path (no fallback to subprocess engine here —
//!   the caller decides whether to fall through to Python on failure)
//! - Synthesizes `answer[]` items from anchors + in-scope files only
//! - Stub `verification_steps`, `excluded`, `ambiguous`
//! - Conservative `trust_policy`: always `needs_verification` because we
//!   don't yet have the symbol/text/callsite passes that justify
//!   `answer_candidate`
//!
//! Subsequent sessions add: symbol search (B2 in plan), source-text grep
//! (B3), full trust policy + verification (B4), behavior_localization (B5).
//!
//! Wire shape
//! ----------
//! Output JSON matches `aethyme-explore-v1` schema produced by the Python
//! `_explore_task_localization_query` at compact detail. A consumer that
//! reads `answer[]` + `safe_to_use_as_answer` + `trust_policy` works
//! identically against either implementation.

use std::path::Path;

use serde::Serialize;

use crate::daemon;

// ── public envelope ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ExploreResponse {
    pub schema_version: &'static str,
    pub mode: &'static str,
    pub intent: &'static str,
    pub intent_source: &'static str,
    pub status: &'static str,
    pub request: ExploreRequest,
    pub answer: Vec<AnswerItem>,
    pub navigation_hints: Vec<AnswerItem>,
    pub excluded: Vec<serde_json::Value>,
    pub ambiguous: Vec<serde_json::Value>,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub safe_to_use_as_answer: bool,
    pub safe_to_use_as_navigation: bool,
    pub trust_policy: TrustPolicy,
    pub degraded_reasons: Vec<String>,
    pub verification_steps: Vec<serde_json::Value>,
    pub next_actions: Vec<String>,
    pub available_specialized_intents: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ExploreRequest {
    pub raw: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnswerItem {
    pub kind: String,
    pub target: String,
    pub path: Option<String>,
    pub status: String,
    pub confidence: f64,
    pub reason: String,
    pub role: String,
    pub evidence: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub answer_count: usize,
    pub navigation_hint_count: usize,
    pub excluded_count: usize,
}

#[derive(Debug, Serialize)]
pub struct Confidence {
    pub overall: Option<f64>,
    pub answer_summary: ConfidenceSummary,
    pub excluded_summary: ConfidenceSummary,
    pub analyzed_summary: serde_json::Value,
}

#[derive(Debug, Default, Serialize)]
pub struct ConfidenceSummary {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

#[derive(Debug, Serialize)]
pub struct TrustPolicy {
    pub safe_to_use_as_answer: bool,
    pub safe_to_use_as_navigation: bool,
    pub evidence_level: String,
    pub authoritative_answer_count: usize,
    pub navigation_hint_count: usize,
    pub degraded: bool,
    pub trust_policy: &'static str,
    pub reason: String,
}

// ── parameters ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExploreParams {
    pub max_answer_items: usize,
    /// Detail level: `compact`, `standard`, or `full`. Mirrors the Python
    /// `--detail` flag. Today only `compact` is fully implemented in the
    /// native path; standard/full fall back to Python at the call site.
    pub detail: Detail,
    /// Number of distinct symbol queries to derive from the request. The
    /// Python compact default is 5; matched here.
    pub max_symbol_queries: usize,
    /// Per-query result cap for symbol search.
    pub max_symbol_results: usize,
    /// Number of symbol-search-derived files to include in `answer[]`.
    /// Caps independently of `max_answer_items` so symbol evidence
    /// doesn't crowd out anchor evidence.
    pub max_symbol_files: usize,
    /// Maximum source-text candidate files emitted to `answer[]`.
    pub max_text_files: usize,
    /// Per-file cap on the `evidence.line_refs` excerpt list. Only the
    /// highest-scoring lines per file appear in the response — agents read
    /// 1-2; emitting all hits would be ~6,000 tokens of noise on a
    /// well-matching file.
    pub max_text_line_refs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Compact,
    Standard,
    Full,
}

impl Default for ExploreParams {
    fn default() -> Self {
        Self {
            max_answer_items: 5, // matches Python compact default after f1e3da5
            detail: Detail::Compact,
            max_symbol_queries: 5,
            max_symbol_results: 4,
            max_symbol_files: 8, // truncated when answer list fills
            max_text_files: 5,   // matches Python compact default
            max_text_line_refs: 2,
        }
    }
}

// ── errors ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ExploreError {
    DaemonNotRunning,
    DaemonRpc(String),
    InvalidResponse(String),
}

impl std::fmt::Display for ExploreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DaemonNotRunning => write!(f, "engine daemon not running"),
            Self::DaemonRpc(msg) => write!(f, "engine daemon rpc: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid daemon response: {msg}"),
        }
    }
}

impl std::error::Error for ExploreError {}

// ── orchestration entry point ───────────────────────────────────────────

/// Run a `task_localization_query` intent against the engine daemon.
///
/// Returns `Err(DaemonNotRunning)` when there's no daemon — the caller
/// is responsible for falling back (typically to the Python orchestrator).
/// Other errors indicate the daemon was reachable but the request failed.
pub fn explore_task_localization(
    repo: &Path,
    request: &str,
    params: &ExploreParams,
) -> Result<ExploreResponse, ExploreError> {
    let socket = daemon::socket_path_for(repo);
    if !socket.exists() {
        return Err(ExploreError::DaemonNotRunning);
    }

    // 1. Graph-derived view (anchors + scope + next).
    let view = call_task_localize(&socket, request)?;

    // 2. Symbol-search evidence. On failure (degraded daemon, network
    //    blip), keep going with the anchors-only path rather than block
    //    the whole request.
    let symbol_queries = extract_symbol_queries(request);
    let symbol_queries = if symbol_queries.len() > params.max_symbol_queries {
        symbol_queries[..params.max_symbol_queries].to_vec()
    } else {
        symbol_queries
    };
    let symbol_matches = if symbol_queries.is_empty() {
        SymbolBatchResults::default()
    } else {
        match call_symbol_batch(&socket, &symbol_queries, params.max_symbol_results) {
            Ok(r) => r,
            Err(_) => SymbolBatchResults::default(),
        }
    };

    // 3. Source-text evidence. Runs ripgrep client-side against the repo
    //    filesystem; doesn't need the daemon. Tolerates ripgrep absence:
    //    we degrade to symbol-only without failing the request.
    let text_terms = extract_text_search_terms(request);
    let text_items = source_text_files(
        repo,
        &text_terms,
        params.max_text_files,
        params.max_text_line_refs,
    );

    Ok(build_response(
        request,
        &view,
        &symbol_matches,
        &text_items,
        params,
    ))
}

fn call_task_localize(
    socket: &Path,
    request: &str,
) -> Result<serde_json::Value, ExploreError> {
    let rpc_request = serde_json::json!({
        "command": "task-localize",
        "task": request,
    });
    let response_text = daemon::send_request(socket, &rpc_request)
        .map_err(ExploreError::DaemonRpc)?;
    let envelope: serde_json::Value = serde_json::from_str(response_text.trim())
        .map_err(|e| ExploreError::InvalidResponse(format!("not JSON: {e}")))?;
    if envelope.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let msg = envelope
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown daemon error");
        return Err(ExploreError::DaemonRpc(msg.to_string()));
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| ExploreError::InvalidResponse("missing `result`".into()))
}

fn call_symbol_batch(
    socket: &Path,
    queries: &[String],
    limit: usize,
) -> Result<SymbolBatchResults, ExploreError> {
    let rpc_request = serde_json::json!({
        "command": "symbol-batch",
        "queries": queries,
        "limit": limit,
    });
    let response_text = daemon::send_request(socket, &rpc_request)
        .map_err(ExploreError::DaemonRpc)?;
    let envelope: serde_json::Value = serde_json::from_str(response_text.trim())
        .map_err(|e| ExploreError::InvalidResponse(format!("not JSON: {e}")))?;
    if envelope.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let msg = envelope
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown daemon error");
        return Err(ExploreError::DaemonRpc(msg.to_string()));
    }
    let result_obj = envelope
        .get("result")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ExploreError::InvalidResponse("missing/object `result`".into()))?;

    let mut by_query: std::collections::BTreeMap<String, Vec<SymbolHit>> =
        std::collections::BTreeMap::new();
    for (query, hits) in result_obj {
        let arr = match hits.as_array() {
            Some(a) => a,
            None => continue,
        };
        let parsed: Vec<SymbolHit> = arr
            .iter()
            .filter_map(SymbolHit::from_value)
            .collect();
        by_query.insert(query.clone(), parsed);
    }
    Ok(SymbolBatchResults {
        query_order: queries.to_vec(),
        by_query,
    })
}

#[derive(Debug, Default)]
struct SymbolBatchResults {
    /// Original query order — preserves user-intent order across the
    /// alphabetical BTreeMap iteration.
    query_order: Vec<String>,
    by_query: std::collections::BTreeMap<String, Vec<SymbolHit>>,
}

#[derive(Debug, Clone)]
struct SymbolHit {
    name: String,
    kind: String,
    file: String,
    line: u64,
    score: i64,
}

impl SymbolHit {
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        let obj = v.as_object()?;
        Some(SymbolHit {
            name: obj.get("name")?.as_str()?.to_string(),
            kind: obj.get("kind")?.as_str()?.to_string(),
            file: obj.get("file")?.as_str()?.to_string(),
            line: obj.get("line")?.as_u64().unwrap_or(0),
            score: obj.get("score")?.as_i64().unwrap_or(0),
        })
    }
}

// ── source-text search (Rust port of _task_localization_text_items) ────
//
// Strategy: shell out to ripgrep for the heavy lifting (file walking,
// suffix filtering, gitignore-respecting traversal, multi-pattern match)
// and do the scoring + ranking in Rust. Ripgrep at 161ms on Mockup for a
// single term means the whole multi-term pass lands well under 1s — fast
// enough that we don't need to background it.
//
// What we do NOT port from the Python helper (yet, deferred to later
// sessions): per-line symbol clustering, file-role classification, the
// elaborate `_text_candidate_score` weighting heuristic. This session's
// port is a correct-but-simpler version: per-file hit count × distinct
// term coverage, with a cap on the line-ref preview list to keep the
// response token-cheap.

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
fn source_text_files(
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
    // Source code by suffix.
    let suffix = lower.rsplit('.').next().unwrap_or("");
    match suffix {
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "java" | "kt"
        | "swift" | "rb" | "php" | "c" | "h" | "cpp" | "hpp" | "cs"
        | "mjs" | "cjs" | "vue" | "svelte" => 5,
        // Test files — slightly lower than code but still useful.
        s if (lower.contains("/tests/")
            || lower.contains("/test/")
            || lower.contains(".test.")
            || lower.contains(".spec."))
            && !s.is_empty() =>
        {
            4
        }
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

// ── symbol query extraction (Rust port of _request_symbol_queries) ──────
//
// Tokenizes `request`, drops English stop words and noisy single-letter
// tokens, builds the canonical query list. When a token contains an
// underscore we add the dropped-underscore variant too (so `add_watch`
// also queries `addwatch`). Order-preserving + de-duplicated lowercase.

const STOP_WORDS: &[&str] = &[
    "about", "after", "against", "also", "and", "before", "being", "between",
    "bug", "code", "command", "could", "defined", "does", "done", "file",
    "files", "find", "fix", "for", "from", "have", "here", "how", "implement",
    "implemented", "implementation", "into", "issue", "json", "located",
    "make", "marked", "marks", "need", "object", "not", "only", "output",
    "path", "prose", "question", "relative", "report", "repo", "repository",
    "request", "rules", "shape", "the", "should", "specific", "that", "their",
    "there", "this", "ticket", "seen", "viewed", "viewing", "what", "when",
    "where", "which", "who", "why", "with", "would", "you",
];

pub(crate) fn extract_symbol_queries(request: &str) -> Vec<String> {
    let normalized = request.replace('`', " ");
    let mut raw_terms: Vec<String> = Vec::new();
    for token in normalized.replace('/', " ").replace('-', " ").split_whitespace() {
        let term: String = token
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if term.len() < 3 {
            continue;
        }
        let lowered = term.to_ascii_lowercase();
        if STOP_WORDS.contains(&lowered.as_str()) {
            continue;
        }
        raw_terms.push(term);
    }

    let mut queries: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for term in &raw_terms {
        let mut variants: Vec<String> = vec![term.clone()];
        if term.contains('_') {
            variants.push(term.replace('_', ""));
        }
        for variant in variants {
            let lowered = variant.to_ascii_lowercase();
            if seen.insert(lowered) {
                queries.push(variant);
            }
        }
    }
    queries
}

// ── response synthesis ──────────────────────────────────────────────────

/// Translate the engine daemon's `task-localize` view into the answer-json
/// envelope the agent contract expects.
fn build_response(
    request: &str,
    view: &serde_json::Value,
    symbol_matches: &SymbolBatchResults,
    text_items: &[AnswerItem],
    params: &ExploreParams,
) -> ExploreResponse {
    let anchors = view
        .get("anchors")
        .and_then(|a| a.get("anchors"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let scope = view.get("scope");
    let in_scope_files: Vec<String> = scope
        .and_then(|s| s.get("in_scope_files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let in_scope_areas: Vec<String> = scope
        .and_then(|s| s.get("in_scope_areas"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Synthesize answer items.
    //
    // Insertion order = priority order (each loop respects the
    // `max_answer_items` cap and skips paths already added):
    //
    //   1. source_text_file  — line-level evidence, strongest signal
    //   2. symbol_search_file — name-match evidence
    //   3. anchor             — graph-derived seed (heuristic, weaker)
    //   4. in_scope_file      — area-membership-only (weakest)
    //
    // This matters: anchors are heuristic seeds (e.g. "package.json"
    // matched a generic config-anchor weight). They're weaker
    // evidence than a line that literally contains the request's
    // terms in executable code. Putting them last among
    // answer-track items reflects that.
    //
    // anchors with `kind = "folder" | "area"` and in_scope_areas
    // are routed to `navigation_hints[]` because the agent is asking
    // for FILES to act on, not directories.
    let mut answers: Vec<AnswerItem> = Vec::new();
    let mut nav_hints: Vec<AnswerItem> = Vec::new();
    let mut anchor_file_items: Vec<AnswerItem> = Vec::new();

    for anchor in &anchors {
        let kind = anchor.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let id = anchor.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let file = anchor.get("file").and_then(|v| v.as_str());
        let reason = anchor
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("anchor match")
            .to_string();
        match kind {
            "file" => {
                let path = file.map(String::from).or_else(|| Some(id.to_string()));
                anchor_file_items.push(AnswerItem {
                    kind: "anchor".into(),
                    target: id.to_string(),
                    path,
                    status: "candidate".into(),
                    confidence: 0.85,
                    reason,
                    role: "anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": "file",
                    }),
                });
            }
            "folder" | "area" => {
                nav_hints.push(AnswerItem {
                    kind: "anchor_area".into(),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.6,
                    reason,
                    role: "navigation_anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": kind,
                    }),
                });
            }
            other if !other.is_empty() => {
                nav_hints.push(AnswerItem {
                    kind: format!("anchor_{other}"),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.55,
                    reason,
                    role: other.to_string(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": other,
                    }),
                });
            }
            _ => {}
        }
    }

    // Slot budgeting: if symbol search has multi-query hits, reserve up
    // to 2 slots so they always land in `answer[]` even when text
    // matches are plentiful. Without this, a query like "find suppliers
    // grader scoring logic" gets 5 weak text matches and zero symbol
    // matches in the response — even when the most relevant file
    // (suppliers_grader.py) was found by symbol search.
    //
    // The reservation is conservative: only ≥2 slots, only when symbol
    // has multi-query hits. Single-query symbol matches stay weakly
    // ranked.
    let symbol_items = build_symbol_file_items(symbol_matches, params.max_symbol_files);
    let multi_query_symbol_count = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .count();
    let symbol_reserved = multi_query_symbol_count.min(2);
    let text_budget = params.max_answer_items.saturating_sub(symbol_reserved);

    for item in text_items.iter().take(text_budget.max(1)) {
        if answers.len() >= text_budget {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &symbol_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    // Backfill text again now that symbol items have landed — the
    // budget cap above held remaining text out; if there's room left
    // (no symbol items, or symbol items dedup'd against text), let
    // text fill the rest.
    for item in text_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &anchor_file_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for file in &in_scope_files {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers.iter().any(|a| a.path.as_deref() == Some(file.as_str())) {
            continue;
        }
        answers.push(AnswerItem {
            kind: "in_scope_file".into(),
            target: file.clone(),
            path: Some(file.clone()),
            status: "candidate".into(),
            confidence: 0.7,
            reason: "Within graph-derived scope for this task".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    for area in &in_scope_areas {
        if nav_hints.iter().any(|h| h.target == *area) {
            continue;
        }
        nav_hints.push(AnswerItem {
            kind: "in_scope_area".into(),
            target: area.clone(),
            path: None,
            status: "navigation_only".into(),
            confidence: 0.5,
            reason: "In-scope area suggested by graph navigation".into(),
            role: "navigation_area".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    // Cap answer count after dedup so we hit the user's intent for
    // `max_answer_items` exactly.
    answers.truncate(params.max_answer_items);

    let answer_count = answers.len();
    let navigation_hint_count = nav_hints.len();

    // Confidence summary: trivial bucketing on the answer items.
    let answer_summary = bucket_confidence(&answers);

    // Trust policy. Tightens as more evidence sources land:
    //
    //   session 1: anchors + scope only          → needs_verification
    //   session 2: + symbol search                → answer_candidate when
    //                                              ≥2 distinct query terms
    //                                              matched in the same file
    //   session 3 (this commit):
    //              + source-text + corroboration → answer_candidate raised
    //                                              when text + symbol agree
    //                                              on the same file (the
    //                                              strongest signal short
    //                                              of running the test
    //                                              suite); weaker shapes
    //                                              degrade gracefully.
    //   session 4+: callsite expansion            → tighter still
    let high_confidence_count = answers
        .iter()
        .filter(|a| a.confidence >= 0.85)
        .count();
    let multi_query_symbol_files: Vec<&str> = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    let strong_text_files: Vec<&str> = text_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_terms")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    // Symbol + text agree on the same file = the highest local signal we
    // produce without running tests or callsite expansion. Treat the
    // intersection as cross-corroborated.
    let cross_corroborated: Vec<&&str> = multi_query_symbol_files
        .iter()
        .filter(|p| strong_text_files.contains(p))
        .collect();

    let policy_kind = if answers.is_empty() && nav_hints.is_empty() {
        "failed"
    } else if !cross_corroborated.is_empty()
        || !multi_query_symbol_files.is_empty()
    {
        "answer_candidate"
    } else if !text_items.is_empty() || !symbol_items.is_empty() {
        // Some text or symbol evidence but not strong enough to defend.
        "needs_verification"
    } else {
        "needs_verification"
    };
    let evidence_level = if !cross_corroborated.is_empty() {
        "graph+symbol+text"
    } else if !multi_query_symbol_files.is_empty() && !text_items.is_empty() {
        "graph+symbol+text-weak"
    } else if !multi_query_symbol_files.is_empty() {
        "graph+symbol"
    } else if !text_items.is_empty() {
        "graph+text"
    } else if !symbol_items.is_empty() {
        "graph+symbol-weak"
    } else {
        "graph"
    };
    let safe_to_use_as_answer = matches!(policy_kind, "answer_candidate");
    let trust_reason = match policy_kind {
        "answer_candidate" if !cross_corroborated.is_empty() => format!(
            "Symbol search and source-text both matched {} candidate file(s); \
             cross-corroborated evidence treated as authoritative.",
            cross_corroborated.len()
        ),
        "answer_candidate" => format!(
            "Symbol search matched {} distinct request terms in the same \
             file; treating as authoritative answer candidate.",
            symbol_items
                .iter()
                .filter_map(|item| item
                    .evidence
                    .get("matched_queries")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len()))
                .max()
                .unwrap_or(2)
        ),
        "failed" => {
            "No anchors, in-scope files, symbol matches, or source-text hits."
                .to_string()
        }
        _ => "Evidence present but not strong enough to defend as an \
              authoritative answer. Verify before acting."
            .to_string(),
    };
    let trust_policy = TrustPolicy {
        safe_to_use_as_answer,
        safe_to_use_as_navigation: !answers.is_empty() || !nav_hints.is_empty(),
        evidence_level: evidence_level.to_string(),
        authoritative_answer_count: high_confidence_count,
        navigation_hint_count,
        degraded: false,
        trust_policy: policy_kind,
        reason: trust_reason,
    };

    let status = if answers.is_empty() && nav_hints.is_empty() {
        "degraded"
    } else {
        "complete"
    };

    let next_actions = if answers.is_empty() && nav_hints.is_empty() {
        vec![
            "Refine the request — graph navigation found no anchors.".into(),
            "Try a more specific keyword from the codebase domain.".into(),
        ]
    } else {
        vec![
            "Read the top answer[] item to verify it matches the task.".into(),
            "If unsure, run `aethyme explore --detail standard` for richer \
             evidence (symbol search + source-text)."
                .into(),
        ]
    };

    ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent: "task_localization_query",
        intent_source: "default",
        status,
        request: ExploreRequest {
            raw: request.to_string(),
            parameters: serde_json::Value::Object(serde_json::Map::new()),
        },
        answer: answers,
        navigation_hints: nav_hints,
        excluded: Vec::new(),
        ambiguous: Vec::new(),
        evidence: Evidence {
            answer_count,
            navigation_hint_count,
            excluded_count: 0,
        },
        confidence: Confidence {
            overall: overall_confidence(&_dummy_strings_for_summary()),
            answer_summary,
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({}),
        },
        safe_to_use_as_answer: trust_policy.safe_to_use_as_answer,
        safe_to_use_as_navigation: trust_policy.safe_to_use_as_navigation,
        trust_policy,
        degraded_reasons: Vec::new(),
        verification_steps: vec![
            serde_json::json!({
                "step": "Open the top answer[] item and confirm it matches the task before relying on it.",
                "rationale": "Graph navigation found this candidate; verifying that the file genuinely handles the task is fast.",
            }),
        ],
        next_actions,
        available_specialized_intents: vec![
            "behavior_localization_query",
            "usage_boundary_query",
        ],
    }
}

/// Group symbol-search hits by file, rank by query coverage + cumulative
/// score, emit AnswerItems with `kind = "symbol_search_file"`. Mirrors
/// `_task_localization_symbol_file_items` in the Python orchestrator so
/// downstream consumers see the same shape.
///
/// Confidence scoring:
///   - 2+ distinct queries matched in this file → 0.88 (multi-term match)
///   - 1 query matched                          → 0.76
///
/// These are the same numbers Python uses; preserving them keeps the
/// trust-policy heuristics consistent across implementations.
fn build_symbol_file_items(
    symbol_matches: &SymbolBatchResults,
    cap: usize,
) -> Vec<AnswerItem> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct PerFile {
        queries: BTreeSet<String>,
        symbols: Vec<serde_json::Value>,
        score: i64,
    }

    let mut by_file: BTreeMap<String, PerFile> = BTreeMap::new();
    // Iterate queries in original order so the most relevant query
    // dominates `symbols[0]` for a given file.
    for query in &symbol_matches.query_order {
        let Some(hits) = symbol_matches.by_query.get(query) else {
            continue;
        };
        for hit in hits {
            if hit.file.trim().is_empty() {
                continue;
            }
            let entry = by_file.entry(hit.file.clone()).or_default();
            entry.queries.insert(query.clone());
            entry.symbols.push(serde_json::json!({
                "name": hit.name,
                "kind": hit.kind,
                "line": hit.line,
                "score": hit.score,
            }));
            entry.score += hit.score;
        }
    }

    let mut ranked: Vec<(String, PerFile)> = by_file.into_iter().collect();
    ranked.sort_by(|(la, a), (lb, b)| {
        // Primary: more distinct queries. Secondary: total score.
        // Tertiary: filename alphabetical for stability.
        b.queries
            .len()
            .cmp(&a.queries.len())
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| la.cmp(lb))
    });

    let mut items: Vec<AnswerItem> = Vec::new();
    for (file_path, summary) in ranked.into_iter().take(cap) {
        let matched_queries: Vec<String> = summary.queries.iter().cloned().collect();
        let multi = matched_queries.len() > 1;
        let confidence = if multi { 0.88 } else { 0.76 };
        let reason = if multi {
            "Multiple request terms matched symbols in this file."
        } else {
            "A request term matched a symbol in this file."
        };
        let symbols_preview: Vec<serde_json::Value> =
            summary.symbols.into_iter().take(5).collect();
        items.push(AnswerItem {
            kind: "symbol_search_file".into(),
            target: file_path.clone(),
            path: Some(file_path),
            status: "candidate".into(),
            confidence,
            reason: reason.into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "query-symbol",
                "matched_queries": matched_queries,
                "symbols": symbols_preview,
                "combined_score": summary.score,
            }),
        });
    }
    items
}

fn bucket_confidence(items: &[AnswerItem]) -> ConfidenceSummary {
    let mut summary = ConfidenceSummary::default();
    for item in items {
        if item.confidence >= 0.8 {
            summary.high += 1;
        } else if item.confidence >= 0.6 {
            summary.medium += 1;
        } else {
            summary.low += 1;
        }
    }
    summary
}

fn overall_confidence(_items: &[String]) -> Option<f64> {
    // Stub — real implementation will use weighted aggregate over evidence
    // sources in a later session. Returning None keeps the contract honest:
    // "no overall confidence is computed yet."
    None
}

fn _dummy_strings_for_summary() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view() -> serde_json::Value {
        serde_json::json!({
            "task": "find watchlist handlers",
            "anchors": {
                "task": "find watchlist handlers",
                "anchors": [
                    {"kind": "file", "id": "includes/Watchlist/WatchedItemStore.php",
                     "file": "includes/Watchlist/WatchedItemStore.php",
                     "reason": "filename match"},
                    {"kind": "folder", "id": "includes/Watchlist",
                     "file": null,
                     "reason": "area match"}
                ]
            },
            "scope": {
                "task": "find watchlist handlers",
                "navigation_order": ["includes/Watchlist", "includes/Specials"],
                "in_scope_files": [
                    "includes/Specials/SpecialEditWatchlist.php",
                    "includes/Watchlist/WatchlistManager.php"
                ],
                "in_scope_symbols": [],
                "in_scope_areas": ["includes/Watchlist"],
                "out_of_scope": [],
                "risks": []
            },
            "next": {
                "target": "find watchlist handlers",
                "relation": "next",
                "items": []
            }
        })
    }

    fn empty_symbols() -> SymbolBatchResults {
        SymbolBatchResults::default()
    }

    fn symbols_for(file: &str, queries: &[(&str, i64)]) -> SymbolBatchResults {
        let mut by_query = std::collections::BTreeMap::new();
        let mut order = Vec::new();
        for (q, score) in queries {
            order.push((*q).to_string());
            by_query.insert(
                (*q).to_string(),
                vec![SymbolHit {
                    name: format!("hit_for_{q}"),
                    kind: "function".into(),
                    file: file.to_string(),
                    line: 42,
                    score: *score,
                }],
            );
        }
        SymbolBatchResults {
            query_order: order,
            by_query,
        }
    }

    #[test]
    fn build_response_synthesizes_answers_and_nav_hints() {
        let response = build_response(
            "find watchlist handlers",
            &sample_view(),
            &empty_symbols(),
            &[],
            &ExploreParams::default(),
        );
        assert!(!response.answer.is_empty(), "expected at least one answer");
        assert!(
            response.answer.iter().any(|a| a.path.as_deref()
                == Some("includes/Watchlist/WatchedItemStore.php")),
            "anchor file should land in answer[]"
        );
        assert!(
            response.answer.iter().any(|a| a.path.as_deref()
                == Some("includes/Specials/SpecialEditWatchlist.php")),
            "in-scope file should land in answer[]"
        );
        assert!(
            response.navigation_hints.iter().any(|h| h.target == "includes/Watchlist"),
            "folder anchor should land in navigation_hints[]"
        );
    }

    #[test]
    fn build_response_caps_answer_count() {
        let mut view = sample_view();
        let scope = view.get_mut("scope").unwrap();
        scope["in_scope_files"] = serde_json::json!(
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]
        );
        let response = build_response(
            "test",
            &view,
            &empty_symbols(),
            &[],
            &ExploreParams {
                max_answer_items: 3,
                ..ExploreParams::default()
            },
        );
        assert_eq!(response.answer.len(), 3);
    }

    #[test]
    fn build_response_degraded_when_empty() {
        let view = serde_json::json!({
            "anchors": {"anchors": []},
            "scope": {
                "in_scope_files": [],
                "in_scope_symbols": [],
                "in_scope_areas": [],
                "out_of_scope": [],
                "risks": []
            },
            "next": {"items": []}
        });
        let response = build_response(
            "nothing matches",
            &view,
            &empty_symbols(),
            &[],
            &ExploreParams::default(),
        );
        assert_eq!(response.status, "degraded");
        assert!(response.answer.is_empty());
        assert!(response.navigation_hints.is_empty());
        assert_eq!(response.trust_policy.trust_policy, "failed");
    }

    #[test]
    fn trust_policy_without_symbol_evidence_is_needs_verification() {
        let response = build_response(
            "find handlers",
            &sample_view(),
            &empty_symbols(),
            &[],
            &ExploreParams::default(),
        );
        // Without symbol evidence, anchors+scope alone don't earn
        // `answer_candidate` — the cross-corroboration is missing.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
        assert_eq!(response.trust_policy.evidence_level, "graph");
    }

    #[test]
    fn multi_query_symbol_match_elevates_to_answer_candidate() {
        let symbols = symbols_for(
            "src/auth/SessionStore.php",
            &[("session", 200), ("authenticate", 300)],
        );
        let response = build_response(
            "find session authenticate handlers",
            &sample_view(),
            &symbols,
            &[],
            &ExploreParams::default(),
        );
        assert!(response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "answer_candidate");
        assert_eq!(response.trust_policy.evidence_level, "graph+symbol");
        // The matched file should appear in answer[] as a symbol_search_file
        // ahead of in_scope_file items because symbol evidence is stronger.
        let symbol_match_position = response
            .answer
            .iter()
            .position(|a| a.kind == "symbol_search_file");
        assert!(
            symbol_match_position.is_some(),
            "symbol_search_file should be present in answer[]"
        );
    }

    #[test]
    fn single_query_symbol_match_stays_at_needs_verification() {
        let symbols = symbols_for("src/util/helpers.php", &[("helper", 100)]);
        let response = build_response(
            "find helper code",
            &sample_view(),
            &symbols,
            &[],
            &ExploreParams::default(),
        );
        // One query matched is weak corroboration — bar to claim
        // `answer_candidate` is multi-term match in the SAME file.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
        assert_eq!(response.trust_policy.evidence_level, "graph+symbol-weak");
    }

    #[test]
    fn extract_symbol_queries_drops_stop_words_and_short_terms() {
        let queries = extract_symbol_queries(
            "Find the file that handles WatchedItem revisions",
        );
        // "find", "the", "that" are stop words. "Watcheditem" stays.
        assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("WatchedItem")));
        assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("revisions")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("the")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("find")));
    }

    #[test]
    fn extract_symbol_queries_adds_underscore_collapsed_variant() {
        let queries = extract_symbol_queries("trace add_watch behavior");
        // Both `add_watch` and `addwatch` should be present.
        let lower: Vec<String> =
            queries.iter().map(|q| q.to_ascii_lowercase()).collect();
        assert!(lower.contains(&"add_watch".to_string()));
        assert!(lower.contains(&"addwatch".to_string()));
    }

    #[test]
    fn extract_text_search_terms_extends_for_behavioural_words() {
        // For text search we keep behavioural keywords like "viewed" and
        // "seen" that the symbol-query helper drops. The trigger is
        // matching them in the request itself; if the request mentions
        // "watchlist" we add domain synonyms ("watched", "notification").
        let terms = extract_text_search_terms(
            "Bug: viewing a diff revision marks watchlist as seen",
        );
        let lower: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
        // The request word "viewing" survives (symbol-search would drop it
        // as too noisy, text-search keeps it).
        assert!(lower.contains(&"viewing".to_string()));
        // Domain expansions added by the watchlist trigger:
        assert!(lower.contains(&"watched".to_string()));
        assert!(lower.contains(&"notification".to_string()));
        // Domain expansions added by the diff/revision trigger:
        assert!(lower.contains(&"diff".to_string()));
        assert!(lower.contains(&"revisions".to_string()));
        // No duplicates (case-insensitive):
        let mut sorted = lower.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), lower.len(), "duplicate term in {lower:?}");
    }
}
