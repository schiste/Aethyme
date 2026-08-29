//! `aethyme broker check-contract` — the cross-process contract gate.
//!
//! Native port of `scripts/check-cross-process-contract.py`
//! (python-retirement Phase 6). The checker is CI- and gate-load-bearing
//! (`.github/workflows/cross-process-contract.yml` and the
//! `cross-process-contract` gate in `.aethyme/gates.toml`), so it had to
//! go native *before* `src/` and the Python toolchain were deleted —
//! otherwise the biggest deletion of the migration would have landed with
//! its own guard switched off.
//!
//! Why the broker crate: the broker already owns the gate runner that
//! invokes this check (`gates.rs`) and the read-only repo-inspection
//! pattern (`certify`/`init`). A check whose entire job is "inspect this
//! worktree's diff and refuse undeclared contract changes" is the same
//! shape, and living here means CI and the gate both call one shipped
//! binary rather than a checked-in script.
//!
//! A "contract change" is a diff that touches a symbol named in
//! `packages/aethyme/docs/architecture/cross-process-consumers.md` — the
//! canonical inventory of cross-process Aethyme entry points. The
//! 2026-05-08 hard-delete of the Python `explore` command broke the
//! deployed `aethyme-explore` wrapper because the consumer wasn't listed;
//! this check is the friction layer that catches the next miss.
//!
//! Logic (unchanged from the Python original):
//!
//! 1. Parse the consumers doc for inline-code symbols (backtick-wrapped
//!    tokens). These are the names whose removal has cross-process blast
//!    radius.
//! 2. Read the diff against `--base`.
//! 3. For each *removed* line, check whether it contains a tracked
//!    symbol. Removals are the dangerous direction — additions mean
//!    someone is *introducing* something, which is fine on its own.
//! 4. If any tracked symbols appear on removed lines, look for a contract
//!    decision in the PR body / commit messages (`--pr-body`). Missing or
//!    `none` fails; `introduce` / `soft-retire` / `hard-delete` passes
//!    with an informational note.
//!
//! Intentionally heuristic. False positives are acceptable — they prompt
//! a human to confirm the decision. False negatives (silently dropping a
//! tracked symbol) are the failure mode this exists to prevent.
//!
//! Exit codes:
//!   0 — clean, or contract decision documented.
//!   1 — contract change detected without a documented decision.
//!   2 — invocation error (bad args, missing files).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Tokens shorter than this are too noisy to track (`a`, `it`, `of`).
const MIN_SYMBOL_LEN: usize = 4;

/// Path of the consumers registry, relative to the repository root.
const DEFAULT_CONSUMERS_DOC: &str = "packages/aethyme/docs/architecture/cross-process-consumers.md";

/// python-retirement Phase 5.5/6: deployed templates project Explore with
/// `aethyme explore-summary --from <json>` and emit their SessionStart
/// envelope with `aethyme repo hook-envelope`. Any reference to a Python
/// interpreter in a canonical text consumer is stale — the product path
/// must work with no Python on PATH (Phase 6 exit criterion).
const STALE_PYTHON_INVOCATIONS: &[&str] = &[
    "python -m src.cli explore",
    ".venv/bin/python -m src.cli explore",
    "\"$AETHYME_PY\" -m src.cli explore",
    "\"$AETHYME_ROOT/.venv/bin/python\" -m src.cli explore",
    ".venv/bin/python",
    "$AETHYME_PY",
];

/// Canonical text consumers scanned for stale invocations, relative to
/// the repository root.
const TEXT_CONSUMER_PATHS: &[&str] = &[
    "packages/aethyme/skills/aethyme/SKILL.md",
    "packages/aethyme/skills/aethyme/AGENTS.md",
    "packages/aethyme/skills/aethyme/references/explore.md",
];

/// Phrases that mark a mention as documentation-of-removal rather than an
/// executable example.
const REMOVAL_MARKERS: &[&str] = &["do not run", "not a valid command", "was removed"];

