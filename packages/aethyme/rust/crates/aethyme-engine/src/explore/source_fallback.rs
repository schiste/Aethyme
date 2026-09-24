//! Bounded, read-only content search when no trustworthy graph is available.
//!
//! The repository's files (tracked plus untracked-but-not-ignored, as Git
//! reports them) are searched in full for the request's meaningful terms and
//! ranked by BM25F (see [`bm25f`]) over the file name, directory names,
//! definition names from the on-demand symbol index, code lines and comment
//! lines, scaled by a generic path role (source over tests, docs, vendored
//! and generated code). The reported span is the densest window of distinct
//! matched terms or the best-matching definition. The search is
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
use super::query_terms::{QueryTerms, boundary_matches, split_identifier, stem, token_count};
use super::symbol_index::{self, Definition, IndexStats, Stamp, SymbolIndex};
use super::{AnswerItem, ExploreSubsystem, ExploreSubsystemTarget};
use bm25f::{Corpus, Document, Field};

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
/// Occurrences of one term counted per line: enough to measure density
/// without letting one pathological line dominate.
const MAX_MATCHES_PER_LINE: u32 = 8;

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
            "scoring": {
                "model": "bm25f",
                "k1": bm25f::K1,
                "fields": ["name", "dir", "symbol", "code", "comment"],
                "weights": bm25f::FIELDS.map(|field| field.weight),
                "b": bm25f::FIELDS.map(|field| field.b),
            },
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
    /// An import or include line: evidence the file uses a name, not where
    /// the behavior is.
    import: bool,
}

struct ScannedFile {
    path: String,
    role: PathRole,
    retired: bool,
    /// Term occurrences per term: `[code, comment]`.
    tf: Vec<[u32; 2]>,
    /// Tokens on code and comment lines.
    tokens: [u32; 2],
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
        tf: vec![[0; 2]; query.terms.len()],
        tokens: [0; 2],
        lines: Vec::new(),
        definitions: Vec::new(),
    };
    // ASCII lowercasing keeps byte offsets, so line slices of `lowered` line
    // up with the original lines.
    let lowered = text.to_ascii_lowercase();
    // Most files hold no request term at all: one substring pass each
    // settles that without per-line work. Such a file has no field lengths
    // either; averages are taken over the files that match (see `rank`).
    if !query
        .terms
        .iter()
        .any(|term| lowered.contains(term.stem.as_str()))
    {
        return Scan::Searched(Box::new(file));
    }
    // Line table: start offset, end of the searched prefix, comment flag.
    let mut table = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = truncate_at_char(raw.trim_end_matches(['\n', '\r']), MAX_LINE_BYTES);
        let comment = is_comment_line(line);
        file.tokens[usize::from(comment)] += token_count(line);
        table.push((offset, offset + line.len(), comment));
        offset += raw.len();
    }
    // One whole-text pass per term; occurrences come in ascending order, so
    // the line cursor only moves forward.
    let mut masks = vec![0u32; table.len()];
    for (bit, term) in query.terms.iter().enumerate() {
        let mut index = 0;
        let mut counted = (usize::MAX, 0u32);
        for at in boundary_matches(&text, &lowered, &term.stem) {
            while index + 1 < table.len() && table[index + 1].0 <= at {
                index += 1;
            }
            let (_, end, comment) = table[index];
            if at + term.stem.len() > end {
                continue;
            }
            if counted.0 != index {
                counted = (index, 0);
            }
            if counted.1 >= MAX_MATCHES_PER_LINE {
                continue;
            }
            counted.1 += 1;
            masks[index] |= 1 << bit;
            file.tf[bit][usize::from(comment)] += 1;
        }
    }
    for (index, &mask) in masks.iter().enumerate() {
        if mask == 0 {
            continue;
        }
        if file.lines.len() >= MAX_MATCHED_LINES {
            break;
        }
        let (start, end, _) = table[index];
        let line = &text[start..end];
        file.lines.push(MatchedLine {
            line: index as u32 + 1,
            mask,
            definition: is_definition_line(line),
            import: is_import_line(line),
        });
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

/// A line that imports or includes another module in a widely used syntax.
fn is_import_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    [
        "import ", "from ", "use ", "#include", "#import", "using ", "require ", "require(",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
        || (trimmed.starts_with("export ") && trimmed.contains(" from "))
}

/// A line that is only a comment in a widely used comment syntax (`//`,
/// `/*`, `*`, `#`, `--`, `;`, `<!--`, docstring quotes). `#` followed by a
/// word or `[` is a preprocessor directive or an attribute, not a comment.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        return !rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '[');
    }
    ["//", "/*", "*", "--", ";", "<!--", "\"\"\"", "'''"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

// ── ranking ─────────────────────────────────────────────────────────────

fn mask_weight(mask: u32, idf: &[f64]) -> f64 {
    idf.iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, weight)| weight)
        .sum()
}

