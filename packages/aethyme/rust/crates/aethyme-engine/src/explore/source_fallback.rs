//! Bounded, read-only content search when no trustworthy graph is available.
//!
//! The repository's files (tracked plus untracked-but-not-ignored, as Git
//! reports them) are searched in full for the request's meaningful terms and
//! ranked by content signals: term coverage, BM25-style density, matches on
//! definition lines, proximity of distinct terms, definitions from the
//! on-demand symbol index, file-name matches, and a generic path role
//! (source over tests, docs, vendored and generated code). The search is
//! bounded by wall time, not by a file count, and reports whether it
//! finished. Results are lexical navigation hints, never evidence of callers
//! or impact closure.
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use super::path_role::{self, PathRole};
use super::query_terms::{QueryTerms, occurs_at_boundary, split_identifier, stem};
use super::symbol_index::{self, Definition, IndexStats, Stamp, SymbolIndex};
use super::{AnswerItem, ExploreSubsystem, ExploreSubsystemTarget};

/// Files larger than this are treated as data, not source, and skipped.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Upper bound on the path listing itself (NUL-separated bytes).
const MAX_LISTING_BYTES: u64 = 64 * 1024 * 1024;
/// Only the first bytes of a very long (minified) line are searched.
const MAX_LINE_BYTES: usize = 1024;
/// Matched lines remembered per file for window scoring.
const MAX_MATCHED_LINES: usize = 512;
/// Lines spanned by one proximity window.
const WINDOW_LINES: u32 = 6;
const MAX_LINE_REFS: usize = 3;
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// Term coverage the top hit needs for the coverage rule of
/// [`answer_safety`].
const ANSWER_MIN_COVERAGE: f64 = 0.8;
/// How far the top hit must out-score hit 2 for the coverage rule.
const ANSWER_MIN_MARGIN: f64 = 1.5;

pub const DEFAULT_SOURCE_SEARCH_HITS: usize = 8;
pub const DEFAULT_SOURCE_SEARCH_BUDGET: Duration = Duration::from_millis(2_000);

/// Where the on-demand symbol index is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolCache {
    /// The host cache directory (`AETHYME_HOST_CACHE_DIR`, `XDG_CACHE_HOME`,
    /// or the platform cache directory); not persisted for repositories under
    /// the system temp directory unless a cache directory is named explicitly.
    HostDefault,
    /// An explicit base directory (tests, embedding callers).
    Under(PathBuf),
    /// Parse on demand without persisting.
    Disabled,
}

/// Knobs for the graph-free source search.
#[derive(Debug, Clone)]
pub struct SourceSearchOptions {
    /// Ranked files returned (the navigation hint count).
    pub max_hits: usize,
    /// Wall-time budget for listing, reading, parsing and ranking.
    pub budget: Duration,
    pub symbol_cache: SymbolCache,
}

impl Default for SourceSearchOptions {
    fn default() -> Self {
        SourceSearchOptions {
            max_hits: DEFAULT_SOURCE_SEARCH_HITS,
            budget: DEFAULT_SOURCE_SEARCH_BUDGET,
            symbol_cache: SymbolCache::HostDefault,
        }
    }
}

#[derive(Default)]
pub(super) struct SourceFallback {
    pub hints: Vec<AnswerItem>,
    pub subsystems: Vec<ExploreSubsystem>,
    /// Files whose full content was searched.
    pub scanned_files: usize,
    /// Files the listing produced (after hidden-path filtering).
    pub listed_files: usize,
    /// Binary, oversized, or unreadable files.
    pub skipped_files: usize,
    /// Every listed file was searched within the budget.
    pub complete: bool,
    /// Why the search is incomplete (`None` when complete).
    pub incomplete_reason: Option<&'static str>,
    /// More hits existed than were returned, or the search was incomplete.
    pub truncated: bool,
    pub listing: &'static str,
    pub terms: Vec<String>,
    pub elapsed_ms: u64,
    pub budget_ms: u64,
    pub max_hits: usize,
    pub symbol_index: IndexStats,
    /// Whether the top hit is strong enough to be used as an answer.
    pub answer_safety: AnswerSafety,
}