const USAGE: &str = "\
usage: aethyme broker check-contract [--base <ref>] [--pr-body <file>]
                                     [--consumers-doc <file>]

Refuse diffs that remove cross-process symbols without a declared
contract decision.

  --base <ref>            base ref to diff against (default: origin/main)
  --pr-body <file>        file containing the PR body or commit messages,
                          parsed for the contract decision
  --consumers-doc <file>  override the consumers registry path
                          (default: <repo>/packages/aethyme/docs/\
architecture/cross-process-consumers.md)

Exit codes: 0 clean/declared, 1 undeclared contract change, 2 bad usage.
";

struct Args {
    base: String,
    pr_body: Option<PathBuf>,
    consumers_doc: Option<PathBuf>,
}

/// Run the check. `args` excludes the leading `check-contract` word.
pub fn run(args: &[String]) -> u8 {
    let parsed = match parse_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("ERROR: {message}");
            eprint!("{USAGE}");
            return 2;
        }
    };

    let repo_root = match repo_root() {
        Ok(root) => root,
        Err(message) => {
            eprintln!("ERROR: {message}");
            return 2;
        }
    };

    let consumers_doc = parsed
        .consumers_doc
        .clone()
        .unwrap_or_else(|| repo_root.join(DEFAULT_CONSUMERS_DOC));
    let doc_text = match std::fs::read_to_string(&consumers_doc) {
        Ok(text) => text,
        Err(_) => {
            eprintln!(
                "ERROR: {} not found — cannot determine tracked symbols.",
                consumers_doc.display()
            );
            return 2;
        }
    };
    let tracked = extract_tracked_symbols(&doc_text);
    if tracked.is_empty() {
        eprintln!(
            "ERROR: no tracked symbols extracted from consumers doc — \
             the doc may be empty or malformed."
        );
        return 2;
    }

    let text_violations = find_text_consumer_violations(&text_consumer_checks(&repo_root));
    if !text_violations.is_empty() {
        eprintln!("ERROR: forbidden removed command references found in text consumers:");
        for (path, patterns) in &text_violations {
            eprintln!("  - {path}");
            for pattern in patterns {
                eprintln!("    contains: {pattern}");
            }
        }
        eprintln!(
            "Deployed artifacts and agent instructions must spell commands \
             `aethyme ...` — the product path carries no Python."
        );
        return 1;
    }

    let diff_lines = match read_diff(&repo_root, &parsed.base) {
        Ok(lines) => lines,
        Err(message) => {
            eprintln!("ERROR: {message}");
            return 2;
        }
    };
    if diff_lines.is_empty() {
        println!("clean: empty diff against base, nothing to check.");
        return 0;
    }

    let findings = find_touched_symbols(&diff_lines, &tracked);
    if findings.is_empty() {
        println!(
            "clean: no tracked cross-process symbols touched on removed lines \
             (checked {} symbols).",
            tracked.len()
        );
        return 0;
    }

    let pr_body = parsed
        .pr_body
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let decision = parse_contract_decision(&pr_body);

    println!("Cross-process symbols touched on removed lines:");
    for (symbol, lines) in &findings {
        println!("  - `{symbol}` ({} occurrence(s))", lines.len());
    }
    println!();
    match decision {
        Some(Decision::Introduce) | Some(Decision::SoftRetire) | Some(Decision::HardDelete) => {
            println!(
                "Contract decision in PR body: **{}** — treating as deliberate.",
                decision.expect("matched Some above").label()
            );
            0
        }
        Some(Decision::None) => {
            eprintln!(
                "ERROR: PR contract is **none**, but the diff removes tracked \
                 cross-process symbols. Either pick a different contract label \
                 (introduce / soft-retire / hard-delete) or restore the symbols."
            );
            1
        }
        None => {
            eprintln!(
                "ERROR: PR body does not declare a contract decision \
                 (`none` / `introduce` / `soft-retire` / `hard-delete`). \
                 See `.github/pull_request_template.md`. The 2026-05-08 \
                 playground breakage came from a missing decision here."
            );
            1
        }
    }
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        base: "origin/main".to_string(),
        pr_body: None,
        consumers_doc: None,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(None),
            "--base" => {
                index += 1;
                parsed.base = args
                    .get(index)
                    .ok_or_else(|| "--base requires a value".to_string())?
                    .clone();
            }
            "--pr-body" => {
                index += 1;
                parsed.pr_body = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| "--pr-body requires a value".to_string())?,
                ));
            }
            "--consumers-doc" => {
                index += 1;
                parsed.consumers_doc =
                    Some(PathBuf::from(args.get(index).ok_or_else(|| {
                        "--consumers-doc requires a value".to_string()
                    })?));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 1;
    }
    Ok(Some(parsed))
}