/// Lowercase parts of the file name (without extensions) and of the
/// directory names.
fn path_parts(path: &str) -> (Vec<String>, Vec<String>) {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let name = name.split('.').next().unwrap_or(name);
    let dir_parts = dirs
        .split('/')
        .flat_map(split_identifier)
        .collect::<Vec<_>>();
    (split_identifier(name), dir_parts)
}

fn part_matches(part: &str, stem_of_term: &str) -> bool {
    part.starts_with(stem_of_term)
}

const ALL_FIELDS: [Field; bm25f::FIELD_COUNT] = [
    Field::Name,
    Field::Dir,
    Field::Symbol,
    Field::Code,
    Field::Comment,
];

struct SymbolMatch<'a> {
    definition: &'a Definition,
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
                score,
                exact,
            });
        }
    }
    best
}

/// Best proximity window: the densest run of `WINDOW_LINES` lines by the
/// idf weight of the distinct terms it contains. Import lines are skipped:
/// a block of imports names many terms without implementing any.
fn best_window(lines: &[MatchedLine], idf: &[f64]) -> Option<(u32, u32, u32, f64)> {
    let lines = lines.iter().filter(|line| !line.import).collect::<Vec<_>>();
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

/// The BM25F document for one scanned file. Its terms are the request
/// terms followed by the request's compounds (`loadtoken`), which only the
/// symbol field can hold: a definition named exactly like a compound is a
/// phrase match, weighted by how rare that definition is in the corpus.
///
/// `None` for a file that matches no term in any field: it only counts
/// toward the corpus size.
fn document(file: &ScannedFile, query: &QueryTerms) -> Option<Document> {
    let (name_parts, dir_parts) = path_parts(&file.path);
    let path_match = || {
        query.terms.iter().any(|term| {
            name_parts
                .iter()
                .chain(&dir_parts)
                .any(|part| part_matches(part, &term.stem))
        })
    };
    if file.lines.is_empty() && !path_match() {
        return None;
    }
    let mut doc = Document::new(query.terms.len() + query.compounds.len());
    doc.len[Field::Name as usize] = name_parts.len() as f64;
    doc.len[Field::Dir as usize] = dir_parts.len() as f64;
    doc.len[Field::Code as usize] = f64::from(file.tokens[0]);
    doc.len[Field::Comment as usize] = f64::from(file.tokens[1]);
    for (bit, term) in query.terms.iter().enumerate() {
        let hits = |parts: &[String]| {
            parts
                .iter()
                .filter(|part| part_matches(part, &term.stem))
                .count() as f64
        };
        doc.add(bit, Field::Name, hits(&name_parts));
        doc.add(bit, Field::Dir, hits(&dir_parts));
        doc.add(bit, Field::Code, f64::from(file.tf[bit][0]));
        doc.add(bit, Field::Comment, f64::from(file.tf[bit][1]));
    }
    for definition in &file.definitions {
        let parts = split_identifier(&definition.name);
        doc.len[Field::Symbol as usize] += parts.len() as f64;
        if parts.len() >= 2 {
            let joined = parts.concat();
            if let Some(index) = query.compounds.iter().position(|c| *c == joined) {
                doc.add(query.terms.len() + index, Field::Symbol, 1.0);
            }
        }
        for (bit, term) in query.terms.iter().enumerate() {
            let matched = parts
                .iter()
                .any(|part| part_matches(part, &term.stem) || stem(part) == term.stem);
            if matched {
                doc.add(bit, Field::Symbol, 1.0);
            }
        }
    }
    Some(doc)
}

/// Lines a one-line (keyword-found) definition is taken to span when no
/// later definition bounds it.
const PASSAGE_FALLBACK_LINES: u32 = 60;

/// The definition whose body best matches the request. Each definition's
/// line range is a passage scored by BM25 (k1 and b as for files) over the
/// matched lines it holds, against the file's average definition length, so
/// a short function dense in request terms beats the class around it. A
/// one-line definition (found by keyword, without a parser) spans until the
/// next definition, the end of its enclosing definition, or
/// [`PASSAGE_FALLBACK_LINES`], whichever comes first.
fn best_passage<'a>(file: &'a ScannedFile, idf: &[f64]) -> Option<(&'a Definition, u32, f64)> {
    let definitions = &file.definitions;
    if definitions.is_empty() || file.lines.is_empty() {
        return None;
    }
    let spans = definitions
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let start = definition.start_line;
            if definition.end_line > start {
                return (start, definition.end_line);
            }
            // Bounded by the innermost multi-line definition around it.
            let bound = definitions
                .iter()
                .filter(|outer| outer.start_line <= start && outer.end_line > start)
                .map(|outer| outer.end_line)
                .min()
                .unwrap_or(start + PASSAGE_FALLBACK_LINES);
            let next = definitions[index + 1..]
                .iter()
                .map(|next| next.start_line)
                .find(|&line| line > start)
                .map_or(bound, |line| line - 1);
            (start, next.min(bound))
        })
        .collect::<Vec<_>>();
    let average = spans
        .iter()
        .map(|(start, end)| f64::from(end - start + 1))
        .sum::<f64>()
        / spans.len() as f64;
    let mut best: Option<(&Definition, u32, f64)> = None;
    for (definition, &(start, end)) in definitions.iter().zip(&spans) {
        let from = file.lines.partition_point(|line| line.line < start);
        let mut tf = vec![0u32; idf.len()];
        for line in file.lines[from..]
            .iter()
            .take_while(|line| line.line <= end)
        {
            for (bit, count) in tf.iter_mut().enumerate() {
                *count += u32::from(line.mask & (1 << bit) != 0);
            }
        }
        let length = f64::from(end - start + 1);
        let norm = 1.0 - bm25f::PASSAGE_B + bm25f::PASSAGE_B * length / average.max(1.0);
        let score = tf
            .iter()
            .zip(idf)
            .filter(|(tf, _)| **tf > 0)
            .map(|(tf, idf)| {
                let tf = f64::from(*tf);
                idf * tf * (bm25f::K1 + 1.0) / (tf + bm25f::K1 * norm)
            })
            .sum::<f64>();
        // Ties go to the shorter passage: the innermost definition.
        let better = best.is_none_or(|(current, current_end, current_score)| {
            score > current_score
                || (score == current_score && end - start < current_end - current.start_line)
        });
        if score > 0.0 && better {
            best = Some((definition, end, score));
        }
    }
    best
}