/// Ranking facts [`answer_safety`] reads, one per ranked hit.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct HitSignal {
    pub score: f64,
    /// Share of the idf weight of the present request terms the hit matched.
    pub coverage: f64,
    /// Separator-free lowercase name of a definition the request named
    /// exactly (`load_token` for a request mentioning `load token`).
    pub exact_symbol: Option<String>,
}

/// The verdict of [`answer_safety`]: `rule` names the rule that held, or
/// the first reason none did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AnswerSafety {
    pub safe: bool,
    pub rule: &'static str,
}

impl Default for AnswerSafety {
    fn default() -> Self {
        AnswerSafety {
            safe: false,
            rule: "no_hits",
        }
    }
}

/// Decide whether the top content-search hit may be used as an answer.
///
/// Content search locates where the request's vocabulary is defined or
/// concentrated; it never proves callers, dependencies or impact. The top
/// hit is answer-safe only when the search listed and read every file within
/// its budget and one of two rules holds:
///
/// - `exact_symbol_definition`: the top hit defines a symbol whose name the
///   request spelled out (`load_token`, `loadToken` or `load token`), and no
///   other ranked hit defines a symbol of the same name;
/// - `dominant_term_coverage`: the top hit covers at least
///   [`ANSWER_MIN_COVERAGE`] of the request's present term weight and scores
///   at least [`ANSWER_MIN_MARGIN`] times hit 2 (or is the only hit).
///
/// `signals` is the full ranking, before truncation to the hit count.
pub(super) fn answer_safety(signals: &[HitSignal], complete: bool) -> AnswerSafety {
    let unsafe_because = |rule| AnswerSafety { safe: false, rule };
    if !complete {
        return unsafe_because("search_incomplete");
    }
    let Some(top) = signals.first() else {
        return unsafe_because("no_hits");
    };
    if let Some(name) = &top.exact_symbol {
        let duplicated = signals[1..]
            .iter()
            .any(|other| other.exact_symbol.as_ref() == Some(name));
        if !duplicated {
            return AnswerSafety {
                safe: true,
                rule: "exact_symbol_definition",
            };
        }
    }
    if top.coverage < ANSWER_MIN_COVERAGE {
        return unsafe_because(if top.exact_symbol.is_some() {
            "ambiguous_symbol_definition"
        } else {
            "low_term_coverage"
        });
    }
    let clear_margin = signals
        .get(1)
        .is_none_or(|second| top.score >= second.score * ANSWER_MIN_MARGIN);
    if !clear_margin {
        return unsafe_because(if top.exact_symbol.is_some() {
            "ambiguous_symbol_definition"
        } else {
            "no_clear_margin"
        });
    }
    AnswerSafety {
        safe: true,
        rule: "dominant_term_coverage",
    }
}

impl SourceFallback {
    pub fn observability(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": "bounded_content_search",
            "listing": self.listing,
            "listed_files": self.listed_files,
            "scanned_files": self.scanned_files,
            "skipped_files": self.skipped_files,
            "max_bytes_per_file": MAX_FILE_BYTES,
            "budget_ms": self.budget_ms,
            "elapsed_ms": self.elapsed_ms,
            "complete": self.complete,
            "reason": self.incomplete_reason,
            "max_hits": self.max_hits,
            "terms": self.terms,
            "answer_safety": {
                "safe": self.answer_safety.safe,
                "rule": self.answer_safety.rule,
            },
            "symbol_index": {
                "cached_files": self.symbol_index.cached_files,
                "cache_hits": self.symbol_index.hits,
                "parsed_files": self.symbol_index.parsed,
                "persisted": self.symbol_index.persisted,
            },
        })
    }
}

#[cfg(test)]
pub(super) fn inspect(repo: &Path, request: &str) -> SourceFallback {
    inspect_with(repo, request, &SourceSearchOptions::default())
}

