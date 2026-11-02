//! Automation outcome issues keyed by fingerprint (#1238, first primitive).
//!
//! Work nobody watches -- a prebuild, a budget that keeps being missed --
//! needs somewhere durable to say "this is failing". `broker advanced issues
//! upsert` gives an automation one issue per fingerprint:
//!
//! - the first failure opens an issue whose body carries a hidden marker,
//!   `<!-- aethyme-outcome:<fingerprint> -->`;
//! - a repeat while it is open adds one bounded comment, at most once per
//!   `issue_comment_interval_minutes`;
//! - `--resolve` comments and closes the open issue, and does nothing when
//!   none is open.
//!
//! **Policy.** Nothing is written unless the repository opts in on its
//! default branch:
//!
//! ```toml
//! [automation]
//! issues = "allow"                 # default "deny"
//! issue_labels = ["ci:local"]      # the only labels an upsert may set
//! issues_per_day = 10              # opens + comments per UTC day
//! issue_comment_interval_minutes = 360
//! ```
//!
//! A denied, unlabelled or over-budget request writes nothing to GitHub; the
//! refusal is recorded locally and returned as a structured outcome.
//!
//! **Journal.** Every write is preceded by a durable pending entry in
//! `.aethyme/run/automation-issues.json`, taken under an exclusive lock. An
//! interrupted create is never retried blindly: the next upsert looks for the
//! marker first and adopts the issue it finds. Writes made with a session go
//! through the coordinated GitHub operation layer as well, which journals and
//! blocks on an ambiguous outcome; without a session the broker runs `gh`
//! itself, under this journal and the repository policy.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::text_redaction::redact_secrets;

/// The configuration table that holds the policy, as printed in refusals.
pub const AUTOMATION_ISSUES_POLICY_KEY: &str = "automation.issues";
pub const OUTCOME_ISSUE_SCHEMA: &str = "aethyme.outcome-issue/v1";
const STATE_FILE: &str = "automation-issues.json";
const LOCK_FILE: &str = "automation-issues.lock";
const STATE_SCHEMA: u32 = 1;

pub const DEFAULT_ISSUES_PER_DAY: u32 = 10;
pub const DEFAULT_COMMENT_INTERVAL_MINUTES: u64 = 360;
pub const TITLE_MAX_CHARS: usize = 200;
pub const BODY_MAX_BYTES: usize = 16 * 1024;
pub const COMMENT_MAX_BYTES: usize = 4 * 1024;
pub const FINGERPRINT_MAX_CHARS: usize = 128;
/// How long an interrupted create waits for search to show its issue before
/// the journal gives up on it and allows a new one.
pub const PENDING_CREATE_GRACE_MS: i64 = 15 * 60 * 1000;
/// Local refusals kept for `doctor`-style inspection.
const REFUSALS_KEPT: usize = 20;

/// The hidden marker that makes an issue the one for `fingerprint`.
pub fn outcome_marker(fingerprint: &str) -> String {
    format!("<!-- aethyme-outcome:{fingerprint} -->")
}

/// `[automation]` on the default branch. Absent or unreadable denies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationIssuesPolicy {
    pub allow: bool,
    pub labels: Vec<String>,
    pub per_day: u32,
    pub comment_interval_minutes: u64,
    /// `[[automation.outcomes]]` rules (#1238).
    pub outcomes: Vec<OutcomeRule>,
}

