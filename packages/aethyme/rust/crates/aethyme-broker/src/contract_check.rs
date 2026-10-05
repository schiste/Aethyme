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
//!    decision in every source the caller names: a PR body (`--pr-body`),
//!    the commits in `<base>..HEAD` (`--commit-messages`), and, only when
//!    those declare nothing that passes, the pull requests GitHub
//!    associates with HEAD (`--merged-pr`). Missing or unjustified `none`
//!    fails and names each source it read; `introduce` / `soft-retire` /
//!    `hard-delete` passes with an informational note.
//!
//! The PR workflow and the gate pass the same decision sources, so a
//! decision accepted on a pull request is accepted again on its merge
//! commit. Before 2026-10-04 the gate read only commit messages: PR #514
//! declared its decision in the PR body, passed the PR check, and then
//! failed `Aethyme Gates` on main, where no PR body was read.
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
                                     [--commit-messages] [--merged-pr]
                                     [--consumers-doc <file>]

Refuse diffs that remove cross-process symbols without a declared
contract decision.

  --base <ref>            base ref to diff against (default: origin/main)
  --pr-body <file>        file containing the PR body (or any text),
                          parsed for the contract decision
  --commit-messages       also read the messages of the commits in
                          <base>..HEAD
  --merged-pr             when no other source declares a decision, ask
                          GitHub (through `gh`) for the pull requests
                          associated with HEAD and read their bodies; an
                          unavailable lookup is reported, never passed
  --consumers-doc <file>  override the consumers registry path
                          (default: <repo>/packages/aethyme/docs/\
architecture/cross-process-consumers.md)

Exit codes: 0 clean/declared, 1 undeclared contract change, 2 bad usage.
";

struct Args {
    base: String,
    pr_body: Option<PathBuf>,
    commit_messages: bool,
    merged_pr: bool,
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

    let mut sources = Vec::new();
    if let Some(path) = &parsed.pr_body {
        sources.push(DecisionSource {
            label: format!("PR body ({})", path.display()),
            text: std::fs::read_to_string(path)
                .map_err(|error| format!("could not read the file: {error}")),
        });
    }
    if parsed.commit_messages {
        sources.push(commit_message_source(&repo_root, &parsed.base));
    }
    let lookup = || associated_pull_request_sources(&repo_root);
    let merged_pr: Option<&dyn Fn() -> Vec<DecisionSource>> = if parsed.merged_pr {
        Some(&lookup)
    } else {
        None
    };
    let (sources, verdict) = decide(sources, merged_pr);

    println!("Cross-process symbols touched on removed lines:");
    for (symbol, lines) in &findings {
        println!("  - `{symbol}` ({} occurrence(s))", lines.len());
    }
    println!();
    match verdict {
        Verdict::Declared { decision, source } => {
            println!(
                "Contract decision in {source}: **{}** — treating as deliberate.",
                decision.label()
            );
            0
        }
        Verdict::JustifiedNone {
            source,
            justification,
        } => {
            // The finding stays printed above; this records why the author
            // says it is spurious, so a reviewer sees both.
            println!(
                "Contract decision in {source}: **none**, justified — treating \
                 as deliberate.\n  justification: {justification}"
            );
            0
        }
        Verdict::UnjustifiedNone | Verdict::Undeclared => {
            eprint!("{}", failure_message(&verdict, &sources));
            1
        }
    }
}

/// One place a contract decision may be declared, and what reading it gave:
/// its text, or why it could not be read.
///
/// Sources are kept apart rather than concatenated up front so that a failure
/// can say where it looked. The gate that turned main red on 2026-10-04 read
/// only commit messages while its error blamed the "PR body", and the PR body
/// was exactly where the decision was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionSource {
    pub label: String,
    pub text: Result<String, String>,
}

/// What the declared decisions, taken together, amount to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// `introduce` / `soft-retire` / `hard-delete`, found in `source`.
    Declared { decision: Decision, source: String },
    /// `none`, with a `Contract justification:` line.
    JustifiedNone {
        source: String,
        justification: String,
    },
    /// `none`, and nothing says why the finding is spurious.
    UnjustifiedNone,
    /// No source declares anything.
    Undeclared,
}

impl Verdict {
    fn passes(&self) -> bool {
        matches!(
            self,
            Verdict::Declared { .. } | Verdict::JustifiedNone { .. }
        )
    }
}