/// The worktree root the diff and the registry are read from. Uses the
/// *current* worktree (not the main checkout): broker sessions run this
/// gate inside their own worktree, where the diff under review lives.
fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let repo =
        crate::GitRepo::discover(&cwd).map_err(|e| format!("not inside a git repository: {e}"))?;
    Ok(repo.root().to_path_buf())
}

fn text_consumer_checks(repo_root: &Path) -> Vec<(PathBuf, &'static [&'static str])> {
    TEXT_CONSUMER_PATHS
        .iter()
        .map(|relative| (repo_root.join(relative), STALE_PYTHON_INVOCATIONS))
        .collect()
}

/// Pull every backtick-wrapped symbol out of the consumers doc.
///
/// The doc uses `code` for everything that has cross-process blast
/// radius: file paths, CLI subcommand names, intent names, schema field
/// names, env vars. The whole set is tracked.
pub fn extract_tracked_symbols(doc_text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in inline_code_tokens(doc_text) {
        let token = token.trim();
        if token.len() < MIN_SYMBOL_LEN {
            continue;
        }
        if is_excluded(token) {
            continue;
        }
        out.insert(token.to_string());
    }
    out
}

/// Scan for `` `content` `` spans, mirroring the original regex
/// `` `([^`\n]+)` `` : content is one-or-more non-backtick characters and
/// cannot span a newline. A stray backtick that finds no partner is
/// skipped, and the next backtick may open a span (regex backtracking
/// semantics — `` ``x`` `` yields `x`).
fn inline_code_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '`' {
                let mut end = index + 1;
                while end < chars.len() && chars[end] != '`' {
                    end += 1;
                }
                if end < chars.len() && end > index + 1 {
                    out.push(chars[index + 1..end].iter().collect());
                    index = end + 1;
                    continue;
                }
            }
            index += 1;
        }
    }
    out
}

/// Symbols too generic to track usefully: bare ALLCAPS like `TODO`, and
/// bare numbers.
fn is_excluded(token: &str) -> bool {
    if token.chars().all(|c| c.is_ascii_uppercase()) {
        return true;
    }
    token.chars().all(|c| c.is_ascii_digit())
}