pub(super) fn inspect_with(
    repo: &Path,
    request: &str,
    options: &SourceSearchOptions,
) -> SourceFallback {
    let started = Instant::now();
    let deadline = started + options.budget;
    let mut result = SourceFallback {
        listing: "none",
        budget_ms: options.budget.as_millis() as u64,
        max_hits: options.max_hits,
        ..SourceFallback::default()
    };
    let Ok(root) = repo.canonicalize() else {
        return result;
    };
    let query = QueryTerms::parse(request);
    result.terms = query.terms.iter().map(|term| term.text.clone()).collect();

    let listing = list_files(&root, deadline);
    result.listing = listing.source;
    result.listed_files = listing.paths.len();
    let mut incomplete_reason = listing.incomplete_reason;

    if query.is_empty() {
        result.complete = incomplete_reason.is_none();
        result.incomplete_reason = incomplete_reason;
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        return result;
    }

    let index = SymbolIndex::open(
        &root,
        match &options.symbol_cache {
            SymbolCache::HostDefault => symbol_index::default_location(&root),
            SymbolCache::Under(base) => Some(symbol_index::location_under(base, &root)),
            SymbolCache::Disabled => None,
        },
    );
    let scans = listing
        .paths
        .par_iter()
        .map(|path| scan_file(&root, path, &query, &index, deadline))
        .collect::<Vec<_>>();

    let mut files = Vec::new();
    let mut budget_skipped = false;
    for scan in scans {
        match scan {
            Scan::Searched(file) => files.push(*file),
            Scan::Skipped => result.skipped_files += 1,
            Scan::OutOfBudget => budget_skipped = true,
        }
    }
    if budget_skipped && incomplete_reason.is_none() {
        incomplete_reason = Some("time_budget_exhausted");
    }
    result.scanned_files = files.len();
    let seen = incomplete_reason.is_none().then(|| {
        files
            .iter()
            .map(|file| file.path.clone())
            .collect::<HashSet<_>>()
    });
    result.symbol_index = index.finish(seen.as_ref());
    result.complete = incomplete_reason.is_none();
    result.incomplete_reason = incomplete_reason;

    let (mut ranked, signals): (Vec<_>, Vec<_>) = rank(&query, &files).into_iter().unzip();
    result.answer_safety = answer_safety(&signals, result.complete);
    result.truncated = !result.complete || ranked.len() > options.max_hits;
    ranked.truncate(options.max_hits);
    result.hints = ranked;
    if !result.hints.is_empty() {
        result.subsystems.push(ExploreSubsystem {
            rank: 1,
            id: "bounded_source_fallback".into(),
            label: "Source locations to verify".into(),
            role: "navigation_only".into(),
            confidence: result.hints[0].confidence,
            paths: result
                .hints
                .iter()
                .filter_map(|hint| hint.path.clone())
                .collect(),
            token_subsystems: vec![],
            top_verification_targets: result
                .hints
                .iter()
                .map(|hint| ExploreSubsystemTarget {
                    kind: hint.kind.clone(),
                    target: hint.target.clone(),
                    path: hint.path.clone(),
                    reason: hint.reason.clone(),
                    confidence: hint.confidence,
                })
                .collect(),
            signals: vec!["bounded_content_search".into()],
            missing_coverage_warnings: vec![
                "No graph: callers, dependency closure, test selection and documentation impact are unavailable".into(),
            ],
        });
    }
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    result
}

// ── listing ─────────────────────────────────────────────────────────────

struct Listing {
    paths: Vec<String>,
    source: &'static str,
    incomplete_reason: Option<&'static str>,
}

fn list_files(root: &Path, deadline: Instant) -> Listing {
    match git_listing(root, deadline) {
        GitListing::Listed(paths, incomplete_reason) => Listing {
            paths,
            source: "git",
            incomplete_reason,
        },
        GitListing::TimedOut => Listing {
            paths: Vec::new(),
            source: "git",
            incomplete_reason: Some("time_budget_exhausted"),
        },
        GitListing::NotARepository => {
            let mut paths = Vec::new();
            let complete = walk(root, root, deadline, &mut paths);
            paths.sort();
            Listing {
                paths,
                source: "filesystem",
                incomplete_reason: (!complete).then_some("time_budget_exhausted"),
            }
        }
    }
}