/// Judge the readable sources together: the most restrictive decision any of
/// them declares wins, exactly as it did when they were one text.
pub fn judge(sources: &[DecisionSource]) -> Verdict {
    let readable: Vec<(&str, &str)> = sources
        .iter()
        .filter_map(|source| {
            source
                .text
                .as_deref()
                .ok()
                .map(|text| (source.label.as_str(), text))
        })
        .collect();
    let combined = readable
        .iter()
        .map(|(_, text)| *text)
        .collect::<Vec<_>>()
        .join("\n");
    let Some(decision) = parse_contract_decision(&combined) else {
        return Verdict::Undeclared;
    };
    let source = readable
        .iter()
        .find(|(_, text)| parse_contract_decision(text) == Some(decision))
        .map(|(label, _)| (*label).to_string())
        .unwrap_or_default();
    if decision != Decision::None {
        return Verdict::Declared { decision, source };
    }
    match parse_contract_justification(&combined) {
        Some(justification) => Verdict::JustifiedNone {
            source,
            justification,
        },
        None => Verdict::UnjustifiedNone,
    }
}

/// Judge `sources`; when they do not pass and a pull-request lookup is
/// available, consult it and judge again.
///
/// The lookup runs only when it could change the outcome, so a broker
/// submission whose commits carry the decision never reaches the network.
pub fn decide(
    mut sources: Vec<DecisionSource>,
    pull_requests: Option<&dyn Fn() -> Vec<DecisionSource>>,
) -> (Vec<DecisionSource>, Verdict) {
    let verdict = judge(&sources);
    if verdict.passes() {
        return (sources, verdict);
    }
    let Some(lookup) = pull_requests else {
        return (sources, verdict);
    };
    sources.extend(lookup());
    let verdict = judge(&sources);
    (sources, verdict)
}