/// Return the unified diff against `base`.
fn read_diff(repo_root: &Path, base: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["diff", "--unified=0", base, "--", "."])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git diff failed to spawn: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git diff failed".to_string()
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Return `{symbol: [diff_line, …]}` for tracked symbols whose removal
/// appears in the diff. Only removals are inspected:
///
/// - Added lines mentioning a tracked symbol are usually new consumers —
///   those should be added to the registry, but they don't break anything
///   by being added.
/// - Removed lines are the operation that breaks downstream consumers
///   (the entry point is gone; callers crash).
///
/// A symbol is only reported when the diff REDUCES its occurrences:
/// removed-count > added-count. Rewriting a line — which is how every
/// edit to a registry row appears, since a row is one long line — shows
/// each of its symbols as both removed and added, and reduces nothing.
/// Counting only removals flagged those rewrites (2026-08-07 sweep hit
/// this while repairing a stale row), and the only way past the gate was
/// to attach an introduce/soft-retire/hard-delete label to a change that
/// did none of those things. A guard that can only be satisfied by
/// mislabelling corrupts the signal it exists to give, so it counts both
/// sides now.
pub fn find_touched_symbols(
    diff_lines: &[String],
    tracked: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut removed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut added: BTreeMap<String, usize> = BTreeMap::new();
    for line in diff_lines {
        // `--- a/path` and `+++ b/path` appear at hunk boundaries. Skip
        // them; real removals are `-text`, real additions `+text`.
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if let Some(body) = line.strip_prefix('-') {
            for symbol in tracked {
                if body.contains(symbol.as_str()) {
                    removed
                        .entry(symbol.clone())
                        .or_default()
                        .push(line.clone());
                }
            }
        } else if let Some(body) = line.strip_prefix('+') {
            for symbol in tracked {
                if body.contains(symbol.as_str()) {
                    *added.entry(symbol.clone()).or_default() += 1;
                }
            }
        }
    }
    removed
        .into_iter()
        .filter(|(symbol, lines)| lines.len() > added.get(symbol).copied().unwrap_or(0))
        .collect()
}

/// The contract decision an author declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    None,
    Introduce,
    SoftRetire,
    HardDelete,
}

impl Decision {
    pub fn label(self) -> &'static str {
        match self {
            Decision::None => "none",
            Decision::Introduce => "introduce",
            Decision::SoftRetire => "soft-retire",
            Decision::HardDelete => "hard-delete",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        match label.to_ascii_lowercase().as_str() {
            "none" => Some(Decision::None),
            "introduce" => Some(Decision::Introduce),
            "soft-retire" => Some(Decision::SoftRetire),
            "hard-delete" => Some(Decision::HardDelete),
            _ => None,
        }
    }
}

/// Find the contract decision the author declared.
///
/// Two accepted spellings:
/// - PR template checkbox: `- [x] **<label>**` (the GitHub PR path).
/// - Commit-message line: `Contract decision: <label>` (the broker path,
///   where submissions have no PR body and the decision lives in commit
///   messages — added 2026-07-27 when the checker became a broker gate;
///   the 12-day `query deps` break shipped through a broker submission
///   the PR-only checker never saw).
///
/// If neither appears, the contract is undeclared. When several are
/// declared (mistake or indecision), the most-restrictive wins so a
/// co-checked `none` cannot fool the check.
pub fn parse_contract_decision(pr_body: &str) -> Option<Decision> {
    let mut matches: Vec<Decision> = Vec::new();
    matches.extend(checkbox_decisions(pr_body));
    matches.extend(commit_line_decisions(pr_body));
    if matches.is_empty() {
        return None;
    }
    for tier in [
        Decision::HardDelete,
        Decision::SoftRetire,
        Decision::Introduce,
        Decision::None,
    ] {
        if matches.contains(&tier) {
            return Some(tier);
        }
    }
    matches.first().copied()
}