enum GitListing {
    Listed(Vec<String>, Option<&'static str>),
    TimedOut,
    NotARepository,
}

/// Tracked files plus untracked files Git does not ignore: the files a
/// developer would search, without secrets or build output named in
/// `.gitignore`. Hidden paths (`.env`, `.git/`) are always excluded.
fn git_listing(root: &Path, deadline: Instant) -> GitListing {
    let Ok(mut child) = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ])
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return GitListing::NotARepository;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return GitListing::NotARepository;
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.take(MAX_LISTING_BYTES + 1).read_to_end(&mut bytes);
        let _ = sender.send((read, bytes));
    });
    let wait = deadline.saturating_duration_since(Instant::now());
    let Ok((read, mut bytes)) = receiver.recv_timeout(wait) else {
        let _ = child.kill();
        let _ = child.wait();
        return GitListing::TimedOut;
    };
    let mut incomplete_reason = None;
    if bytes.len() as u64 > MAX_LISTING_BYTES {
        incomplete_reason = Some("listing_too_large");
        let _ = child.kill();
        bytes.truncate(MAX_LISTING_BYTES as usize);
        match bytes.iter().rposition(|byte| *byte == 0) {
            Some(last) => bytes.truncate(last + 1),
            None => bytes.clear(),
        }
    }
    let status = child.wait();
    if read.is_err()
        || (incomplete_reason.is_none() && !status.is_ok_and(|status| status.success()))
    {
        return GitListing::NotARepository;
    }
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter(|path| visible(path))
        .map(str::to_string)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    GitListing::Listed(paths, incomplete_reason)
}

fn visible(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path).components().all(|component| {
            matches!(component, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
        })
}

/// Filesystem walk for directories that are not Git repositories: hidden
/// entries and symlinks are skipped. Returns false when the deadline hit.
fn walk(root: &Path, dir: &Path, deadline: Instant, out: &mut Vec<String>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in entries.flatten() {
        if Instant::now() >= deadline {
            return false;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if !walk(root, &path, deadline, out) {
                return false;
            }
        } else if file_type.is_file()
            && let Ok(relative) = path.strip_prefix(root)
            && let Some(relative) = relative.to_str()
        {
            out.push(relative.replace(std::path::MAIN_SEPARATOR, "/"));
        }
    }
    true
}

// ── scanning ────────────────────────────────────────────────────────────

enum Scan {
    Searched(Box<ScannedFile>),
    Skipped,
    OutOfBudget,
}

struct MatchedLine {
    line: u32,
    mask: u32,
    definition: bool,
}

struct ScannedFile {
    path: String,
    role: PathRole,
    retired: bool,
    line_count: u32,
    /// Matching lines per term.
    tf: Vec<u32>,
    /// Terms seen on a definition-looking line.
    definition_mask: u32,
    lines: Vec<MatchedLine>,
    definitions: Vec<Definition>,
}

fn scan_file(
    root: &Path,
    path: &str,
    query: &QueryTerms,
    index: &SymbolIndex,
    deadline: Instant,
) -> Scan {
    if Instant::now() >= deadline {
        return Scan::OutOfBudget;
    }
    let full = root.join(path);
    // symlink_metadata: a symlink is never followed out of the repository.
    let Ok(metadata) = full.symlink_metadata() else {
        return Scan::Skipped;
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Scan::Skipped;
    }
    let Ok(bytes) = std::fs::read(&full) else {
        return Scan::Skipped;
    };
    if bytes[..bytes.len().min(8_192)].contains(&0) {
        return Scan::Skipped;
    }
    let text = String::from_utf8_lossy(&bytes);
    let classified = path_role::classify(path);
    let role = if classified.role == PathRole::Source && path_role::has_generated_header(&text) {
        PathRole::Generated
    } else {
        classified.role
    };
    let mut file = ScannedFile {
        path: path.to_string(),
        role,
        retired: classified.retired,
        line_count: 0,
        tf: vec![0; query.terms.len()],
        definition_mask: 0,
        lines: Vec::new(),
        definitions: Vec::new(),
    };
    for (number, line) in text.lines().enumerate() {
        file.line_count += 1;
        let line = truncate_at_char(line, MAX_LINE_BYTES);
        let lower = line.to_ascii_lowercase();
        let mut mask = 0u32;
        for (bit, term) in query.terms.iter().enumerate() {
            if occurs_at_boundary(line, &lower, &term.stem) {
                mask |= 1 << bit;
                file.tf[bit] += 1;
            }
        }
        if mask == 0 {
            continue;
        }
        let definition = is_definition_line(line);
        if definition {
            file.definition_mask |= mask;
        }
        if file.lines.len() < MAX_MATCHED_LINES {
            file.lines.push(MatchedLine {
                line: number as u32 + 1,
                mask,
                definition,
            });
        }
    }
    if !file.lines.is_empty() && symbol_index::supports(path) {
        file.definitions = index.definitions(path, Stamp::of(&metadata), &text);
    }
    Scan::Searched(Box::new(file))
}