/// The refusal, naming every place the check looked and what each held.
pub fn failure_message(verdict: &Verdict, sources: &[DecisionSource]) -> String {
    let mut out = match verdict {
        Verdict::UnjustifiedNone => format!(
            "ERROR: the contract decision is **none**, but the diff removes tracked \
             cross-process symbols. Either pick a different contract label \
             (introduce / soft-retire / hard-delete), restore the symbols, or state \
             why the match is spurious on a line beginning `Contract justification:` \
             (at least {MIN_JUSTIFICATION_CHARS} characters). The matcher reads diff \
             text, so it cannot tell a name leaving a comment or a string from an \
             entry point leaving the product.\n"
        ),
        _ => "ERROR: no contract decision (`none` / `introduce` / `soft-retire` / \
              `hard-delete`) is declared. Declare one in the PR body (see \
              `.github/pull_request_template.md`) or on a `Contract decision: <label>` \
              line in a commit message. The 2026-05-08 playground breakage came from \
              a missing decision.\n"
            .to_string(),
    };
    out.push_str("Looked in:\n");
    if sources.is_empty() {
        out.push_str("  - nothing: no --pr-body, --commit-messages or --merged-pr was given\n");
    }
    for source in sources {
        let status = match &source.text {
            Err(reason) => format!("unavailable — {reason}"),
            Ok(text) => match parse_contract_decision(text) {
                Some(decision) => format!("declares **{}**", decision.label()),
                None => "no contract decision".to_string(),
            },
        };
        out.push_str(&format!("  - {}: {status}\n", source.label));
    }
    out
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        base: "origin/main".to_string(),
        pr_body: None,
        commit_messages: false,
        merged_pr: false,
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
            "--commit-messages" => parsed.commit_messages = true,
            "--merged-pr" => parsed.merged_pr = true,
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

/// The messages of the commits in `<base>..HEAD`: the broker path, where a
/// submission has no PR body and the decision travels in a commit.
fn commit_message_source(repo_root: &Path, base: &str) -> DecisionSource {
    let range = format!("{base}..HEAD");
    let label = format!("commit messages {range}");
    let output = crate::git::git_command()
        .args(["log", "--format=%B", &range, "--"])
        .current_dir(repo_root)
        .output();
    let text = match output {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => Err(format!("git log failed to spawn: {error}")),
    };
    DecisionSource { label, text }
}

/// The bodies of the pull requests GitHub associates with HEAD.
///
/// On main HEAD is the merge (or squash) commit of the PR that was just
/// merged, and that PR's body is where the repository's convention puts the
/// decision. A failed lookup is a source with a reason, not an empty body:
/// the check must not pass because it could not see.
fn associated_pull_request_sources(repo_root: &Path) -> Vec<DecisionSource> {
    let head = match crate::git::git_command()
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            return vec![DecisionSource {
                label: "pull requests associated with HEAD".to_string(),
                text: Err("could not resolve HEAD".to_string()),
            }];
        }
    };
    let short = &head[..head.len().min(12)];
    let label = format!("pull requests associated with HEAD {short}");
    let output = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{{owner}}/{{repo}}/commits/{head}/pulls"),
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(std::process::Stdio::null())
        .current_dir(repo_root)
        .output();
    let result = match output {
        Err(error) => Err(format!(
            "could not run `gh` ({error}); the lookup needs the GitHub CLI and a \
             token (GH_TOKEN)"
        )),
        Ok(output) if !output.status.success() => Err(format!(
            "`gh api repos/{{owner}}/{{repo}}/commits/{short}/pulls` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(output) => associated_pull_requests(&String::from_utf8_lossy(&output.stdout)),
    };
    match result {
        Err(reason) => vec![DecisionSource {
            label,
            text: Err(reason),
        }],
        Ok(pulls) if pulls.is_empty() => vec![DecisionSource {
            label,
            text: Err("GitHub associates no pull request with this commit".to_string()),
        }],
        Ok(pulls) => pulls
            .into_iter()
            .map(|(number, body)| DecisionSource {
                label: format!("PR #{number} body (associated with HEAD {short})"),
                text: Ok(body),
            })
            .collect(),
    }
}

/// `(number, body)` for each pull request in a
/// `GET /repos/{owner}/{repo}/commits/{sha}/pulls` response.
pub fn associated_pull_requests(json: &str) -> Result<Vec<(u64, String)>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("unreadable GitHub response: {error}"))?;
    let pulls = value
        .as_array()
        .ok_or_else(|| "unexpected GitHub response: not a list of pull requests".to_string())?;
    Ok(pulls
        .iter()
        .filter_map(|pull| {
            let number = pull.get("number")?.as_u64()?;
            let body = pull
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((number, body))
        })
        .collect())
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
    let output = crate::git::git_command()
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
/// Whether `body` mentions `symbol` as a symbol rather than as a fragment of a
/// longer name.
///
/// A plain substring test reports a removal whenever a tracked name appears
/// anywhere inside another identifier: deleting `changed_paths: Vec::new()`
/// was reported as removing the tracked `paths`, which left the author with no
/// truthful contract label to declare (#263). Identifier characters on either
/// side mean this is a different name, so the occurrence says nothing about
/// the tracked one.
///
/// Deliberately still matches inside comments and string literals. Command
/// names reach their dispatch as string literals, so skipping those would turn
/// a genuine removal into silence -- the failure this check exists to prevent.
fn mentions_symbol(body: &str, symbol: &str) -> bool {
    if symbol.is_empty() {
        return false;
    }
    let bytes = body.as_bytes();
    let mut from = 0;
    while let Some(offset) = body[from..].find(symbol) {
        let start = from + offset;
        let end = start + symbol.len();
        let before_joins = start
            .checked_sub(1)
            .is_some_and(|index| is_symbol_byte(bytes[index]));
        let after_joins = bytes.get(end).copied().is_some_and(is_symbol_byte);
        if !before_joins && !after_joins {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Bytes that can continue an identifier, and so join a match to its
/// neighbour. `-` counts because tracked names are CLI spellings such as
/// `check-contract`, where a hyphen is part of the name rather than a break.
fn is_symbol_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

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
                if mentions_symbol(body, symbol) {
                    removed
                        .entry(symbol.clone())
                        .or_default()
                        .push(line.clone());
                }
            }
        } else if let Some(body) = line.strip_prefix('+') {
            for symbol in tracked {
                if mentions_symbol(body, symbol) {
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

/// Check a broker submission's contract decision before its gates start.
///
/// The merged-tree gate remains authoritative, but a missing decision in an
/// associated PR body is already knowable from the session worktree. Catching
/// it here avoids building the CLI inside `cross-process-contract` only to
/// report a text-only omission after the rest of submit has begun.
pub fn preflight_submit_decision(
    repo_root: &Path,
    base: &str,
    pending_commit_messages: &str,
) -> Result<(), String> {
    let consumers_doc = repo_root.join(DEFAULT_CONSUMERS_DOC);
    let doc_text = match std::fs::read_to_string(&consumers_doc) {
        Ok(doc_text) => doc_text,
        // The broker is also used in repositories that do not carry Aethyme's
        // consumer inventory. Without that inventory there are no tracked
        // symbols to preflight; the merged-tree gate remains authoritative.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "submit preflight could not read {}: {error}",
                consumers_doc.display()
            ));
        }
    };
    let tracked = extract_tracked_symbols(&doc_text);
    if tracked.is_empty() {
        return Err(format!(
            "submit preflight could not extract tracked symbols from {}",
            consumers_doc.display()
        ));
    }
    let diff_lines = read_diff(repo_root, base)
        .map_err(|message| format!("submit preflight could not inspect {base}..HEAD: {message}"))?;
    let findings = find_touched_symbols(&diff_lines, &tracked);
    if findings.is_empty() {
        return Ok(());
    }

    let sources = vec![DecisionSource {
        label: "pending session commit messages".to_string(),
        text: Ok(pending_commit_messages.to_string()),
    }];
    let lookup = || associated_pull_request_sources(repo_root);
    validate_submit_decision(&findings, sources, &lookup)
}

fn validate_submit_decision(
    findings: &BTreeMap<String, Vec<String>>,
    sources: Vec<DecisionSource>,
    pull_requests: &dyn Fn() -> Vec<DecisionSource>,
) -> Result<(), String> {
    if findings.is_empty() {
        return Ok(());
    }
    let (sources, verdict) = decide(sources, Some(pull_requests));
    if verdict.passes() {
        return Ok(());
    }

    Err(format!(
        "ERROR: contract-decision preflight failed before any gate ran.\n{}\
         Add `Contract decision: <none|introduce|soft-retire|hard-delete>` to the PR body or a commit message.\n\
         If `none` is correct for a tracked removal, also add `Contract justification: <reason>` (at least {MIN_JUSTIFICATION_CHARS} characters).\n",
        failure_message(&verdict, &sources)
    ))
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
/// Shortest justification that says anything. Long enough to exclude "n/a"
/// and "see above", short enough not to demand an essay.
const MIN_JUSTIFICATION_CHARS: usize = 24;

/// A stated reason why a reported removal is not an interface change.
///
/// The matcher is a heuristic over diff text: it can see that a tracked name
/// left a removed line, but not whether an entry point left the product. When
/// it is wrong there is otherwise no truthful label -- `none` is refused, and
/// every other label asserts a retirement that did not happen -- so the author
/// is left choosing between mislabelling, rewording code to move a substring,
/// and bypassing the gate. This is the fourth option: say why, on the record.
///
/// It is deliberately not a silencer. The finding is still printed, the
/// justification is printed beside it, and both land in the run log where a
/// reviewer can disagree.
pub fn parse_contract_justification(pr_body: &str) -> Option<String> {
    pr_body.lines().find_map(|line| {
        let rest = line
            .trim()
            .trim_start_matches(['-', '*', '#', ' '])
            .strip_prefix("Contract justification:")
            .or_else(|| {
                line.trim()
                    .trim_start_matches(['-', '*', '#', ' '])
                    .strip_prefix("contract justification:")
            })?;
        let reason = rest.trim();
        (reason.chars().count() >= MIN_JUSTIFICATION_CHARS).then(|| reason.to_string())
    })
}

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
        if expect(&chars, &mut cursor, '*')
            && expect(&chars, &mut cursor, '*')
            && let Some(decision) = Decision::from_label(&label)
        {
            out.push(decision);
            index = cursor;
            continue;
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

    /// A tracked name inside a longer identifier is a different name.
    ///
    /// Deleting `changed_paths: Vec::new()` was reported as removing the
    /// tracked `paths`, and once reported there is no truthful label left:
    /// `none` is refused, and every other label asserts a retirement that did
    /// not happen (#263).
    #[test]
    fn a_tracked_name_inside_a_longer_identifier_is_not_a_removal() {
        let tracked = BTreeSet::from(["paths".to_string(), "graph".to_string()]);
        let diff = vec![
            "-            changed_paths: Vec::new(),".to_string(),
            "-    let graphs = load();".to_string(),
            "-    call_graph_builder();".to_string(),
        ];
        assert!(
            find_touched_symbols(&diff, &tracked).is_empty(),
            "changed_paths, graphs and call_graph_builder are other names"
        );
    }

    /// The narrowing must not cost the detection the check exists for.
    ///
    /// Command names reach their dispatch as string literals, so a removal
    /// inside quotes is exactly the case worth catching.
    #[test]
    fn a_standalone_tracked_name_is_still_a_removal_even_in_a_string() {
        let tracked = BTreeSet::from(["paths".to_string(), "graph".to_string()]);
        let diff = vec![
            "-        \"graph\" => run_graph(tail),".to_string(),
            "-    let paths = collect();".to_string(),
        ];
        let findings = find_touched_symbols(&diff, &tracked);
        assert_eq!(findings.len(), 2, "both removals must still be reported");
        assert!(findings.contains_key("graph"));
        assert!(findings.contains_key("paths"));
    }

    /// Hyphens belong to CLI spellings, so they join rather than separate.
    #[test]
    fn a_hyphenated_name_does_not_match_a_longer_hyphenated_one() {
        let tracked = BTreeSet::from(["check-contract".to_string()]);
        let removed_longer = vec!["-    run(\"check-contract-plan\");".to_string()];
        assert!(find_touched_symbols(&removed_longer, &tracked).is_empty());
        let removed_exact = vec!["-    run(\"check-contract\");".to_string()];
        assert_eq!(find_touched_symbols(&removed_exact, &tracked).len(), 1);
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

    /// A justification has to say something. Token reasons would make the
    /// escape hatch a rubber stamp, which is worse than not having one.
    #[test]
    fn a_token_justification_is_not_a_justification() {
        for weak in ["n/a", "see above", "spurious", "  ", "false positive"] {
            assert_eq!(
                parse_contract_justification(&format!("Contract justification: {weak}")),
                None,
                "{weak:?} should not count as a stated reason"
            );
        }
    }

    /// A real reason is accepted, and is returned verbatim so it can be
    /// printed beside the finding rather than replacing it.
    #[test]
    fn a_stated_reason_is_accepted_and_preserved() {
        let body = "## Contract decision\n\n- [x] **none**\n\n                    Contract justification: the name appears only inside a deleted \
                    error-message string; no entry point changes.\n";
        let reason = parse_contract_justification(body).expect("a real reason is accepted");
        assert!(reason.starts_with("the name appears only inside"));
        assert!(reason.ends_with("no entry point changes."));
    }

    /// The marker must be a line of its own, not a phrase in prose, so that
    /// discussing the mechanism in a PR body does not silently satisfy it.
    #[test]
    fn prose_mentioning_the_marker_does_not_justify() {
        let body = "We considered whether a Contract justification: line would help here.";
        assert_eq!(parse_contract_justification(body), None);
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
    fn submit_preflight_skips_lookup_without_tracked_removals() {
        let looked_up = std::cell::Cell::new(false);
        let lookup = || {
            looked_up.set(true);
            Vec::new()
        };
        assert!(validate_submit_decision(&BTreeMap::new(), Vec::new(), &lookup).is_ok());
        assert!(!looked_up.get());
    }

    #[test]
    fn submit_preflight_skips_repositories_without_consumer_inventory() {
        let repo = tempfile::tempdir().expect("temporary repository");
        assert!(preflight_submit_decision(repo.path(), "HEAD", "").is_ok());
    }

    #[test]
    fn submit_preflight_accepts_the_associated_pr_body_decision() {
        let findings = BTreeMap::from([(
            "aethyme-explore".to_string(),
            vec!["-    run(\"aethyme-explore\");".to_string()],
        )]);
        let lookup = || {
            vec![DecisionSource {
                label: "PR #42 body".to_string(),
                text: Ok("Contract decision: hard-delete".to_string()),
            }]
        };

        assert!(validate_submit_decision(&findings, Vec::new(), &lookup).is_ok());
    }

    #[test]
    fn submit_preflight_refusal_names_the_required_lines_before_gates() {
        let findings = BTreeMap::from([(
            "aethyme-explore".to_string(),
            vec!["-    run(\"aethyme-explore\");".to_string()],
        )]);
        let lookup = || {
            vec![DecisionSource {
                label: "PR #42 body".to_string(),
                text: Ok("No decision declared".to_string()),
            }]
        };

        let error = validate_submit_decision(&findings, Vec::new(), &lookup).unwrap_err();
        assert!(error.contains("before any gate ran"));
        assert!(error.contains("Contract decision: <none|introduce|soft-retire|hard-delete>"));
        assert!(error.contains("Contract justification: <reason>"));
        assert!(error.contains("PR #42 body: no contract decision"));
    }

    #[test]
    fn submit_preflight_needs_a_reason_for_none_on_a_tracked_removal() {
        let findings = BTreeMap::from([(
            "aethyme-explore".to_string(),
            vec!["-    run(\"aethyme-explore\");".to_string()],
        )]);
        let lookup = || {
            vec![DecisionSource {
                label: "PR #42 body".to_string(),
                text: Ok("Contract decision: none".to_string()),
            }]
        };

        let error = validate_submit_decision(&findings, Vec::new(), &lookup).unwrap_err();
        assert!(error.contains("Contract justification:"));
        assert!(error.contains("before any gate ran"));
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

    fn source(label: &str, text: &str) -> DecisionSource {
        DecisionSource {
            label: label.to_string(),
            text: Ok(text.to_string()),
        }
    }

    /// The main-branch shape of #514: the merged commits carry no decision,
    /// the PR body does. The gate must accept on main what the PR check
    /// accepted on the pull request.
    #[test]
    fn a_decision_only_in_the_merged_pr_body_passes() {
        let commits = vec![source(
            "commit messages HEAD~1..HEAD",
            "Merge pull request #514 from schiste/agent/x\n\nfix(broker): report behind\n",
        )];
        let lookup = || {
            vec![source(
                "PR #514 body (associated with HEAD 43e61fc8)",
                "## Summary\n\nContract decision: introduce\n",
            )]
        };
        let (_, verdict) = decide(commits, Some(&lookup));
        assert_eq!(
            verdict,
            Verdict::Declared {
                decision: Decision::Introduce,
                source: "PR #514 body (associated with HEAD 43e61fc8)".to_string(),
            }
        );
    }

    /// Reading more sources must not weaken the check: nothing declared
    /// anywhere still fails, and the refusal names every place it read.
    #[test]
    fn no_decision_anywhere_still_fails_and_names_each_source() {
        let commits = vec![source("commit messages abc..HEAD", "fix: thing\n")];
        let lookup = || vec![source("PR #9 body (associated with HEAD abc)", "Summary\n")];
        let (sources, verdict) = decide(commits, Some(&lookup));
        assert_eq!(verdict, Verdict::Undeclared);
        let message = failure_message(&verdict, &sources);
        assert!(
            message.contains("commit messages abc..HEAD: no contract decision"),
            "{message}"
        );
        assert!(
            message.contains("PR #9 body (associated with HEAD abc): no contract decision"),
            "{message}"
        );
    }

    /// Offline, unauthenticated, or not on GitHub: the lookup cannot see the
    /// PR. That is a failure with its cause, never a pass by default.
    #[test]
    fn an_unavailable_lookup_fails_with_its_cause() {
        let commits = vec![source("commit messages abc..HEAD", "fix: thing\n")];
        let lookup = || {
            vec![DecisionSource {
                label: "pull requests associated with HEAD abc".to_string(),
                text: Err("could not run `gh` (No such file or directory)".to_string()),
            }]
        };
        let (sources, verdict) = decide(commits, Some(&lookup));
        assert_eq!(verdict, Verdict::Undeclared);
        let message = failure_message(&verdict, &sources);
        assert!(
            message.contains(
                "pull requests associated with HEAD abc: unavailable — could not run `gh`"
            ),
            "{message}"
        );
    }

    /// A broker submission whose commits declare the decision never reaches
    /// the network.
    #[test]
    fn the_lookup_runs_only_when_the_other_sources_do_not_pass() {
        let called = std::cell::Cell::new(false);
        let lookup = || {
            called.set(true);
            Vec::new()
        };
        let commits = vec![source(
            "commit messages",
            "Contract decision: soft-retire\n",
        )];
        let (_, verdict) = decide(commits, Some(&lookup));
        assert!(matches!(verdict, Verdict::Declared { .. }));
        assert!(!called.get());
    }

    /// An unjustified `none` in the commits is not final: the PR body may
    /// carry a more restrictive label, as it would if both were one text.
    #[test]
    fn an_unjustified_none_still_consults_the_pull_request() {
        let commits = vec![source("commit messages", "Contract decision: none\n")];
        let lookup = || vec![source("PR #3 body", "- [x] **hard-delete**\n")];
        let (_, verdict) = decide(commits, Some(&lookup));
        assert!(matches!(
            verdict,
            Verdict::Declared {
                decision: Decision::HardDelete,
                ..
            }
        ));
    }

    #[test]
    fn associated_pull_requests_reads_numbers_and_bodies() {
        let json = r#"[{"number":514,"body":"Contract decision: introduce"},
                       {"number":515,"body":null}]"#;
        assert_eq!(
            associated_pull_requests(json).unwrap(),
            vec![
                (514, "Contract decision: introduce".to_string()),
                (515, String::new())
            ]
        );
        assert!(associated_pull_requests(r#"{"message":"Not Found"}"#).is_err());
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