/// `-\s*\[\s*[xX]\s*\]\s*\*\*(label)\*\*`, case-insensitive.
fn checkbox_decisions(text: &str) -> Vec<Decision> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '-' {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        if !(skip_ws(&chars, &mut cursor)
            && expect(&chars, &mut cursor, '[')
            && skip_ws(&chars, &mut cursor)
            && expect_checked(&chars, &mut cursor)
            && skip_ws(&chars, &mut cursor)
            && expect(&chars, &mut cursor, ']')
            && skip_ws(&chars, &mut cursor)
            && expect(&chars, &mut cursor, '*')
            && expect(&chars, &mut cursor, '*'))
        {
            index += 1;
            continue;
        }
        let start = cursor;
        while cursor < chars.len() && (chars[cursor].is_ascii_alphabetic() || chars[cursor] == '-')
        {
            cursor += 1;
        }
        let label: String = chars[start..cursor].iter().collect();
        if expect(&chars, &mut cursor, '*') && expect(&chars, &mut cursor, '*') {
            if let Some(decision) = Decision::from_label(&label) {
                out.push(decision);
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    out
}

/// `^\s*Contract decision:\s*(label)\b`, case-insensitive, per line.
fn commit_line_decisions(text: &str) -> Vec<Decision> {
    const PREFIX: &str = "contract decision:";
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        // `get` rather than `[..]`: PREFIX.len() is a BYTE offset, and a
        // commit body line whose 18th byte falls inside a multi-byte
        // character (an em dash at column 17, say) would panic the whole
        // checker on a plain string slice. `None` is the right answer
        // anyway — PREFIX is pure ASCII, so a line that does not split
        // cleanly there cannot start with it.
        let Some(head) = trimmed.get(..PREFIX.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(PREFIX) {
            continue;
        }
        let rest = trimmed[PREFIX.len()..].trim_start();
        let end = rest
            .find(|c: char| !(c.is_ascii_alphabetic() || c == '-'))
            .unwrap_or(rest.len());
        // `\b` after the label: the token must not continue into another
        // word character. Trailing `-` is part of the label alternatives
        // themselves (`soft-retire`), so the greedy scan above already
        // consumed it.
        if let Some(decision) = Decision::from_label(&rest[..end]) {
            out.push(decision);
        }
    }
    out
}

fn skip_ws(chars: &[char], cursor: &mut usize) -> bool {
    while *cursor < chars.len() && chars[*cursor].is_whitespace() {
        *cursor += 1;
    }
    true
}

fn expect(chars: &[char], cursor: &mut usize, expected: char) -> bool {
    if chars.get(*cursor) == Some(&expected) {
        *cursor += 1;
        true
    } else {
        false
    }
}

fn expect_checked(chars: &[char], cursor: &mut usize) -> bool {
    match chars.get(*cursor) {
        Some('x') | Some('X') => {
            *cursor += 1;
            true
        }
        _ => false,
    }
}

/// Return forbidden command references in canonical text consumers.
pub fn find_text_consumer_violations(
    checks: &[(PathBuf, &'static [&'static str])],
) -> BTreeMap<String, Vec<String>> {
    let mut violations: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, forbidden_patterns) in checks {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut hits: Vec<String> = Vec::new();
        for line in text.lines() {
            let normalized = line.trim().to_ascii_lowercase();
            for pattern in *forbidden_patterns {
                if !line.contains(pattern) {
                    continue;
                }
                if REMOVAL_MARKERS
                    .iter()
                    .any(|marker| normalized.contains(marker))
                {
                    continue;
                }
                hits.push((*pattern).to_string());
            }
        }
        if !hits.is_empty() {
            violations.insert(path.display().to_string(), hits);
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tracked_symbols_strips_short_and_noisy_tokens() {
        // The MIN_SYMBOL_LEN cutoff drops tokens like `a`, `it`, `of`
        // that would otherwise produce thousands of false positives.
        let sample = "First, see `foo` (too short — dropped).\n\
             Second, `aethyme-explore` is tracked.\n\
             Third, `usage_boundary_query` is tracked.\n\
             Bare numbers `2024` should be dropped (excluded).\n\
             ALLCAPS `TODO` should be dropped (excluded).\n";
        let tracked = extract_tracked_symbols(sample);
        assert!(tracked.contains("aethyme-explore"));
        assert!(tracked.contains("usage_boundary_query"));
        assert!(!tracked.contains("foo"));
        assert!(!tracked.contains("2024"));
        assert!(!tracked.contains("TODO"));
    }

    #[test]
    fn commit_line_decisions_reads_the_label() {
        assert_eq!(
            commit_line_decisions("Contract decision: hard-delete (src/ is gone)\n"),
            vec![Decision::HardDelete]
        );
        assert_eq!(
            commit_line_decisions("  contract decision:none\n"),
            vec![Decision::None]
        );
        assert!(commit_line_decisions("Contract decision: bogus\n").is_empty());
    }

    /// Regression, 2026-08-06: the prefix comparison sliced the line at a
    /// BYTE offset, so a body line whose 18th byte landed inside a
    /// multi-byte character panicked the whole checker — taking the
    /// `cross-process-contract` gate down with it. Found by running the
    /// gate over this very migration's commit bodies.
    #[test]
    fn commit_line_decisions_survives_multibyte_lines() {
        // The em dash straddles byte 18 of this line.
        let body = "Rationale: this — an em dash — sits across the prefix window.\n\
             Contract decision: none (still parsed)\n";
        assert_eq!(commit_line_decisions(body), vec![Decision::None]);
        // Shorter than the prefix, and non-ASCII: must not panic either.
        assert!(commit_line_decisions("é\n").is_empty());
    }

    #[test]
    fn inline_code_scan_matches_regex_backtracking() {
        assert_eq!(inline_code_tokens("``xyzw``"), vec!["xyzw".to_string()]);
        assert_eq!(
            inline_code_tokens("`abcd` and `efgh`"),
            vec!["abcd".to_string(), "efgh".to_string()]
        );
        // An unpaired backtick opens nothing.
        assert_eq!(inline_code_tokens("`abcd` and `efgh"), vec!["abcd"]);
        // Content cannot span lines.
        assert!(inline_code_tokens("`abcd\nefgh`").is_empty());
    }

    fn tracked_explore() -> BTreeSet<String> {
        ["aethyme-explore".to_string()].into_iter().collect()
    }

    fn diff_of(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_touched_symbols_only_flags_removals() {
        // Added lines mentioning a tracked symbol are usually new
        // consumers — they don't break things by being added. A pure
        // removal, with nothing added back, is the dangerous direction.
        let diff = diff_of(&[
            "--- a/foo",
            "+++ b/foo",
            "+ adding some-other-thing (this is fine)",
            "  context line mentions aethyme-explore (unchanged)",
            "- removing aethyme-explore (this is the dangerous direction)",
        ]);
        let findings = find_touched_symbols(&diff, &tracked_explore());
        let hits = findings.get("aethyme-explore").expect("symbol flagged");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].starts_with('-'));
    }

    #[test]
    fn find_touched_symbols_ignores_in_place_rewrites() {
        // Editing a registry row rewrites one long line, so every symbol
        // in it appears as both removed and added. Nothing is reduced,
        // so nothing is flagged — otherwise the only way to reword a row
        // is to attach a contract label to a change that retires nothing.
        let diff = diff_of(&[
            "--- a/registry.md",
            "+++ b/registry.md",
            "- | `aethyme-explore` | old wording |",
            "+ | `aethyme-explore` | new wording |",
        ]);
        assert!(find_touched_symbols(&diff, &tracked_explore()).is_empty());
    }

    #[test]
    fn find_touched_symbols_flags_a_net_reduction() {
        // Two mentions removed, one added back: the symbol lost ground,
        // so the author still owes a decision.
        let diff = diff_of(&[
            "--- a/registry.md",
            "+++ b/registry.md",
            "- row one cites `aethyme-explore`",
            "- row two cites `aethyme-explore`",
            "+ merged row cites `aethyme-explore`",
        ]);
        let findings = find_touched_symbols(&diff, &tracked_explore());
        assert_eq!(findings.get("aethyme-explore").map(Vec::len), Some(2));
    }

    #[test]
    fn find_touched_symbols_skips_diff_headers() {
        // `--- a/foo` starts with `-` and would otherwise be
        // misclassified as a removal.
        let tracked: BTreeSet<String> = ["aethyme-explore".to_string()].into_iter().collect();
        let diff: Vec<String> = [
            "--- a/aethyme-explore",
            "+++ b/aethyme-explore",
            "-some other line",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(find_touched_symbols(&diff, &tracked).is_empty());
    }

    #[test]
    fn parse_contract_decision_picks_checked_label() {
        assert_eq!(
            parse_contract_decision("- [x] **soft-retire** — deprecated"),
            Some(Decision::SoftRetire)
        );
    }

    #[test]
    fn parse_contract_decision_returns_none_when_unchecked() {
        assert_eq!(
            parse_contract_decision("- [ ] **none**\n- [ ] **introduce**"),
            None
        );
    }

    #[test]
    fn parse_contract_decision_prefers_most_restrictive_when_multiple() {
        // A co-checked `none` must not fool the check.
        assert_eq!(
            parse_contract_decision("- [x] **none**\n- [x] **hard-delete**\n"),
            Some(Decision::HardDelete)
        );
    }

    #[test]
    fn parse_contract_decision_accepts_commit_message_line() {
        // Broker submissions have no PR body; the decision lives in
        // commit messages and the gate feeds `git log` output as the body.
        let body = "Fix the deps wrapper\n\nContract decision: soft-retire\n\nCo-Authored-By: x";
        assert_eq!(parse_contract_decision(body), Some(Decision::SoftRetire));
    }

    #[test]
    fn parse_contract_decision_commit_line_is_case_insensitive_and_wins_by_tier() {
        let body = "commit A\n\ncontract decision: NONE\n\n\
             commit B\n\nContract decision: hard-delete\n";
        assert_eq!(parse_contract_decision(body), Some(Decision::HardDelete));
    }

    #[test]
    fn parse_contract_decision_ignores_prose_mentions() {
        // A sentence merely *discussing* decisions must not count as one.
        assert_eq!(
            parse_contract_decision("We should think about the contract decision: maybe later."),
            None
        );
    }

    #[test]
    fn parse_contract_decision_handles_empty_body() {
        assert_eq!(parse_contract_decision(""), None);
    }

    #[test]
    fn find_text_consumer_violations_flags_stale_executable_examples() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill = dir.path().join("SKILL.md");
        std::fs::write(
            &skill,
            "Quick start:\n\"$AETHYME_ROOT/.venv/bin/python\" -m src.cli explore --repo \"$PWD\"\n",
        )
        .expect("write");
        const PATTERNS: &[&str] = &["\"$AETHYME_ROOT/.venv/bin/python\" -m src.cli explore"];
        let violations = find_text_consumer_violations(&[(skill.clone(), PATTERNS)]);
        assert!(violations.contains_key(&skill.display().to_string()));
    }

    #[test]
    fn find_text_consumer_violations_allows_explicit_removed_command_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill = dir.path().join("SKILL.md");
        std::fs::write(
            &skill,
            "Do not run `python -m src.cli explore`; it was removed.\n",
        )
        .expect("write");
        const PATTERNS: &[&str] = &["python -m src.cli explore"];
        assert!(find_text_consumer_violations(&[(skill, PATTERNS)]).is_empty());
    }

    #[test]
    fn consumers_doc_yields_real_tracked_symbols() {
        // End-to-end: the actual registry must produce a sane tracked set
        // including the high-blast-radius names. If this set is empty or
        // tiny the check silently no-ops in CI.
        let doc = repo_relative(DEFAULT_CONSUMERS_DOC);
        let text =
            std::fs::read_to_string(&doc).unwrap_or_else(|e| panic!("read {}: {e}", doc.display()));
        let tracked = extract_tracked_symbols(&text);
        assert!(
            tracked.len() >= 20,
            "tracked set suspiciously small: {}",
            tracked.len()
        );
        assert!(tracked.iter().any(|s| s.contains("aethyme-explore")));
        assert!(tracked.iter().any(|s| s.contains("SKILL.md")));
    }

    #[test]
    fn current_text_consumers_have_no_executable_python_guidance() {
        let root = repo_relative(".");
        assert!(find_text_consumer_violations(&text_consumer_checks(&root)).is_empty());
    }

    /// Resolve a path relative to the monorepo root from the crate dir.
    fn repo_relative(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(5)
            .expect("monorepo root above crates/<crate>/rust/packages/aethyme")
            .join(relative)
    }
}