fn truncate_at_char(line: &str, max: usize) -> &str {
    if line.len() <= max {
        return line;
    }
    let mut end = max;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

fn is_definition_line(line: &str) -> bool {
    symbol_index::line_definition(line).is_some()
}

// ── ranking ─────────────────────────────────────────────────────────────

struct Weights {
    idf: Vec<f64>,
    /// Sum of idf over terms that occur anywhere (content or a path).
    present_total: f64,
}

fn weights(query: &QueryTerms, files: &[ScannedFile], path_masks: &[u32]) -> Weights {
    let n = files.len().max(1) as f64;
    let any_path = path_masks
        .iter()
        .fold(0u32, |all, mask| all | mask | (mask >> 16));
    let mut idf = Vec::with_capacity(query.terms.len());
    let mut present_total = 0.0;
    for bit in 0..query.terms.len() {
        let df = files.iter().filter(|file| file.tf[bit] > 0).count() as f64;
        let weight = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        idf.push(weight);
        if df > 0.0 || any_path & (1 << bit) != 0 {
            present_total += weight;
        }
    }
    Weights { idf, present_total }
}

fn mask_weight(mask: u32, idf: &[f64]) -> f64 {
    idf.iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, weight)| weight)
        .sum()
}

/// Terms matching a file-name part (bits 0..16) or a directory part
/// (bits 16..32).
fn path_term_mask(path: &str, query: &QueryTerms) -> u32 {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let name = name.split('.').next().unwrap_or(name);
    let name_parts = split_identifier(name);
    let dir_parts = dirs
        .split('/')
        .flat_map(split_identifier)
        .collect::<Vec<_>>();
    let mut mask = 0u32;
    for (bit, term) in query.terms.iter().enumerate() {
        if name_parts.iter().any(|part| part.starts_with(&term.stem)) {
            mask |= 1 << bit;
        } else if dir_parts.iter().any(|part| part.starts_with(&term.stem)) {
            mask |= 1 << (bit + 16);
        }
    }
    mask
}

struct SymbolMatch<'a> {
    definition: &'a Definition,
    mask: u32,
    score: f64,
    exact: bool,
}

fn best_symbol<'a>(
    definitions: &'a [Definition],
    query: &QueryTerms,
    idf: &[f64],
) -> Option<SymbolMatch<'a>> {
    let mut best: Option<SymbolMatch<'a>> = None;
    for definition in definitions {
        let parts = split_identifier(&definition.name);
        if parts.is_empty() {
            continue;
        }
        let joined = parts.concat();
        let mut mask = 0u32;
        let mut matched_parts = 0usize;
        for part in &parts {
            let part_stem = stem(part);
            let mut hit = false;
            for (bit, term) in query.terms.iter().enumerate() {
                if part.starts_with(&term.stem) || part_stem == term.stem {
                    mask |= 1 << bit;
                    hit = true;
                }
            }
            matched_parts += usize::from(hit);
        }
        if mask == 0 {
            continue;
        }
        let weight = mask_weight(mask, idf);
        let part_coverage = matched_parts as f64 / parts.len() as f64;
        let exact = query.compounds.contains(&joined)
            || (parts.len() == 1 && query.terms.iter().any(|term| term.text == joined));
        let mut score = if exact && parts.len() > 1 {
            weight * 2.5 + 0.5 * parts.len() as f64
        } else if mask.count_ones() >= 2 && part_coverage >= 0.5 {
            weight * part_coverage * 1.5
        } else if exact || part_coverage >= 1.0 {
            weight * 0.6
        } else {
            0.0
        };
        if definition.kind == "variable" {
            score *= 0.5;
        }
        if score > best.as_ref().map_or(0.0, |best| best.score) {
            best = Some(SymbolMatch {
                definition,
                mask,
                score,
                exact,
            });
        }
    }
    best
}