fn rank(query: &QueryTerms, files: &[ScannedFile]) -> Vec<(AnswerItem, HitSignal)> {
    let (candidates, documents): (Vec<&ScannedFile>, Vec<Document>) = files
        .par_iter()
        .filter_map(|file| Some((file, document(file, query)?)))
        .unzip();
    // Field-length averages come from the files that match at least one
    // term (the only ones ranked); idf uses every searched file.
    let mut corpus = Corpus::new(
        &documents,
        query.terms.len() + query.compounds.len(),
        files.len(),
    );
    // A phrase is rare by construction; it may count as much as matching
    // the request terms it is made of, never more.
    for (index, compound) in query.compounds.iter().enumerate() {
        let parts = query
            .terms
            .iter()
            .enumerate()
            .filter(|(_, term)| compound.contains(term.text.as_str()))
            .map(|(bit, _)| corpus.idf[bit])
            .sum::<f64>();
        let phrase = &mut corpus.idf[query.terms.len() + index];
        *phrase = phrase.min(parts);
    }
    let idf = &corpus.idf[..query.terms.len()];
    let term_mask = |doc: &Document, fields: &[Field]| {
        (0..query.terms.len())
            .filter(|&bit| {
                fields
                    .iter()
                    .any(|field| doc.tf[bit][*field as usize] > 0.0)
            })
            .fold(0u32, |mask, bit| mask | (1 << bit))
    };
    // Share of idf weight counts only terms present somewhere in the corpus.
    let present = documents
        .iter()
        .fold(0u32, |all, doc| all | term_mask(doc, &ALL_FIELDS));
    let present_total = mask_weight(present, idf);
    let wants_tests = query.mentions(&["test", "tests", "testing", "spec", "specs"]);
    let wants_docs = query.mentions(&["doc", "docs", "documentation", "readme", "guide"]);

    let mut scored = Vec::new();
    for (file, doc) in candidates.into_iter().zip(&documents) {
        // A directory-only match says nothing about this file in particular.
        let matched = term_mask(
            doc,
            &[Field::Name, Field::Symbol, Field::Code, Field::Comment],
        );
        if matched == 0 {
            continue;
        }
        let coverage = if present_total > 0.0 {
            (mask_weight(matched, idf) / present_total).min(1.0)
        } else {
            0.0
        };
        let role_weight =
            file.role.weight(wants_tests, wants_docs) * if file.retired { 0.6 } else { 1.0 };
        let score = corpus.score(doc) * role_weight;
        if score <= 0.0 {
            continue;
        }
        let window = best_window(&file.lines, idf);
        let symbol = best_symbol(&file.definitions, query, idf);
        scored.push((score, file, matched, coverage, window, symbol));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));

    scored
        .into_iter()
        .map(|(score, file, matched, coverage, window, symbol)| {
            let mut line_refs: Vec<serde_json::Value> = Vec::new();
            let covered = |refs: &[serde_json::Value], line: u32| {
                refs.iter().any(|existing| {
                    let start = existing["line"].as_u64().unwrap_or(0) as u32;
                    let end = existing["end_line"].as_u64().unwrap_or(0) as u32;
                    (start..=end.max(start)).contains(&line)
                })
            };
            // The definition whose body is densest in request terms leads,
            // then the densest window outside it, then the definition whose
            // name best matches the request.
            if let Some((definition, end, _)) = best_passage(file, idf) {
                line_refs.push(serde_json::json!({
                    "line": definition.start_line,
                    "end_line": end,
                    "kind": "definition",
                    "symbol": definition.name,
                    "symbol_kind": definition.kind,
                }));
            }
            if let Some((start, end, _, _)) = window
                && !(covered(&line_refs, start) && covered(&line_refs, end))
            {
                line_refs
                    .push(serde_json::json!({"line": start, "end_line": end, "kind": "match"}));
            }
            if let Some(symbol) = &symbol
                && !covered(&line_refs, symbol.definition.start_line)
            {
                line_refs.push(serde_json::json!({
                    "line": symbol.definition.start_line,
                    "end_line": symbol.definition.end_line,
                    "kind": "definition",
                    "symbol": symbol.definition.name,
                    "symbol_kind": symbol.definition.kind,
                }));
            }
            if let Some(line) = file.lines.iter().find(|line| line.definition)
                && !covered(&line_refs, line.line)
            {
                line_refs.push(serde_json::json!({"line": line.line, "kind": "definition_line"}));
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

mod bm25f;
#[cfg(test)]
mod tests;