impl Default for AutomationIssuesPolicy {
    fn default() -> Self {
        Self {
            allow: false,
            labels: Vec::new(),
            per_day: DEFAULT_ISSUES_PER_DAY,
            comment_interval_minutes: DEFAULT_COMMENT_INTERVAL_MINUTES,
            outcomes: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomationTable {
    issues: Option<String>,
    #[serde(default)]
    issue_labels: Vec<String>,
    issues_per_day: Option<u32>,
    issue_comment_interval_minutes: Option<u64>,
    #[serde(default)]
    outcomes: Vec<OutcomeRuleTable>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeRuleTable {
    source: String,
    issue: Option<bool>,
    #[serde(default)]
    labels: Vec<String>,
    notify_owners: Option<bool>,
    #[serde(default)]
    paths: Vec<String>,
}

/// One `[[automation.outcomes]]` rule (#1238): what an automation's failure
/// and recovery set in motion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeRule {
    /// `<kind>:<name>`, for example `prebuild:db-template`.
    pub source: String,
    /// Keep one issue for the outcome (default `true`).
    pub issue: bool,
    pub labels: Vec<String>,
    /// Send a note to the live sessions behind the reported commits
    /// (default `false`).
    pub notify_owners: bool,
    /// Narrow the owners to commits touching these globs.
    pub paths: Vec<String>,
}

/// `<kind>:<name>`, at most 64 characters, each part lowercase ASCII
/// letters, digits, `.`, `_` or `-`. The bound keeps the note sender
/// `automation:<kind>.<name>` within its own limit.
pub fn valid_outcome_source(source: &str) -> bool {
    let part = |text: &str| {
        (1..=64).contains(&text.len())
            && text.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
    };
    source.len() <= 64
        && source
            .split_once(':')
            .is_some_and(|(kind, name)| part(kind) && part(name))
}

fn outcome_rules(tables: Vec<OutcomeRuleTable>) -> Result<Vec<OutcomeRule>, String> {
    let mut rules: Vec<OutcomeRule> = Vec::with_capacity(tables.len());
    for table in tables {
        if !valid_outcome_source(&table.source) {
            return Err(format!(
                "[[automation.outcomes]] source = {:?} is invalid; expected <kind>:<name> in lowercase letters, digits, '.', '_' or '-'",
                table.source
            ));
        }
        if rules.iter().any(|rule| rule.source == table.source) {
            return Err(format!(
                "[[automation.outcomes]] source = {:?} is declared twice",
                table.source
            ));
        }
        if let Some(path) = table
            .paths
            .iter()
            .find(|path| path.trim().is_empty() || path.starts_with('/'))
        {
            return Err(format!(
                "[[automation.outcomes]] {}: path {path:?} must be a non-empty repository-relative glob",
                table.source
            ));
        }
        rules.push(OutcomeRule {
            source: table.source,
            issue: table.issue.unwrap_or(true),
            labels: table.labels,
            notify_owners: table.notify_owners.unwrap_or(false),
            paths: table.paths,
        });
    }
    Ok(rules)
}

impl AutomationIssuesPolicy {
    /// Parse `.aethyme/config.toml` text. No `[automation]` table denies; an
    /// invalid one is an error, so a typo never silently grants or denies.
    pub fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(table) = value.get("automation") else {
            return Ok(Self::default());
        };
        let table: AutomationTable = table
            .clone()
            .try_into()
            .map_err(|error| format!("[automation] policy is invalid: {error}"))?;
        let allow = match table.issues.as_deref() {
            None | Some("deny") => false,
            Some("allow") => true,
            Some(other) => {
                return Err(format!(
                    "[automation] issues = {other:?} is invalid; expected \"allow\" or \"deny\""
                ));
            }
        };
        Ok(Self {
            allow,
            labels: table.issue_labels,
            per_day: table.issues_per_day.unwrap_or(DEFAULT_ISSUES_PER_DAY),
            comment_interval_minutes: table
                .issue_comment_interval_minutes
                .unwrap_or(DEFAULT_COMMENT_INTERVAL_MINUTES),
            outcomes: outcome_rules(table.outcomes)?,
        })
    }

    /// The trusted policy: `.aethyme/config.toml` at the default branch's
    /// remote-tracking commit, never a worktree an agent could edit. No
    /// tracked default branch means deny.
    pub fn load_trusted(repo: &crate::GitRepo) -> Result<Self, String> {
        let Some(default) = crate::session_push::tracked_default(repo) else {
            return Ok(Self::default());
        };
        match repo
            .config_toml_at_commit(&default.commit, "config")
            .map_err(|error| error.to_string())?
        {
            Some(text) => Self::from_config_text(&text),
            None => Ok(Self::default()),
        }
    }
}

/// One upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeIssueRequest {
    /// `owner/name`.
    pub repository: String,
    pub fingerprint: String,
    pub title: String,
    /// Markdown; redacted and bounded before it is written. Optional only
    /// with `resolve`.
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub resolve: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeIssueAction {
    Created,
    Commented,
    RateLimited,
    Resolved,
    NothingToResolve,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeIssueResult {
    pub schema: &'static str,
    pub repository: String,
    pub fingerprint: String,
    pub action: OutcomeIssueAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    /// Set with `refused`: `automation_issues_denied`, `label_not_allowed`,
    /// `daily_budget_exhausted` or `reconciliation_pending`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    pub message: String,
    /// Coordinated operation ids of the writes, when made with a session.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operation_ids: Vec<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum OutcomeIssueError {
    #[error("invalid fingerprint {0:?}: use 1-128 of A-Z a-z 0-9 . _ : / -")]
    InvalidFingerprint(String),
    #[error("invalid repository {0:?}: expected owner/name")]
    InvalidRepository(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    Policy(String),
    #[error("GitHub: {0}")]
    Gh(String),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl OutcomeIssueError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidFingerprint(_) => "invalid_fingerprint",
            Self::InvalidRepository(_) => "invalid_repository",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Policy(_) => "invalid_automation_policy",
            Self::Gh(_) => "github_failed",
            Self::Io { .. } => "io",
        }
    }
}

/// An issue as GitHub reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub number: u64,
    pub url: String,
    pub open: bool,
    pub body: String,
}

/// The GitHub calls an upsert needs. Reads may run directly; writes return
/// the coordinated operation id when one journaled them.
pub trait OutcomeIssueGh {
    /// One issue by number, `None` when it does not exist.
    fn view(&mut self, repository: &str, number: u64) -> Result<Option<IssueRef>, String>;
    /// Open issues whose body may contain `marker` (search is fuzzy; the
    /// caller checks the exact marker).
    fn search_open(&mut self, repository: &str, marker: &str) -> Result<Vec<IssueRef>, String>;
    fn create(
        &mut self,
        repository: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<(IssueRef, Option<i64>), String>;
    fn comment(&mut self, repository: &str, number: u64, body: &str)
    -> Result<Option<i64>, String>;
    fn close(&mut self, repository: &str, number: u64) -> Result<Option<i64>, String>;
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
struct State {
    schema: u32,
    #[serde(default)]
    fingerprints: BTreeMap<String, FingerprintState>,
    /// Opens and comments per UTC day (`YYYY-MM-DD`); only today is kept.
    #[serde(default)]
    daily: BTreeMap<String, u32>,
    #[serde(default)]
    refusals: Vec<Refusal>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
struct FingerprintState {
    repository: String,
    issue_number: Option<u64>,
    issue_url: Option<String>,
    last_comment_ms: Option<i64>,
    /// A write that started and has not been confirmed.
    pending: Option<Pending>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Pending {
    action: String,
    at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Refusal {
    at_ms: i64,
    repository: String,
    fingerprint: String,
    code: String,
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    !fingerprint.is_empty()
        && fingerprint.chars().count() <= FINGERPRINT_MAX_CHARS
        && fingerprint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
}

fn valid_repository(repository: &str) -> bool {
    let mut parts = repository.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    ok(owner) && ok(name)
}

/// Cut `text` to at most `max` bytes on a character boundary.
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub("\n\n[truncated]".len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[truncated]", &text[..end])
}

/// Redact and bound free text, and neutralize any marker it carries so a
/// body can never claim another fingerprint.
fn clean(text: &str, max: usize) -> String {
    let redacted = redact_secrets(text).replace("<!--", "&lt;!--");
    bounded(&redacted, max)
}

pub fn issue_title(title: &str) -> String {
    let single_line = redact_secrets(title).replace(['\n', '\r'], " ");
    single_line.trim().chars().take(TITLE_MAX_CHARS).collect()
}

pub fn issue_body(fingerprint: &str, body: &str) -> String {
    format!(
        "{}\n\n---\n_Opened by `aethyme broker advanced issues upsert` for outcome `{fingerprint}`. \
         It is updated while the outcome keeps failing and closed when it recovers._\n\n{}\n",
        clean(body, BODY_MAX_BYTES),
        outcome_marker(fingerprint)
    )
}

fn repeat_comment(body: &str) -> String {
    format!("Still failing.\n\n{}", clean(body, COMMENT_MAX_BYTES))
}

fn resolve_comment(body: Option<&str>) -> String {
    match body {
        Some(body) if !body.trim().is_empty() => {
            format!("Recovered; closing.\n\n{}", clean(body, COMMENT_MAX_BYTES))
        }
        _ => "Recovered; closing.".to_string(),
    }
}

/// `YYYY-MM-DD` (UTC) of a millisecond timestamp.
fn utc_day(now_ms: i64) -> String {
    let days = now_ms.div_euclid(86_400_000);
    // Civil-from-days (Howard Hinnant), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

struct Journal {
    path: PathBuf,
    state: State,
}

impl Journal {
    fn load(run_dir: &Path) -> Result<Self, OutcomeIssueError> {
        let path = run_dir.join(STATE_FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| OutcomeIssueError::Io {
                action: "parse",
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State {
                schema: STATE_SCHEMA,
                ..State::default()
            },
            Err(source) => {
                return Err(OutcomeIssueError::Io {
                    action: "read",
                    path,
                    source,
                });
            }
        };
        Ok(Self { path, state })
    }

    fn save(&self) -> Result<(), OutcomeIssueError> {
        let bytes =
            serde_json::to_vec_pretty(&self.state).map_err(|error| OutcomeIssueError::Io {
                action: "serialize",
                path: self.path.clone(),
                source: std::io::Error::other(error),
            })?;
        crate::atomic_file::with_synced_temporary(&self.path, &bytes, |temporary| {
            std::fs::rename(temporary, &self.path)
        })
        .map_err(|source| OutcomeIssueError::Io {
            action: "write",
            path: self.path.clone(),
            source,
        })
    }

    fn entry(&mut self, request: &OutcomeIssueRequest) -> &mut FingerprintState {
        let key = format!("{}#{}", request.repository, request.fingerprint);
        self.state
            .fingerprints
            .entry(key)
            .or_insert_with(|| FingerprintState {
                repository: request.repository.clone(),
                ..FingerprintState::default()
            })
    }

    fn writes_today(&self, now_ms: i64) -> u32 {
        self.state.daily.get(&utc_day(now_ms)).copied().unwrap_or(0)
    }

    fn count_write(&mut self, now_ms: i64) {
        let today = utc_day(now_ms);
        self.state.daily.retain(|day, _| *day == today);
        *self.state.daily.entry(today).or_insert(0) += 1;
    }

    fn refuse(&mut self, request: &OutcomeIssueRequest, code: &str, now_ms: i64) {
        self.state.refusals.push(Refusal {
            at_ms: now_ms,
            repository: request.repository.clone(),
            fingerprint: request.fingerprint.clone(),
            code: code.into(),
        });
        let excess = self.state.refusals.len().saturating_sub(REFUSALS_KEPT);
        self.state.refusals.drain(..excess);
    }
}

fn result(
    request: &OutcomeIssueRequest,
    action: OutcomeIssueAction,
    issue: Option<(u64, String)>,
    code: Option<&'static str>,
    message: String,
    operation_ids: Vec<i64>,
) -> OutcomeIssueResult {
    let (issue_number, issue_url) = match issue {
        Some((number, url)) => (Some(number), Some(url)),
        None => (None, None),
    };
    OutcomeIssueResult {
        schema: OUTCOME_ISSUE_SCHEMA,
        repository: request.repository.clone(),
        fingerprint: request.fingerprint.clone(),
        action,
        issue_number,
        issue_url,
        code,
        message,
        operation_ids,
    }
}

/// Find the open issue for this fingerprint: the journaled number first
/// (search indexes lag a fresh issue), then a marker search. Only an issue
/// whose body holds the exact marker counts.
fn find_open(
    gh: &mut dyn OutcomeIssueGh,
    request: &OutcomeIssueRequest,
    known: Option<u64>,
) -> Result<Option<IssueRef>, OutcomeIssueError> {
    let marker = outcome_marker(&request.fingerprint);
    if let Some(number) = known
        && let Some(issue) = gh
            .view(&request.repository, number)
            .map_err(OutcomeIssueError::Gh)?
        && issue.open
        && issue.body.contains(&marker)
    {
        return Ok(Some(issue));
    }
    let found = gh
        .search_open(&request.repository, &marker)
        .map_err(OutcomeIssueError::Gh)?;
    Ok(found
        .into_iter()
        .filter(|issue| issue.open && issue.body.contains(&marker))
        .min_by_key(|issue| issue.number))
}

/// Open, update or resolve the issue for one outcome fingerprint.
///
/// `run_dir` holds the journal and its lock (normally the primary
/// repository's `.aethyme/run`). The caller supplies the trusted policy and
/// the GitHub runner; [`upsert_outcome_issue_for_repository`] wires both for
/// a repository checkout.
pub fn upsert_outcome_issue(
    run_dir: &Path,
    policy: &AutomationIssuesPolicy,
    gh: &mut dyn OutcomeIssueGh,
    request: &OutcomeIssueRequest,
    now_ms: i64,
) -> Result<OutcomeIssueResult, OutcomeIssueError> {
    if !valid_fingerprint(&request.fingerprint) {
        return Err(OutcomeIssueError::InvalidFingerprint(
            request.fingerprint.clone(),
        ));
    }
    if !valid_repository(&request.repository) {
        return Err(OutcomeIssueError::InvalidRepository(
            request.repository.clone(),
        ));
    }
    if !request.resolve && request.body.as_deref().is_none_or(|b| b.trim().is_empty()) {
        return Err(OutcomeIssueError::InvalidRequest(
            "an upsert needs a non-empty body (--body-file); only --resolve may omit it".into(),
        ));
    }
    if !request.resolve && issue_title(&request.title).is_empty() {
        return Err(OutcomeIssueError::InvalidRequest(
            "an upsert needs a non-empty --title".into(),
        ));
    }
    std::fs::create_dir_all(run_dir).map_err(|source| OutcomeIssueError::Io {
        action: "create",
        path: run_dir.to_path_buf(),
        source,
    })?;
    let lock_path = run_dir.join(LOCK_FILE);
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|source| OutcomeIssueError::Io {
            action: "open",
            path: lock_path.clone(),
            source,
        })?;
    let _lock = crate::file_lock::ExclusiveFileLock::acquire(lock_file).map_err(|source| {
        OutcomeIssueError::Io {
            action: "lock",
            path: lock_path,
            source,
        }
    })?;
    let mut journal = Journal::load(run_dir)?;

    let refuse = |journal: &mut Journal, code: &'static str, message: String| {
        journal.refuse(request, code, now_ms);
        journal.save()?;
        Ok::<_, OutcomeIssueError>(result(
            request,
            OutcomeIssueAction::Refused,
            None,
            Some(code),
            message,
            Vec::new(),
        ))
    };

    if !policy.allow {
        return refuse(
            &mut journal,
            "automation_issues_denied",
            format!(
                "repository policy {AUTOMATION_ISSUES_POLICY_KEY} is not \"allow\" on the default \
                 branch; nothing was written to GitHub. Set `issues = \"allow\"` under \
                 `[automation]` in .aethyme/config.toml to opt in"
            ),
        );
    }
    if let Some(label) = request
        .labels
        .iter()
        .find(|label| !policy.labels.contains(label))
    {
        return refuse(
            &mut journal,
            "label_not_allowed",
            format!(
                "label {label:?} is not in [automation] issue_labels; nothing was written to GitHub"
            ),
        );
    }

    let known = journal.entry(request).issue_number;
    // An interrupted create: adopt the issue it made, or wait out the search
    // index before allowing another. Never assume it failed.
    if let Some(pending) = journal.entry(request).pending.clone()
        && pending.action == "create"
    {
        match find_open(gh, request, known)? {
            Some(issue) => {
                let entry = journal.entry(request);
                entry.issue_number = Some(issue.number);
                entry.issue_url = Some(issue.url);
                entry.pending = None;
                journal.save()?;
            }
            None if now_ms - pending.at_ms < PENDING_CREATE_GRACE_MS => {
                return refuse(
                    &mut journal,
                    "reconciliation_pending",
                    "an earlier create for this fingerprint has an unknown outcome and its issue \
                     is not visible yet; retry after the search index catches up"
                        .into(),
                );
            }
            None => {
                journal.entry(request).pending = None;
                journal.save()?;
            }
        }
    }

    let known = journal.entry(request).issue_number;
    let open = find_open(gh, request, known)?;

    if request.resolve {
        let Some(issue) = open else {
            let entry = journal.entry(request);
            entry.issue_number = None;
            entry.issue_url = None;
            journal.save()?;
            return Ok(result(
                request,
                OutcomeIssueAction::NothingToResolve,
                None,
                None,
                "no open issue for this fingerprint; nothing to resolve".into(),
                Vec::new(),
            ));
        };
        journal.entry(request).pending = Some(Pending {
            action: "resolve".into(),
            at_ms: now_ms,
        });
        journal.save()?;
        let mut operations = Vec::new();
        operations.extend(
            gh.comment(
                &request.repository,
                issue.number,
                &resolve_comment(request.body.as_deref()),
            )
            .map_err(OutcomeIssueError::Gh)?,
        );
        operations.extend(
            gh.close(&request.repository, issue.number)
                .map_err(OutcomeIssueError::Gh)?,
        );
        let entry = journal.entry(request);
        entry.pending = None;
        entry.issue_number = None;
        entry.issue_url = None;
        entry.last_comment_ms = None;
        journal.save()?;
        return Ok(result(
            request,
            OutcomeIssueAction::Resolved,
            Some((issue.number, issue.url)),
            None,
            format!("closed #{}", issue.number),
            operations,
        ));
    }

    let body = request.body.as_deref().unwrap_or_default();
    if let Some(issue) = open {
        let interval_ms = policy.comment_interval_minutes as i64 * 60_000;
        let entry = journal.entry(request);
        entry.issue_number = Some(issue.number);
        entry.issue_url = Some(issue.url.clone());
        if entry
            .last_comment_ms
            .is_some_and(|last| now_ms - last < interval_ms)
        {
            journal.save()?;
            return Ok(result(
                request,
                OutcomeIssueAction::RateLimited,
                Some((issue.number, issue.url)),
                None,
                format!(
                    "#{} is open and was updated within {} minutes; not commenting again",
                    issue.number, policy.comment_interval_minutes
                ),
                Vec::new(),
            ));
        }
        if journal.writes_today(now_ms) >= policy.per_day {
            return refuse(
                &mut journal,
                "daily_budget_exhausted",
                format!(
                    "[automation] issues_per_day ({}) is spent for today; nothing was written",
                    policy.per_day
                ),
            );
        }
        // A comment that may or may not have landed is harmless to skip: the
        // next one after the interval carries the same news.
        let entry = journal.entry(request);
        entry.last_comment_ms = Some(now_ms);
        journal.count_write(now_ms);
        journal.save()?;
        let operation = gh
            .comment(&request.repository, issue.number, &repeat_comment(body))
            .map_err(OutcomeIssueError::Gh)?;
        return Ok(result(
            request,
            OutcomeIssueAction::Commented,
            Some((issue.number, issue.url)),
            None,
            format!("commented on #{}", issue.number),
            operation.into_iter().collect(),
        ));
    }

    if journal.writes_today(now_ms) >= policy.per_day {
        return refuse(
            &mut journal,
            "daily_budget_exhausted",
            format!(
                "[automation] issues_per_day ({}) is spent for today; nothing was written",
                policy.per_day
            ),
        );
    }
    journal.entry(request).pending = Some(Pending {
        action: "create".into(),
        at_ms: now_ms,
    });
    journal.count_write(now_ms);
    journal.save()?;
    let (issue, operation) = gh
        .create(
            &request.repository,
            &issue_title(&request.title),
            &issue_body(&request.fingerprint, body),
            &request.labels,
        )
        .map_err(OutcomeIssueError::Gh)?;
    let entry = journal.entry(request);
    entry.pending = None;
    entry.issue_number = Some(issue.number);
    entry.issue_url = Some(issue.url.clone());
    entry.last_comment_ms = Some(now_ms);
    journal.save()?;
    Ok(result(
        request,
        OutcomeIssueAction::Created,
        Some((issue.number, issue.url)),
        None,
        format!("opened #{}", issue.number),
        operation.into_iter().collect(),
    ))
}

/// Run `gh` and return stdout, or the trimmed stderr as the error.
fn gh_read(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("gh")
        .args(args)
        .output()
        .map_err(|error| format!("cannot run gh: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_issue(value: &serde_json::Value) -> Option<IssueRef> {
    Some(IssueRef {
        number: value.get("number")?.as_u64()?,
        url: value.get("url")?.as_str()?.to_string(),
        open: value.get("state")?.as_str()?.eq_ignore_ascii_case("open"),
        body: value
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The issue number at the end of a `gh issue create` URL.
pub fn issue_number_from_url(stdout: &str) -> Option<(u64, String)> {
    let url = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("https://"))?;
    let number = url.rsplit('/').next()?.parse().ok()?;
    Some((number, url.to_string()))
}

/// Reads through `gh` directly; writes through `gh` directly (sessionless)
/// or through the coordinated operation layer when a session is given.
pub struct SystemGh<'a> {
    pub coordinated: Option<(&'a mut crate::Broker, i64)>,
}

impl SystemGh<'_> {
    fn write(
        &mut self,
        scope: String,
        reason: String,
        args: Vec<String>,
    ) -> Result<(String, Option<i64>), String> {
        let Some((broker, session_id)) = self.coordinated.as_mut() else {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            return gh_read(&refs).map(|stdout| (stdout, None));
        };
        let report = broker
            .run_coordinated_operation(crate::CoordinatedCommand {
                session_id: *session_id,
                provider: crate::OperationProvider::Github,
                repository: args
                    .iter()
                    .position(|a| a == "--repo")
                    .and_then(|i| args.get(i + 1))
                    .cloned(),
                resolved_target: None,
                scope: Some(scope),
                declared_effect: Some(crate::OperationEffect::Write),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(reason),
                args,
            })
            .map_err(|error| error.to_string())?;
        if !report.command_success {
            return Err(format!(
                "coordinated operation {} failed: {}",
                report.operation.id,
                report.stderr.trim()
            ));
        }
        Ok((report.stdout, Some(report.operation.id)))
    }
}

impl OutcomeIssueGh for SystemGh<'_> {
    fn view(&mut self, repository: &str, number: u64) -> Result<Option<IssueRef>, String> {
        let number_text = number.to_string();
        match gh_read(&[
            "issue",
            "view",
            &number_text,
            "--repo",
            repository,
            "--json",
            "number,url,state,body",
        ]) {
            Ok(stdout) => {
                let value: serde_json::Value =
                    serde_json::from_str(&stdout).map_err(|e| e.to_string())?;
                Ok(parse_issue(&value).filter(|issue| issue.number == number))
            }
            Err(error) if error.contains("Could not resolve") || error.contains("not found") => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn search_open(&mut self, repository: &str, marker: &str) -> Result<Vec<IssueRef>, String> {
        let needle = marker.trim_start_matches("<!-- ").trim_end_matches(" -->");
        let query = format!("\"{needle}\" in:body");
        let stdout = gh_read(&[
            "issue",
            "list",
            "--repo",
            repository,
            "--state",
            "open",
            "--search",
            &query,
            "--json",
            "number,url,state,body",
            "--limit",
            "20",
        ])?;
        let values: Vec<serde_json::Value> =
            serde_json::from_str(&stdout).map_err(|e| e.to_string())?;
        Ok(values.iter().filter_map(parse_issue).collect())
    }

    fn create(
        &mut self,
        repository: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<(IssueRef, Option<i64>), String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut file, body.as_bytes()).map_err(|e| e.to_string())?;
        let mut args: Vec<String> = [
            "issue",
            "create",
            "--repo",
            repository,
            "--title",
            title,
            "--body-file",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(file.path().to_string_lossy().into_owned());
        for label in labels {
            args.push("--label".into());
            args.push(label.clone());
        }
        let (stdout, operation) = self.write(
            "issues/outcome/create".into(),
            "automation outcome issue authorized by [automation] issues = \"allow\"".into(),
            args,
        )?;
        let (number, url) = issue_number_from_url(&stdout)
            .ok_or_else(|| format!("gh issue create printed no issue URL: {}", stdout.trim()))?;
        Ok((
            IssueRef {
                number,
                url,
                open: true,
                body: body.to_string(),
            },
            operation,
        ))
    }

    fn comment(
        &mut self,
        repository: &str,
        number: u64,
        body: &str,
    ) -> Result<Option<i64>, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut file, body.as_bytes()).map_err(|e| e.to_string())?;
        let args = vec![
            "issue".into(),
            "comment".into(),
            number.to_string(),
            "--repo".into(),
            repository.into(),
            "--body-file".into(),
            file.path().to_string_lossy().into_owned(),
        ];
        self.write(
            format!("issues/{number}/outcome/comment"),
            "automation outcome issue authorized by [automation] issues = \"allow\"".into(),
            args,
        )
        .map(|(_, operation)| operation)
    }

    fn close(&mut self, repository: &str, number: u64) -> Result<Option<i64>, String> {
        let args = vec![
            "issue".into(),
            "close".into(),
            number.to_string(),
            "--repo".into(),
            repository.into(),
            "--reason".into(),
            "completed".into(),
        ];
        self.write(
            format!("issues/{number}/outcome/close"),
            "automation outcome recovered; [automation] issues = \"allow\"".into(),
            args,
        )
        .map(|(_, operation)| operation)
    }
}

/// A refusal kept in the local journal, newest last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeIssueRefusal {
    pub at_ms: i64,
    pub repository: String,
    pub fingerprint: String,
    pub code: String,
}

/// The refusals the journal under `main_root/.aethyme/run` still keeps (the
/// most recent [`REFUSALS_KEPT`]), for `doctor`. A missing journal is none.
pub fn recent_outcome_issue_refusals(
    main_root: &Path,
) -> Result<Vec<OutcomeIssueRefusal>, OutcomeIssueError> {
    let run_dir = main_root.join(".aethyme").join("run");
    if !run_dir.join(STATE_FILE).exists() {
        return Ok(Vec::new());
    }
    let journal = Journal::load(&run_dir)?;
    Ok(journal
        .state
        .refusals
        .into_iter()
        .map(|refusal| OutcomeIssueRefusal {
            at_ms: refusal.at_ms,
            repository: refusal.repository,
            fingerprint: refusal.fingerprint,
            code: refusal.code,
        })
        .collect())
}

/// [`upsert_outcome_issue`] for a repository checkout: the trusted policy
/// from its default branch, the journal under its primary root's
/// `.aethyme/run`, and real `gh`. With `coordinated`, writes go through the
/// coordinated operation layer under that session.
pub fn upsert_outcome_issue_for_repository(
    repo: &crate::GitRepo,
    request: &OutcomeIssueRequest,
    coordinated: Option<(&mut crate::Broker, i64)>,
) -> Result<OutcomeIssueResult, OutcomeIssueError> {
    let policy = AutomationIssuesPolicy::load_trusted(repo).map_err(OutcomeIssueError::Policy)?;
    let main_root = repo.main_root().map_err(|error| OutcomeIssueError::Io {
        action: "resolve",
        path: repo.root().to_path_buf(),
        source: std::io::Error::other(error.to_string()),
    })?;
    let run_dir = crate::file_lock::repository_run_directory(&main_root).map_err(|source| {
        OutcomeIssueError::Io {
            action: "prepare",
            path: main_root.join(".aethyme/run"),
            source,
        }
    })?;
    let mut gh = SystemGh { coordinated };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    upsert_outcome_issue(&run_dir, &policy, &mut gh, request, now_ms)
}

#[cfg(test)]
#[path = "outcome_issues_tests.rs"]
mod tests;