/// Best proximity window: the densest run of `WINDOW_LINES` lines by the
/// idf weight of the distinct terms it contains.
fn best_window(lines: &[MatchedLine], idf: &[f64]) -> Option<(u32, u32, u32, f64)> {
    let mut best: Option<(u32, u32, u32, f64)> = None;
    for (start_index, first) in lines.iter().enumerate() {
        let mut mask = 0u32;
        let mut last = first.line;
        for line in lines[start_index..]
            .iter()
            .take_while(|line| line.line < first.line + WINDOW_LINES)
        {
            mask |= line.mask;
            last = line.line;
        }
        let bonus = if lines[start_index].definition {
            1.1
        } else {
            1.0
        };
        let score = mask_weight(mask, idf) * bonus;
        if score > best.map_or(0.0, |best| best.3) {
            best = Some((first.line, last, mask, score));
        }
    }
    best
}

fn rank(query: &QueryTerms, files: &[ScannedFile]) -> Vec<(AnswerItem, HitSignal)> {
    let path_masks = files
        .iter()
        .map(|file| path_term_mask(&file.path, query))
        .collect::<Vec<_>>();
    let weights = weights(query, files, &path_masks);
    let idf = &weights.idf;
    let avg_len = (files.iter().map(|file| file.line_count as f64).sum::<f64>()
        / files.len().max(1) as f64)
        .max(1.0);
    let wants_tests = query.mentions(&["test", "tests", "testing", "spec", "specs"]);
    let wants_docs = query.mentions(&["doc", "docs", "documentation", "readme", "guide"]);

    let mut scored = Vec::new();
    for (file, &path_mask) in files.iter().zip(&path_masks) {
        let name_mask = path_mask & 0xFFFF;
        let dir_mask = path_mask >> 16;
        let content_mask = file
            .tf
            .iter()
            .enumerate()
            .filter(|(_, tf)| **tf > 0)
            .fold(0u32, |mask, (bit, _)| mask | (1 << bit));
        if content_mask == 0 && name_mask == 0 {
            continue;
        }
        let length_norm = 1.0 - BM25_B + BM25_B * file.line_count as f64 / avg_len;
        let content = file
            .tf
            .iter()
            .enumerate()
            .map(|(bit, tf)| {
                let tf = *tf as f64;
                idf[bit] * tf * (BM25_K1 + 1.0) / (tf + BM25_K1 * length_norm)
            })
            .sum::<f64>();
        let definition = mask_weight(file.definition_mask, idf) * 0.8;
        let path =
            mask_weight(name_mask, idf) * 1.5 + mask_weight(dir_mask & !name_mask, idf) * 0.4;
        let window = best_window(&file.lines, idf);
        let window_score = window
            .filter(|window| window.2.count_ones() >= 2)
            .map_or(0.0, |window| window.3 * 0.8);
        let symbol = best_symbol(&file.definitions, query, idf);
        let symbol_score = symbol.as_ref().map_or(0.0, |symbol| symbol.score);
        let matched = content_mask | name_mask | symbol.as_ref().map_or(0, |symbol| symbol.mask);
        let coverage = if weights.present_total > 0.0 {
            (mask_weight(matched, idf) / weights.present_total).min(1.0)
        } else {
            0.0
        };
        let role_weight =
            file.role.weight(wants_tests, wants_docs) * if file.retired { 0.6 } else { 1.0 };
        let raw = content + definition + path + window_score + symbol_score;
        let score = raw * (0.3 + 0.7 * coverage * coverage) * role_weight;
        if score <= 0.0 {
            continue;
        }
        scored.push((score, file, matched, coverage, window, symbol));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));

    scored
        .into_iter()
        .map(|(score, file, matched, coverage, window, symbol)| {
            let mut line_refs = Vec::new();
            let symbol_first = symbol
                .as_ref()
                .is_some_and(|symbol| symbol.score >= window.map_or(0.0, |window| window.3));
            let symbol_ref = symbol.as_ref().map(|symbol| {
                serde_json::json!({
                    "line": symbol.definition.start_line,
                    "end_line": symbol.definition.end_line,
                    "kind": "definition",
                    "symbol": symbol.definition.name,
                    "symbol_kind": symbol.definition.kind,
                })
            });
            let window_ref = window.map(|(start, end, _, _)| {
                serde_json::json!({"line": start, "end_line": end, "kind": "match"})
            });
            if symbol_first {
                line_refs.extend(symbol_ref.clone());
                line_refs.extend(window_ref);
            } else {
                line_refs.extend(window_ref);
                line_refs.extend(symbol_ref.clone());
            }
            if let Some(line) = file.lines.iter().find(|line| line.definition) {
                let covered = line_refs.iter().any(|existing| {
                    let start = existing["line"].as_u64().unwrap_or(0) as u32;
                    let end = existing["end_line"].as_u64().unwrap_or(0) as u32;
                    (start..=end).contains(&line.line)
                });
                if !covered {
                    line_refs
                        .push(serde_json::json!({"line": line.line, "kind": "definition_line"}));
                }
            }
            if line_refs.is_empty()
                && let Some(line) = file.lines.first()
            {
                line_refs.push(serde_json::json!({"line": line.line, "kind": "match"}));
            }
            line_refs.truncate(MAX_LINE_REFS);

            let matched_terms = query
                .terms
                .iter()
                .enumerate()
                .filter(|(bit, _)| matched & (1 << bit) != 0)
                .map(|(_, term)| term.text.clone())
                .collect::<Vec<_>>();
            let mut reason = format!(
                "Content search matched {}/{} request terms ({})",
                matched_terms.len(),
                query.terms.len(),
                matched_terms.join(", ")
            );
            if let Some(symbol) = &symbol {
                reason.push_str(&format!(
                    "; defines `{}` ({}) at line {}",
                    symbol.definition.name, symbol.definition.kind, symbol.definition.start_line
                ));
            }
            reason.push_str(&format!(
                "; {} file. Lexical navigation only: verify the span before making semantic claims",
                file.role.as_str()
            ));
            let confidence = ((0.25 + 0.45 * coverage) * 100.0).round() / 100.0;
            let signal = HitSignal {
                score,
                coverage,
                exact_symbol: symbol
                    .as_ref()
                    .filter(|symbol| symbol.exact)
                    .map(|symbol| split_identifier(&symbol.definition.name).concat()),
            };
            let item = AnswerItem {
                kind: "source_file".into(),
                target: file.path.clone(),
                path: Some(file.path.clone()),
                status: "navigation_hint".into(),
                confidence,
                reason,
                role: "source_navigation".into(),
                evidence: serde_json::json!({
                    "source": "bounded_content_search",
                    "graph_available": false,
                    "score": (score * 1000.0).round() / 1000.0,
                    "path_role": file.role.as_str(),
                    "matched_terms": matched_terms,
                    "term_coverage": (coverage * 100.0).round() / 100.0,
                    "symbol_match": symbol.as_ref().map(|symbol| serde_json::json!({
                        "name": symbol.definition.name,
                        "kind": symbol.definition.kind,
                        "exact": symbol.exact,
                    })),
                    "line_refs": line_refs,
                }),
            };
            (item, signal)
        })
        .collect()
}

#[cfg(test)]
mod tests;
