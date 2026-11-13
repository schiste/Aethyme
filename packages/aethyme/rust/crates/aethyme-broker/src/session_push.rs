//! `broker push`: publish a session's own branch while the work is under way.
//!
//! Work that exists only in a worktree is lost with it. Measured 2026-09-30:
//! 94 broker worktrees held commits or edits found on no remote, most of them
//! in closed sessions, and one repository's local integration branch was 74
//! unpublished commits ahead of its remote. Submission promotes locally and
//! publication was a separate act nobody was authorized to take, so work piled
//! up where only one disk held it.
//!
//! This lane makes the cheap, safe publication routine. It pushes exactly one
//! ref -- the session's own `agent/*` branch -- and nothing that another
//! session or the default branch depends on. Authorization is durable and
//! repository-level (`[delivery] push_session_branches = true` on the default
//! branch), so an agent can push after every commit without asking each time.
//! A new PR's mode comes from the trusted broker.pr open_as policy on that same branch;
//! an existing PR is never changed.

use std::path::Path;

use crate::{
    Broker, BrokerOpError, CoordinatedCommand, CoordinatedOperationReport, GitRepo,
    OperationEffect, OperationProvider, QueueWait, RepositoryDeliveryConfig,
    UnknownOutcomeRecovery,
};

/// The configuration key that authorizes `broker push`, as printed in refusals.
pub const SESSION_PUSH_POLICY_KEY: &str = "delivery.push_session_branches";

/// How a newly created session pull request should be opened.
///
/// The value comes from `[broker.pr] open_as` on the fetched default branch.
/// Missing configuration preserves the historical draft behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPullRequestOpenAs {
    Draft,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionPullRequestPolicy {
    open_as: SessionPullRequestOpenAs,
    watch_on_start: bool,
}

impl Default for SessionPullRequestPolicy {
    fn default() -> Self {
        Self {
            open_as: SessionPullRequestOpenAs::Draft,
            watch_on_start: false,
        }
    }
}

impl SessionPullRequestPolicy {
    fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(broker) = value.get("broker") else {
            return Ok(Self::default());
        };
        let broker = broker
            .as_table()
            .ok_or_else(|| "[broker] policy must be a table".to_string())?;
        let Some(pr) = broker.get("pr") else {
            return Ok(Self::default());
        };
        let pr = pr
            .as_table()
            .ok_or_else(|| "[broker.pr] policy must be a table".to_string())?;
        if let Some(key) = pr
            .keys()
            .find(|key| !matches!(key.as_str(), "open_as" | "watch_on_start"))
        {
            return Err(format!("[broker.pr] has unsupported key {key:?}"));
        }
        let open_as = match pr.get("open_as") {
            None => SessionPullRequestOpenAs::Draft,
            Some(value) => match value.as_str() {
                Some("draft") => SessionPullRequestOpenAs::Draft,
                Some("ready") => SessionPullRequestOpenAs::Ready,
                Some(_) => return Err("[broker.pr] open_as must be \"draft\" or \"ready\"".into()),
                None => return Err("[broker.pr] open_as must be a string".into()),
            },
        };
        let watch_on_start = match pr.get("watch_on_start") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "[broker.pr] watch_on_start must be a boolean".to_string())?,
        };
        Ok(Self {
            open_as,
            watch_on_start,
        })
    }
}

/// Prefix every broker session branch carries. `broker push` publishes only
/// refs under it, so the default branch, `aethyme/integration` and tags are
/// out of reach by construction rather than by a list of exceptions.
const SESSION_BRANCH_PREFIX: &str = "agent/";

/// `meta` key recording the last oid this broker pushed for one session.
fn pushed_oid_key(session_id: i64) -> String {
    format!("session_push.{session_id}.oid")
}

/// `meta` key recording when the default branch was last fetched into
/// `tracking_ref` by [`Broker::fetch_default_branch`]: the start of the last
/// successful fetch in epoch milliseconds, or [`DEFAULT_BRANCH_FETCH_FAILED`]
/// after a failed one. The broker database is shared by every session and
/// command of the repository, so any of them can reuse the record.
pub(crate) fn default_branch_fetch_key(tracking_ref: &str) -> String {
    format!("default_branch_fetch.{tracking_ref}.started_at_ms")
}

/// The record a failed default-branch fetch leaves: it never parses as a time,
/// so nothing reuses a fetch the last attempt could not repeat.
const DEFAULT_BRANCH_FETCH_FAILED: &str = "failed";

/// How recent a default-branch fetch `push` reuses instead of fetching again.
const PUSH_FETCH_REUSE_WINDOW_MS: i64 = 60_000;

/// Set to `off` to make every `push` fetch the default branch itself.
pub(crate) const PUSH_FETCH_REUSE_ENV: &str = "AETHYME_PUSH_FETCH_REUSE";

/// Whether `push` may reuse a recent default-branch fetch; read once.
fn push_fetch_reuse_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var(PUSH_FETCH_REUSE_ENV),
            Ok(value) if value.trim().eq_ignore_ascii_case("off")
        )
    })
}

/// How old the recorded default-branch fetch is, when `push` may reuse it.
///
/// `None` without a record, after a failed fetch, for a record in the future
/// (a clock that stepped back proves nothing about freshness), and for one
/// older than `window_ms`.
fn reusable_fetch_age_ms(recorded: Option<&str>, now_ms: i64, window_ms: i64) -> Option<i64> {
    let started_at_ms = recorded?.trim().parse::<i64>().ok()?;
    let age_ms = now_ms.checked_sub(started_at_ms)?;
    (0..=window_ms).contains(&age_ms).then_some(age_ms)
}

/// Whole seconds between `listed_at_ms` and `now_ms`; zero for a clock that
/// stepped back.
fn listing_age_seconds(listed_at_ms: i64, now_ms: i64) -> u64 {
    u64::try_from(now_ms.saturating_sub(listed_at_ms) / 1000).unwrap_or(0)
}

/// Set to `off` to make every `push` fetch the default branch even when its
/// own remote read showed the default branch had not moved.
pub(crate) const PUSH_FETCH_SKIP_UNMOVED_ENV: &str = "AETHYME_PUSH_FETCH_SKIP_UNMOVED";

/// Whether `push` may skip the default-branch fetch on proof that the remote
/// default branch is where the tracking ref already is; read once.
fn push_fetch_skip_unmoved_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var(PUSH_FETCH_SKIP_UNMOVED_ENV),
            Ok(value) if value.trim().eq_ignore_ascii_case("off")
        )
    })
}

/// Whether a fetch from `remote` reaches the endpoint a push to it reads.
///
/// The push's remote read queries the push URL; the default-branch fetch
/// queries the fetch URL. Only when they are the same string (after Git's own
/// rewrites, before any redaction) does what the push read stand for what the
/// fetch would bring. A `pushurl` that differs, several push URLs, or a URL
/// Git cannot report all mean "not the same".
fn fetch_and_push_share_endpoint(repo: &GitRepo, remote: &str) -> bool {
    let Ok(fetch_url) = repo.remote_url(remote) else {
        return false;
    };
    matches!(
        repo.remote_push_urls(remote).as_deref(),
        Ok([push_url]) if *push_url == fetch_url
    )
}

/// What the push's own remote read says about the default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultBranchProof {
    /// The remote default branch was where the tracking ref already is, as
    /// read from the remote at this epoch-millisecond time.
    Unmoved { read_started_at_ms: i64 },
    /// The remote default branch was elsewhere (or absent): only a fetch can
    /// bring the comparison up to date.
    Moved,
    /// Nothing was proven either way; today's rules apply.
    Unavailable,
}

/// Compare what the push read on the remote with the local tracking ref.
fn default_branch_proof(
    observation: Option<&crate::operations::RemoteDefaultObservation>,
    remote_ref: &str,
    tracking_oid: Option<&str>,
) -> DefaultBranchProof {
    let Some(observation) = observation.filter(|seen| seen.remote_ref == remote_ref) else {
        return DefaultBranchProof::Unavailable;
    };
    match (observation.oid.as_deref(), tracking_oid) {
        (Some(remote), Some(local)) if remote.eq_ignore_ascii_case(local) => {
            DefaultBranchProof::Unmoved {
                read_started_at_ms: observation.read_started_at_ms,
            }
        }
        _ => DefaultBranchProof::Moved,
    }
}

/// The fetch record to keep after a proof that the tracking ref matched the
/// remote at `proven_at_ms`, or `None` to leave `recorded` as it is.
///
/// A fetch started at some time leaves the tracking ref at least as new as the
/// remote was then; the proof says the same of the read's start, so it counts
/// as a fetch started at that moment. A newer successful record is kept.
fn record_after_unmoved_proof(recorded: Option<&str>, proven_at_ms: i64) -> Option<String> {
    match recorded.and_then(|value| value.trim().parse::<i64>().ok()) {
        Some(started_at_ms) if started_at_ms >= proven_at_ms => None,
        _ => Some(proven_at_ms.to_string()),
    }
}

/// How long one push or GitHub call may queue for the repository write lock.
fn push_operation_wait() -> QueueWait {
    QueueWait::Seconds(60)
}

/// The total deadline for one coordinated default-branch refresh.
///
/// `QueueWait::Seconds` is shared across operation preparation, repository
/// lock admission and the Git child process; it is not a queue-only budget.
/// Thirty seconds leaves room for ordinary contention while keeping refreshes
/// bounded. A timeout with an unknown write outcome still blocks writes until
/// reconciliation; this budget never makes a cached ref safe to assume.
fn fetch_operation_wait() -> QueueWait {
    QueueWait::Seconds(30)
}

/// Where a session's branch stands relative to its remote, from local refs.
///
/// Read-only and never fetches, so it is as fresh as the last fetch or push.
/// `unpushed_commits` counts commits reachable from the branch that no
/// remote-tracking ref holds -- work that exists only on this machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPushState {
    pub branch: String,
    pub head_oid: String,
    pub remote_oid: Option<String>,
    pub unpushed_commits: u32,
    pub oldest_unpushed_at_ms: Option<i64>,
}

/// The pull request `broker push --pr` found or opened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPullRequest {
    pub url: String,
    pub number: i64,
    pub state: String,
    /// True when this invocation created it.
    pub created: bool,
    /// The repository's CI workflows skip draft pull requests, so checks run
    /// only once it is marked ready. Known only for a pull request this
    /// invocation opened.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ci_skips_drafts: bool,
    /// The pull request is a draft. A review run on push skips drafts.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
}

/// What `broker push` did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPushReport {
    pub session_id: i64,
    pub branch: String,
    pub remote: String,
    pub pushed_oid: String,
    /// The remote branch before this push, as this broker last knew it.
    pub previous_remote_oid: Option<String>,
    /// Commits the push made available that no remote held before.
    pub commits_pushed: u32,
    /// Uncommitted entries left in the worktree; only commits are pushed.
    /// `uncommitted.modified + uncommitted.untracked_entries`: an untracked
    /// directory counts once, as `git status` shows it.
    pub uncommitted_files: u32,
    pub uncommitted: UncommittedCounts,
    pub pr: Option<SessionPullRequest>,
    /// Established or existing watch for this session; absent when disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_watch_id: Option<i64>,
    /// Observation failure after a successful remote push.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_watch_note: Option<String>,
    /// Open pull requests whose change overlaps this session's: same files,
    /// and whether the changed lines overlap or nearly touch. Advisory only.
    pub pr_overlaps: Vec<crate::PrOverlap>,
    /// Open pull requests whose change could not be read, so their overlap
    /// is unknown rather than absent.
    pub pr_overlaps_unknown: Vec<i64>,
    /// True when the open-PR listing was unavailable or incomplete, so no
    /// overlap conclusion could be drawn for the listing as a whole.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pr_overlap_listing_unavailable: bool,
    /// Other sessions that look like the same work: the same branch, the same
    /// open PR, or a task naming the same PR. Omitted when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub duplicate_work: Vec<crate::DuplicateWork>,
    /// The pushed head compared with the default branch, fetched just before
    /// the comparison (or moments before by another broker command; see
    /// `default_branch_fetch_age_seconds`): how far behind it is and whether
    /// merging would conflict. `None` when Git could not answer. Advisory only.
    pub default_branch: Option<crate::DefaultBranchDrift>,
    /// Why `default_branch` is missing or may be stale, e.g. the fetch failed
    /// and the comparison used the last fetched copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch_note: Option<String>,
    /// Seconds since the default branch `default_branch` was measured
    /// against was last known current, when this push did not fetch: it
    /// reused a fetch another broker command made moments before, or (with
    /// `default_branch_fetch_skipped_unmoved`) its own remote read showed the
    /// default branch had not moved. Omitted when this push fetched itself.
    /// `AETHYME_PUSH_FETCH_REUSE=off` disables the reuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch_fetch_age_seconds: Option<u64>,
    /// Seconds since the open-PR listing `pr` was read from, when this push
    /// took its pull request from the listing its overlap check had just
    /// fetched instead of looking the branch up separately. Omitted when `pr`
    /// came from a lookup, from `--pr`, or is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_lookup_listing_age_seconds: Option<u64>,
    /// True when this push did not fetch the default branch because the
    /// remote read it makes before pushing showed the remote default branch
    /// at exactly the commit the tracking ref already held: a fetch would
    /// have changed nothing. `default_branch_fetch_age_seconds` then counts
    /// from that read. Omitted otherwise;
    /// `AETHYME_PUSH_FETCH_SKIP_UNMOVED=off` always fetches.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub default_branch_fetch_skipped_unmoved: bool,
    /// With `--pr`: the change measured against `[review] pr_size` (#239).
    /// Advisory only -- a push is never refused for its size. Omitted without
    /// `--pr`, or when the change could not be measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_size: Option<crate::PrSizeReport>,
    /// With `[review] run_on_push`: what the review run for `pr` did, or why
    /// it did not run or failed. The push itself succeeded either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_run: Option<serde_json::Value>,
    /// Internal phase timings for command telemetry.
    #[serde(skip)]
    pub phase_timings_ms: std::collections::BTreeMap<String, u64>,
    /// `owner/name` the push targeted; the review run on push needs it.
    #[serde(skip)]
    pub repository: String,
}

/// How many paths `broker push` left behind, and a few of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UncommittedCounts {
    /// Tracked paths with staged or unstaged changes.
    pub modified: u32,
    /// Untracked entries; an untracked directory is one entry.
    pub untracked_entries: u32,
    /// Up to [`UNCOMMITTED_SAMPLE`] paths, modified first.
    pub sample: Vec<String>,
}

/// How many uncommitted paths a push report names.
pub const UNCOMMITTED_SAMPLE: usize = 5;

impl From<&crate::UncommittedSummary> for UncommittedCounts {
    fn from(summary: &crate::UncommittedSummary) -> Self {
        let count = |paths: &Vec<String>| u32::try_from(paths.len()).unwrap_or(u32::MAX);
        Self {
            modified: count(&summary.modified),
            untracked_entries: count(&summary.untracked),
            sample: summary
                .modified
                .iter()
                .chain(&summary.untracked)
                .take(UNCOMMITTED_SAMPLE)
                .cloned()
                .collect(),
        }
    }
}

/// The remote and default branch the main checkout tracks, and the commit its
/// remote-tracking ref points at.
pub(crate) struct TrackedDefault {
    pub(crate) remote: String,
    pub(crate) branch: String,
    pub(crate) tracking_ref: String,
    pub(crate) commit: String,
}

/// The one default-branch ref inspected during a session-reuse boundary.
/// `fetched` is false when the comparison used a cached tracking ref after a
/// failed or unavailable fetch; callers must not treat that cache as current
/// remote evidence.
#[derive(Debug, Clone)]
pub(crate) struct DefaultBranchRefreshEvidence {
    pub(crate) tracking_ref: String,
    pub(crate) display_ref: String,
    pub(crate) commit: Option<String>,
    pub(crate) fetched: bool,
}

pub(crate) fn tracked_default(repo: &GitRepo) -> Option<TrackedDefault> {
    let (upstream, commit) = repo.upstream_default()?;
    let (remote, branch) = upstream
        .strip_prefix("refs/remotes/")
        .unwrap_or(&upstream)
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())?;
    Some(TrackedDefault {
        remote: remote.to_string(),
        branch: branch.to_string(),
        tracking_ref: format!("refs/remotes/{remote}/{branch}"),
        commit,
    })
}

fn refused(reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::SessionPushRefused {
        reason: reason.into(),
    }
}

/// One attempt of [`Broker::push_session`].
enum PushAttempt {
    Pushed(Box<SessionPushReport>),
    /// The remote branch was not where the attempt decided to push against;
    /// nothing was pushed. `tip` is what was read there (`None`: no branch).
    RemoteTipMoved {
        remote: String,
        tip: Option<String>,
    },
}

/// How `broker push` sends the session branch, decided by
/// [`Broker::push_path`] against the tip the remote branch holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PushPath {
    /// The remote branch is absent or an ancestor of HEAD: a plain push,
    /// which Git refuses rather than letting it replace anything.
    FastForward,
    /// A rewritten branch, leased against the oid the last `broker push`
    /// recorded for this session.
    LeaseRecorded(String),
    /// A rewritten branch, leased against an oid a succeeded journaled push
    /// of this session sent to this branch on this remote.
    LeaseJournalProven(String),
}

impl PushPath {
    /// The oid a lease expects the remote branch to hold.
    fn lease(&self) -> Option<&str> {
        match self {
            Self::FastForward => None,
            Self::LeaseRecorded(oid) | Self::LeaseJournalProven(oid) => Some(oid),
        }
    }

    /// The `phase_counts` counter recording that this path was chosen.
    fn counter(&self) -> &'static str {
        match self {
            Self::FastForward => "push_plain_fast_forward",
            Self::LeaseRecorded(_) => "push_lease_recorded",
            Self::LeaseJournalProven(_) => "push_lease_journal_proven",
        }
    }

    /// Whether `observed`, the tip the push lock's remote read saw, still
    /// allows this path: a plain push needs the remote at an ancestor of
    /// `head` (or absent), a lease needs it at exactly the leased oid.
    fn holds(&self, repo: &GitRepo, head: &str, observed: Option<&str>) -> bool {
        match self.lease() {
            None => observed.is_none_or(|tip| tip == head || repo.is_ancestor(tip, head)),
            Some(oid) => observed == Some(oid),
        }
    }
}

/// Where the remote branch is believed to be before the push reads it: the
/// newer of the oid the last `broker push` recorded and the remote-tracking
/// ref (which Git moves on every fetch and push of the branch, including
/// `broker advanced git` pushes). When they diverge the tracking ref wins;
/// the push lock's read corrects any wrong belief before Git runs.
fn believed_remote_tip(
    repo: &GitRepo,
    recorded: Option<&str>,
    tracking: Option<&str>,
) -> Option<String> {
    match (recorded, tracking) {
        (Some(recorded), Some(tracking))
            if recorded != tracking && repo.is_ancestor(tracking, recorded) =>
        {
            Some(recorded.to_string())
        }
        (_, Some(tracking)) => Some(tracking.to_string()),
        (recorded, None) => recorded.map(str::to_string),
    }
}

/// Whether a journaled operation's exact push plan sent `oid` to
/// `branch_ref`. Only the plan the broker resolved before the push counts,
/// never the command line, which may name a symbolic source.
fn journal_records_push(details: &str, branch_ref: &str, oid: &str) -> bool {
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details) else {
        return false;
    };
    let push = &details["push_reconciliation"];
    push["planning"].as_str() == Some("planned")
        && push["plan"]["destinations"]
            .as_array()
            .is_some_and(|destinations| {
                destinations.iter().any(|destination| {
                    destination["destination_ref"].as_str() == Some(branch_ref)
                        && destination["proposed_sha"].as_str() == Some(oid)
                })
            })
}

/// The refusal for a remote tip this session neither contains nor pushed.
fn foreign_remote_tip_refusal(
    session_id: i64,
    branch: &str,
    remote: &str,
    tip: &str,
) -> BrokerOpError {
    crate::phase_telemetry::add("push_refused_foreign_remote_tip", std::time::Duration::ZERO);
    refused(format!(
        "remote branch {remote}/{branch} is at {tip}, which this session's branch does not \
         contain and this session did not push (it is neither the oid broker push last \
         recorded nor one a succeeded push of this session sent), so broker push will not \
         replace it; nothing was pushed. Inspect it with `git fetch {remote} {branch}` and \
         `git log HEAD..{tip}`, then merge it in with `aethyme broker sync --session \
         {session_id}` (or `git merge {tip}`) and push again"
    ))
}

/// Whether Git refused the push because the remote ref was not where the
/// push expected it: a lease that no longer holds (`stale info`), or a plain
/// push the remote moved past (`fetch first`, `non-fast-forward`). Git prints
/// these on stderr, or on stdout with `--porcelain`.
fn remote_tip_rejection(output: &str) -> bool {
    output.contains("[rejected]")
        && ["(stale info)", "(fetch first)", "(non-fast-forward)"]
            .iter()
            .any(|reason| output.contains(reason))
}

fn operation_failure(phase: &'static str, report: &CoordinatedOperationReport) -> BrokerOpError {
    if report.operation.status == crate::OperationStatus::OutcomeUnknown {
        BrokerOpError::CoordinatedOperationBlocked {
            repository: report.operation.repository.clone(),
            operation_id: report.operation.id,
            recovery: UnknownOutcomeRecovery::from_operation(&report.operation),
        }
    } else {
        BrokerOpError::SessionPushFailed {
            phase,
            operation_id: report.operation.id,
            status: report.operation.status.as_str(),
            stderr: push_failure_output(
                &report.stdout,
                &report.stderr,
                report.stdout_truncated,
                report.stderr_truncated,
            ),
        }
    }
}

const PRE_PUSH_PATH_HINT: &str = "Hint: the pre-push hook could not find a required executable. Git hooks inherit the PATH of the aethyme process; ensure it includes the hook's toolchain (interactive shell startup files are not loaded), then retry.";

/// The PATH hint, when a refused push reports a missing executable. Only a
/// failed push qualifies: the same failure text is shared by every coordinated
/// git and gh operation, and gh's `HTTP 404: Not Found` would otherwise read
/// as a hook that could not find its toolchain.
pub(crate) fn missing_executable_path_hint(stdout: &str, stderr: &str) -> Option<&'static str> {
    let output = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    let refused_push =
        output.contains("failed to push some refs") || output.contains("pre-push hook");
    if !refused_push {
        return None;
    }
    [
        "command not found",
        ": not found",
        "no such file or directory",
        "executable file not found",
        "not recognized as an internal or external command",
    ]
    .iter()
    .any(|marker| output.contains(marker))
    .then_some(PRE_PUSH_PATH_HINT)
}

pub(crate) fn with_pre_push_path_hint(reason: &str) -> String {
    if reason.contains("pre-push hook refused this push")
        && missing_executable_path_hint("", reason).is_some()
        && !reason.contains(PRE_PUSH_PATH_HINT)
    {
        format!("{reason}\n{PRE_PUSH_PATH_HINT}")
    } else {
        reason.to_string()
    }
}

fn push_failure_output(
    stdout: &str,
    stderr: &str,
    stdout_truncated: bool,
    stderr_truncated: bool,
) -> String {
    const MAX_LINES_PER_STREAM: usize = 3;
    const MAX_CHARS_PER_LINE: usize = 400;

    fn tail(text: &str) -> Option<String> {
        let mut lines = Vec::new();
        for line in text.lines() {
            if let Some(line) = crate::text_redaction::failure_message(line, MAX_CHARS_PER_LINE) {
                if lines.len() == MAX_LINES_PER_STREAM {
                    lines.remove(0);
                }
                lines.push(line);
            }
        }
        (!lines.is_empty()).then(|| lines.join(" | "))
    }

    let mut details = Vec::new();
    if !stdout_truncated && let Some(stdout) = tail(stdout) {
        details.push(format!("push stdout: {stdout}"));
    }
    if !stderr_truncated && let Some(stderr) = tail(stderr) {
        details.push(format!("push stderr: {stderr}"));
    }
    if stdout_truncated {
        details.push("push stdout was truncated after the broker capture limit".into());
    }
    if stderr_truncated {
        details.push("push stderr was truncated after the broker capture limit".into());
    }
    if !stdout_truncated
        && !stderr_truncated
        && let Some(hint) = missing_executable_path_hint(stdout, stderr)
    {
        details.push(hint.to_string());
    }
    details.join("\n")
}

/// Why `branch` may not be published by `broker push`, if it may not.
fn branch_refusal(branch: &str, default_branch: &str) -> Option<String> {
    let Some(name) = branch.strip_prefix(SESSION_BRANCH_PREFIX) else {
        return Some(format!(
            "{branch:?} is not a session branch; broker push publishes only \
             {SESSION_BRANCH_PREFIX}* branches"
        ));
    };
    if name.is_empty() {
        return Some(format!("{branch:?} is not a session branch"));
    }
    if branch == default_branch || branch.starts_with("refs/") || branch.contains("..") {
        return Some(format!("{branch:?} is not a publishable session branch"));
    }
    None
}

/// Whether the trusted repository policy authorizes session-branch pushes.
///
/// Read from the committed `.aethyme/config.toml` at the default branch's
/// remote-tracking ref -- never from a worktree or the session's own branch,
/// which the pushing agent could have edited. This is the same rule `ship`
/// applies to its delivery policy (a copy of the default branch), taken from
/// the last fetch rather than a network read.
/// [`push_authorized`] for readers that only report or tighten: `status`,
/// `doctor`, `finish`. No tracked default branch, or a policy that cannot be
/// read, means "not opted in" rather than an error, so those commands keep
/// working in a repository `broker push` would refuse.
pub(crate) fn session_push_enabled(repo: &GitRepo) -> bool {
    tracked_default(repo).is_some_and(|default| push_authorized(repo, &default).unwrap_or(false))
}

fn push_authorized(repo: &GitRepo, default: &TrackedDefault) -> Result<bool, BrokerOpError> {
    let Some(text) = repo.config_toml_at_commit(&default.commit, "config")? else {
        return Ok(false);
    };
    let config = RepositoryDeliveryConfig::from_config_text(&text).map_err(refused)?;
    Ok(config.is_some_and(|config| config.push_session_branches))
}

/// Read the requested PR mode from the same trusted default-branch commit as
/// the push authorization. A worktree or session edit cannot change the mode.
fn pull_request_policy(
    repo: &GitRepo,
    default: &TrackedDefault,
) -> Result<SessionPullRequestPolicy, BrokerOpError> {
    let Some(text) = repo.config_toml_at_commit(&default.commit, "config")? else {
        return Ok(SessionPullRequestPolicy::default());
    };
    SessionPullRequestPolicy::from_config_text(&text).map_err(refused)
}

impl Broker {
    /// Where a session's branch stands against its remote-tracking ref.
    /// See [`SessionPushState`]; this never fetches or writes.
    pub fn session_push_state(
        &mut self,
        session_id: i64,
    ) -> Result<SessionPushState, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let repo = self.repo_handle();
        let remote = tracked_default(repo)
            .map(|default| default.remote)
            .unwrap_or_else(|| "origin".to_string());
        let head = repo
            .resolve_ref(&format!("refs/heads/{}", session.branch))
            .ok_or_else(|| BrokerOpError::SessionBranchMissing {
                session_id,
                branch: session.branch.clone(),
            })?;
        let remote_oid = repo.resolve_ref(&format!("refs/remotes/{remote}/{}", session.branch));
        let unpushed = repo.commits_not_on_remotes(&head)?;
        Ok(SessionPushState {
            branch: session.branch,
            head_oid: head,
            remote_oid,
            unpushed_commits: u32::try_from(unpushed.len()).unwrap_or(u32::MAX),
            oldest_unpushed_at_ms: unpushed.last().map(|commit| commit.committed_at_ms),
        })
    }

    /// Decide how to push `head` to the session branch when the remote
    /// branch is at `tip` (`None`: absent). A tip `head` contains is a plain
    /// push. Otherwise the branch was rewritten, and the lease may expect
    /// `tip` only when it is this session's own: the oid the last
    /// `broker push` recorded, or the proposed oid of a succeeded journaled
    /// push of this session to this branch on this remote. Anything else
    /// refuses before Git runs.
    #[allow(clippy::too_many_arguments)]
    fn push_path(
        &mut self,
        session_id: i64,
        branch: &str,
        remote: &str,
        coordination_key: &str,
        head: &str,
        recorded: Option<&str>,
        tip: Option<&str>,
    ) -> Result<PushPath, BrokerOpError> {
        let Some(tip) = tip else {
            return Ok(PushPath::FastForward);
        };
        if tip == head || self.repo_handle().is_ancestor(tip, head) {
            return Ok(PushPath::FastForward);
        }
        if recorded == Some(tip) {
            return Ok(PushPath::LeaseRecorded(tip.to_string()));
        }
        let branch_ref = format!("refs/heads/{branch}");
        let journaled = self
            .store()
            .session_git_write_details_mentioning(session_id, coordination_key, tip)?
            .iter()
            .any(|details| journal_records_push(details, &branch_ref, tip));
        if journaled {
            return Ok(PushPath::LeaseJournalProven(tip.to_string()));
        }
        Err(foreign_remote_tip_refusal(session_id, branch, remote, tip))
    }

    /// Keep the oid `broker push` leases against current when a session
    /// pushes its own branch through `broker advanced git`: record what that
    /// succeeded push sent, when it went to the remote `broker push` uses.
    /// Best-effort; `broker push` also proves such a push from the journal.
    pub(crate) fn note_session_branch_push(
        &mut self,
        session: &crate::Session,
        report: &CoordinatedOperationReport,
    ) {
        let branch_ref = format!("refs/heads/{}", session.branch);
        let Some(pushed) = report
            .pushed_refs
            .iter()
            .find(|pushed| pushed.destination_ref == branch_ref)
        else {
            return;
        };
        let Some(target) = report.resolved_target.as_ref() else {
            return;
        };
        let repo = self.repo_handle();
        let same_remote = tracked_default(repo).is_some_and(|default| {
            repo.resolve_remote_target(&default.remote, None)
                .is_ok_and(|resolved| resolved.coordination_key == target.coordination_key)
        });
        if same_remote {
            crate::warn_unrecorded(
                "record the session branch push for broker push",
                self.store()
                    .meta_set(&pushed_oid_key(session.id), &pushed.proposed_sha),
            );
        }
    }

    /// Publish a live session's own branch, and optionally open a pull request
    /// in the trusted repository-configured mode. See module docs for authorization.
    ///
    /// The push is decided against the tip the remote branch is believed to
    /// hold and re-checked against the tip read under the push lock. When the
    /// two differ, nothing is pushed and the push is decided once more against
    /// the tip actually read; a second difference refuses.
    pub fn push_session(
        &mut self,
        session_id: i64,
        open_pr: bool,
    ) -> Result<SessionPushReport, BrokerOpError> {
        let tip = match self.push_session_attempt(session_id, open_pr, None)? {
            PushAttempt::Pushed(report) => return Ok(*report),
            PushAttempt::RemoteTipMoved { tip, .. } => tip,
        };
        crate::phase_telemetry::add("push_remote_tip_redecided", std::time::Duration::ZERO);
        match self.push_session_attempt(session_id, open_pr, Some(tip))? {
            PushAttempt::Pushed(report) => Ok(*report),
            PushAttempt::RemoteTipMoved { remote, tip } => {
                let branch = self.store().session(session_id)?.branch;
                Err(refused(format!(
                    "remote branch {remote}/{branch} moved again while broker push waited for \
                     the push lock (it is now at {}); nothing was pushed. Inspect it with \
                     `git fetch {remote} {branch}` and `git log HEAD..FETCH_HEAD`, merge it in \
                     with `aethyme broker sync --session {session_id}` if it should be kept, \
                     then re-run `aethyme broker push --session {session_id}`",
                    tip.as_deref().unwrap_or("nothing: the branch was deleted"),
                )))
            }
        }
    }

    /// One decision and push for [`Self::push_session`]. `known_tip`, when
    /// given, is the tip a previous attempt read on the remote under the lock
    /// (`Some(None)`: the branch does not exist there).
    fn push_session_attempt(
        &mut self,
        session_id: i64,
        open_pr: bool,
        known_tip: Option<Option<String>>,
    ) -> Result<PushAttempt, BrokerOpError> {
        let mut phase_timings_ms = std::collections::BTreeMap::new();
        let phase_started = std::time::Instant::now();
        let session = self.store().session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let main_root = self.main_root().to_path_buf();
        let session_worktree = Path::new(&session.worktree_path);
        if !session_worktree.is_dir() {
            return Err(refused(format!(
                "session {session_id} worktree is missing; broker push needs the session checkout to run its pre-push hook"
            )));
        }
        let repo = self.repo_handle();
        let default = tracked_default(repo).ok_or_else(|| {
            refused(
                "the main checkout's branch has no fetched upstream, so there is no trusted \
                 default branch to read the push policy from; set one with \
                 `git branch --set-upstream-to <remote>/<branch>`",
            )
        })?;
        if let Some(reason) = branch_refusal(&session.branch, &default.branch) {
            return Err(refused(reason));
        }
        if !push_authorized(repo, &default)? {
            return Err(refused(format!(
                "repository policy {SESSION_PUSH_POLICY_KEY} is not enabled on {} ({}); add \
                 `push_session_branches = true` under `[delivery]` in .aethyme/config.toml on \
                 the default branch, or publish explicitly with `aethyme broker advanced git \
                 --session {session_id} --reason <authorization> -- push ...`",
                default.tracking_ref,
                &default.commit[..default.commit.len().min(12)],
            )));
        }
        // Parse the PR mode before the first remote write. A bad preference
        // must not leave a branch pushed when opening the requested PR refuses.
        let pr_open_as = if open_pr {
            Some(pull_request_policy(repo, &default)?)
        } else {
            None
        };
        let target = repo
            .resolve_remote_target(&default.remote, None)
            .map_err(|error| refused(format!("remote {:?}: {error}", default.remote)))?;
        // The push reads the remote under the lock anyway; asking it for the
        // default branch too can prove the fetch after the push unnecessary.
        // `--pr` keeps fetching after it opens the pull request, as before.
        let remote_default_ref = format!("refs/heads/{}", default.branch);
        let observe_default = !open_pr
            && push_fetch_skip_unmoved_enabled()
            && fetch_and_push_share_endpoint(repo, &default.remote);
        let branch_ref = format!("refs/heads/{}", session.branch);
        let head =
            repo.resolve_ref(&branch_ref)
                .ok_or_else(|| BrokerOpError::SessionBranchMissing {
                    session_id,
                    branch: session.branch.clone(),
                })?;
        let tracking = repo.resolve_ref(&format!(
            "refs/remotes/{}/{}",
            default.remote, session.branch
        ));
        let new_commits = repo.commits_not_on_remotes(&head)?.len();
        let uncommitted = session_worktree
            .is_dir()
            .then(|| GitRepo::discover(session_worktree).ok())
            .flatten()
            .and_then(|worktree| worktree.uncommitted_summary().ok())
            .unwrap_or_default();
        let recorded = self.store().meta_get(&pushed_oid_key(session_id))?;
        let repo = self.repo_handle();

        // A plain push when the remote only moves forward: Git itself refuses
        // anything else. A rewritten branch (rebase, amend) needs a lease, and
        // the lease is only ever against an oid this broker recorded pushing
        // for this session -- never one merely seen in a fetch, which could be
        // someone else's work under the same name. "Recorded" is the oid the
        // last `broker push` stored, or one a succeeded journaled push of this
        // session sent to this branch on this remote (`broker advanced git`).
        // Which applies is decided against the tip the remote is believed to
        // hold, and re-checked under the push lock against the tip it holds.
        let believed_tip = match known_tip {
            Some(tip) => tip,
            None => believed_remote_tip(repo, recorded.as_deref(), tracking.as_deref()),
        };
        let path = self.push_path(
            session_id,
            &session.branch,
            &default.remote,
            &target.coordination_key,
            &head,
            recorded.as_deref(),
            believed_tip.as_deref(),
        )?;
        crate::phase_telemetry::add(path.counter(), std::time::Duration::ZERO);
        let fast_forward = path == PushPath::FastForward;
        let (args, effect) = if fast_forward {
            // The strict spelling holds only this branch's lane (and its one
            // tracking ref), so pushes of different session branches do not
            // queue behind each other. A repository whose configuration
            // cannot prove that footprint keeps the repository-wide spelling.
            let strict = crate::operations::strict_push_args(&default.remote, &head, &branch_ref);
            let args = if crate::operations::session_branch_push_lane_available(
                self.main_root(),
                session_worktree,
                &strict,
            ) {
                strict
            } else {
                vec![
                    "push".to_string(),
                    default.remote.clone(),
                    format!("{head}:{branch_ref}"),
                ]
            };
            (args, OperationEffect::Write)
        } else if let Some(pushed) = path.lease() {
            (
                vec![
                    "push".to_string(),
                    format!("--force-with-lease={branch_ref}:{pushed}"),
                    default.remote.clone(),
                    format!("{head}:{branch_ref}"),
                ],
                OperationEffect::Destructive,
            )
        } else {
            // `push_path` refuses every tip it can neither fast-forward nor
            // lease, so this is unreachable; refusing keeps it harmless.
            return Err(foreign_remote_tip_refusal(
                session_id,
                &session.branch,
                &default.remote,
                believed_tip.as_deref().unwrap_or_default(),
            ));
        };
        phase_timings_ms.insert(
            "push_preflight".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        let phase_started = std::time::Instant::now();
        let push_command = CoordinatedCommand {
            session_id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: Some(target.clone()),
            scope: Some(format!("session-push:{branch_ref}")),
            declared_effect: Some(effect),
            // The lease is the confirmation: it can only replace this
            // session's own last push, on this session's own branch.
            destructive_confirmed: effect == OperationEffect::Destructive,
            cross_session: None,
            ref_write_acknowledged: false,
            authorization_reason: Some(format!("repository policy {SESSION_PUSH_POLICY_KEY}")),
            args,
        };
        // What the push lock's remote read saw, when it no longer allows the
        // path chosen above. Git did not run; the caller decides again.
        let moved_tip = std::cell::RefCell::new(None::<Option<String>>);
        let check_repo = GitRepo::discover(&main_root)?;
        let tip_check: &dyn Fn(Option<&str>) -> Result<(), String> = &|observed| {
            if path.holds(&check_repo, &head, observed) {
                return Ok(());
            }
            *moved_tip.borrow_mut() = Some(observed.map(str::to_owned));
            Err(format!(
                "remote branch {} is at {}, not where broker push decided to push against; \
                 nothing was pushed",
                session.branch,
                observed.unwrap_or("nothing")
            ))
        };
        let push = self.run_coordinated_session_push(
            push_command,
            session_worktree,
            push_operation_wait(),
            observe_default.then_some(remote_default_ref.as_str()),
            (branch_ref.as_str(), tip_check),
        );
        if let Some(tip) = moved_tip.borrow_mut().take() {
            return Ok(PushAttempt::RemoteTipMoved {
                remote: default.remote,
                tip,
            });
        }
        // Git's own check is the last line: the remote moved after the lock's
        // read (or before an outside-the-lock hook dry run), outside the
        // broker. Read what it holds instead of relaying the raw rejection,
        // and decide again against it.
        let rejected = match &push {
            Ok(push) => {
                !push.ok()
                    && (remote_tip_rejection(&push.stderr) || remote_tip_rejection(&push.stdout))
            }
            Err(error) => remote_tip_rejection(&error.to_string()),
        };
        if rejected {
            crate::phase_telemetry::add("push_remote_tip_rejected", std::time::Duration::ZERO);
            return match self
                .repo_handle()
                .remote_ref_oids(&default.remote, std::slice::from_ref(&branch_ref))
            {
                Ok(tips) => Ok(PushAttempt::RemoteTipMoved {
                    tip: tips.get(&branch_ref).cloned().flatten(),
                    remote: default.remote,
                }),
                Err(error) => Err(refused(format!(
                    "Git refused the push because remote branch {remote}/{branch} is no longer \
                     where broker push expected it, and reading where it is failed ({error}); \
                     nothing was pushed. Inspect it with `git fetch {remote} {branch}` and \
                     `git log HEAD..FETCH_HEAD`, merge it in with `aethyme broker sync \
                     --session {session_id}` if it should be kept, then re-run \
                     `aethyme broker push --session {session_id}`",
                    remote = default.remote,
                    branch = session.branch,
                ))),
            };
        }
        let push = push?;
        if !push.ok() {
            return Err(operation_failure("push", &push));
        }
        self.store().meta_set(&pushed_oid_key(session_id), &head)?;
        phase_timings_ms.insert(
            "push_git".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        let phase_started = std::time::Instant::now();

        let mut pr = if let Some(policy) = pr_open_as {
            Some(self.open_session_pull_request(
                &session,
                &default,
                &target.display_slug,
                &head,
                policy.open_as,
                &main_root,
            )?)
        } else {
            None
        };
        // The review run on push needs the open pull request even when this
        // push did not ask to open one. Looked up after the overlap check,
        // whose fresh open-PR listing usually names it already.
        let pr_lookup_for_review = pr_open_as.is_none()
            && crate::ReviewPolicy::load(&main_root).is_ok_and(|policy| policy.run_on_push);

        // Association happens only after --pr confirms this session's PR.
        // A push that already succeeded must not become an error if watching fails.
        let (pr_watch_id, pr_watch_note) = match (pr_open_as, pr.as_ref()) {
            (Some(policy), Some(pr)) if policy.watch_on_start => {
                match self.ensure_session_pull_request_watch(
                    session_id,
                    &target.display_slug,
                    pr.number,
                    &crate::GithubCliPullRequestWatchProvider,
                    crate::clock::epoch_ms(),
                ) {
                    Ok(id) => (Some(id), None),
                    Err(error) => (
                        None,
                        Some(format!(
                            "push succeeded, but automatic PR watch was not established: {error}; \
                         retry broker push --session {session_id} --pr"
                        )),
                    ),
                }
            }
            _ => (None, None),
        };

        phase_timings_ms.insert(
            "push_pr_and_watch".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        let phase_started = std::time::Instant::now();

        // Advisory and best-effort, like the overlap check below: the default
        // branch moved under this session whenever another PR merged, and
        // that is only visible after fetching it. When the push's own remote
        // read showed the remote default branch exactly where the tracking
        // ref is, there is nothing to fetch. Otherwise, unless that read
        // proved it moved, a fetch any broker command made within the last
        // minute is reused, except with `--pr`, whose size measurement should
        // see the current default branch.
        let proof = if observe_default {
            let _span = crate::phase_telemetry::span("default_branch_unmoved_check");
            let tracking_oid = self.repo_handle().resolve_ref(&default.tracking_ref);
            default_branch_proof(
                push.remote_default_observation.as_ref(),
                &remote_default_ref,
                tracking_oid.as_deref(),
            )
        } else {
            DefaultBranchProof::Unavailable
        };
        let reused_fetch_age_ms = match proof {
            DefaultBranchProof::Unmoved { read_started_at_ms } => {
                crate::phase_telemetry::add(
                    "default_branch_fetch_skipped_unmoved",
                    std::time::Duration::ZERO,
                );
                self.record_default_branch_unmoved(&default, read_started_at_ms);
                Some(
                    crate::clock::epoch_ms()
                        .saturating_sub(read_started_at_ms)
                        .max(0),
                )
            }
            DefaultBranchProof::Moved => None,
            DefaultBranchProof::Unavailable if open_pr => None,
            DefaultBranchProof::Unavailable => {
                let reused = self.reusable_default_branch_fetch_age_ms(&default);
                if reused.is_some() {
                    crate::phase_telemetry::add(
                        "default_branch_fetch_reused",
                        std::time::Duration::ZERO,
                    );
                }
                reused
            }
        };
        let skipped_unmoved = matches!(proof, DefaultBranchProof::Unmoved { .. });
        let (default_branch, default_branch_note, _) = if reused_fetch_age_ms.is_some() {
            self.measure_default_branch(session_id, &session.branch, &default, &head, None)
        } else {
            self.fetch_and_measure_default_branch(
                session_id,
                &session.branch,
                &default,
                Some(&target),
                &head,
                &main_root,
            )
        };

        phase_timings_ms.insert(
            "push_default_branch".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        let phase_started = std::time::Instant::now();

        // Advisory and best-effort: a listing or diff that cannot be read
        // degrades to "unknown" and never fails a push that already happened.
        let (overlap, listed_pr) = self.check_pr_overlaps_for_push(
            session_id,
            &session.branch,
            &head,
            &target.display_slug,
            &main_root,
            crate::clock::epoch_ms(),
        );

        phase_timings_ms.insert(
            "push_overlap".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        let mut pr_lookup_listing_age_seconds = None;
        if pr_lookup_for_review {
            let _span = crate::phase_telemetry::span("push_pr_lookup");
            pr = match listed_pr {
                Some(listed) => {
                    crate::phase_telemetry::add("push_pr_lookup_reused", std::time::Duration::ZERO);
                    pr_lookup_listing_age_seconds = Some(listing_age_seconds(
                        listed.listed_at_ms,
                        crate::clock::epoch_ms(),
                    ));
                    Some(SessionPullRequest {
                        url: listed.url,
                        number: listed.number,
                        state: "OPEN".into(),
                        created: false,
                        ci_skips_drafts: false,
                        draft: listed.draft,
                    })
                }
                // A failed lookup only means no review runs; the push
                // already happened.
                None => self
                    .find_session_pull_request(
                        session_id,
                        &target.display_slug,
                        &session.branch,
                        &main_root,
                    )
                    .ok()
                    .flatten(),
            };
        }
        let phase_started = std::time::Instant::now();
        let payload = serde_json::json!({
            "session_id": session_id,
            "branch": session.branch,
            "oid": head,
            "pr_url": pr.as_ref().map(|pr| pr.url.clone()),
        })
        .to_string();
        {
            let _span = crate::phase_telemetry::span("push_finalization_event");
            self.store().append_event(
                crate::events::BROKER_SESSION_PUSHED,
                Some(session_id),
                Some(&payload),
            )?;
        }

        let pr_size = if open_pr {
            measure_pr_size(&main_root, &default.tracking_ref, &head)
        } else {
            None
        };
        let duplicate_work = {
            let _span = crate::phase_telemetry::span("push_finalization_duplicate_work");
            self.duplicate_work_for(&session)
        };
        phase_timings_ms.insert(
            "push_finalization".into(),
            phase_started.elapsed().as_millis() as u64,
        );
        Ok(PushAttempt::Pushed(Box::new(SessionPushReport {
            session_id,
            branch: session.branch,
            remote: default.remote,
            pushed_oid: head,
            previous_remote_oid: believed_tip,
            commits_pushed: u32::try_from(new_commits).unwrap_or(u32::MAX),
            uncommitted_files: u32::try_from(uncommitted.len()).unwrap_or(u32::MAX),
            uncommitted: UncommittedCounts::from(&uncommitted),
            pr,
            pr_watch_id,
            pr_watch_note,
            pr_overlaps: overlap.overlaps,
            pr_overlaps_unknown: overlap.unknown_prs,
            pr_overlap_listing_unavailable: overlap.listing_unavailable,
            duplicate_work,
            default_branch,
            default_branch_note,
            default_branch_fetch_age_seconds: reused_fetch_age_ms
                .map(|age_ms| u64::try_from(age_ms / 1000).unwrap_or(0)),
            pr_lookup_listing_age_seconds,
            default_branch_fetch_skipped_unmoved: skipped_unmoved,
            pr_size,
            review_run: None,
            phase_timings_ms,
            repository: target.display_slug.clone(),
        })))
    }

    /// Fetch exactly the default branch into its remote-tracking ref through
    /// the coordinated operation lane, on behalf of `session_id`.
    ///
    /// The fetch moves only that one remote-tracking ref, like every other
    /// remote read that moves a ref. `Err` carries a note saying why the
    /// caller is working from the last fetched copy; a failed fetch is never
    /// fatal to `push`, `start` or `sync`.
    ///
    /// Every outcome is recorded under [`default_branch_fetch_key`], so a
    /// `push` moments later can reuse a successful fetch and never a failed one.
    pub(crate) fn fetch_default_branch(
        &mut self,
        session_id: i64,
        default: &TrackedDefault,
        target: Option<&crate::ResolvedRemoteTarget>,
        main_root: &Path,
    ) -> Result<(), String> {
        let started_at_ms = crate::clock::epoch_ms();
        let fetched = self.run_default_branch_fetch(session_id, default, target, main_root);
        let record = match &fetched {
            Ok(()) => started_at_ms.to_string(),
            Err(_) => DEFAULT_BRANCH_FETCH_FAILED.to_string(),
        };
        crate::warn_unrecorded(
            "record the default-branch fetch time",
            self.store()
                .meta_set(&default_branch_fetch_key(&default.tracking_ref), &record),
        );
        fetched
    }

    /// Record that the tracking ref was proven current at `proven_at_ms`, as
    /// a fetch started then would have: see [`record_after_unmoved_proof`].
    fn record_default_branch_unmoved(&mut self, default: &TrackedDefault, proven_at_ms: i64) {
        let key = default_branch_fetch_key(&default.tracking_ref);
        let recorded = self.store_ref().meta_get(&key).ok().flatten();
        if let Some(record) = record_after_unmoved_proof(recorded.as_deref(), proven_at_ms) {
            crate::warn_unrecorded(
                "record the default-branch proof time",
                self.store().meta_set(&key, &record),
            );
        }
    }

    /// How old the last recorded default-branch fetch is, when `push` may
    /// compare against the tracking ref it left instead of fetching again.
    fn reusable_default_branch_fetch_age_ms(&self, default: &TrackedDefault) -> Option<i64> {
        if !push_fetch_reuse_enabled() {
            return None;
        }
        let recorded = self
            .store_ref()
            .meta_get(&default_branch_fetch_key(&default.tracking_ref))
            .ok()
            .flatten();
        let age_ms = reusable_fetch_age_ms(
            recorded.as_deref(),
            crate::clock::epoch_ms(),
            PUSH_FETCH_REUSE_WINDOW_MS,
        )?;
        self.repo_handle().resolve_ref(&default.tracking_ref)?;
        Some(age_ms)
    }

    fn run_default_branch_fetch(
        &mut self,
        session_id: i64,
        default: &TrackedDefault,
        target: Option<&crate::ResolvedRemoteTarget>,
        main_root: &Path,
    ) -> Result<(), String> {
        let _span = crate::phase_telemetry::span("default_branch_fetch");
        let target = match target {
            Some(target) => target.clone(),
            None => self
                .repo_handle()
                .resolve_remote_target(&default.remote, None)
                .map_err(|error| {
                    format!(
                        "remote {:?} cannot be resolved ({error}); used the last fetched copy",
                        default.remote
                    )
                })?,
        };
        let source = format!("refs/heads/{}", default.branch);
        let fetch = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(target),
                scope: Some(format!("ref:{}", default.tracking_ref)),
                declared_effect: None,
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(
                    "refresh the default branch to compare a session with it".into(),
                ),
                args: vec![
                    "fetch".into(),
                    default.remote.clone(),
                    format!("{source}:{}", default.tracking_ref),
                ],
            },
            main_root,
            fetch_operation_wait(),
        );
        match fetch {
            Ok(fetch) if fetch.ok() => {
                crate::environments::prebuild::nudge_after_fetch(main_root);
                Ok(())
            }
            Ok(fetch) => Err(format!(
                "fetching {} failed (operation {}); used the last fetched copy",
                default.tracking_ref, fetch.operation.id
            )),
            Err(error) => Err(format!(
                "fetching {} failed ({error}); used the last fetched copy",
                default.tracking_ref
            )),
        }
    }

    /// Fetch exactly the default branch, then compare `head` with it.
    ///
    /// The result is cached so `status` can show it without the network. A
    /// failed fetch falls back to the last fetched copy and says so.
    pub(crate) fn fetch_and_measure_default_branch(
        &mut self,
        session_id: i64,
        branch: &str,
        default: &TrackedDefault,
        target: Option<&crate::ResolvedRemoteTarget>,
        head: &str,
        main_root: &Path,
    ) -> (
        Option<crate::DefaultBranchDrift>,
        Option<String>,
        DefaultBranchRefreshEvidence,
    ) {
        let fetch_error = self
            .fetch_default_branch(session_id, default, target, main_root)
            .err();
        self.measure_default_branch(session_id, branch, default, head, fetch_error)
    }

    /// Compare `head` with the default branch's tracking ref as it stands,
    /// and cache the result for `status`. `fetch_error` says why that ref may
    /// be stale; `None` means it was just fetched, or `push` reused a fetch
    /// recent enough to stand for one.
    fn measure_default_branch(
        &mut self,
        session_id: i64,
        branch: &str,
        default: &TrackedDefault,
        head: &str,
        fetch_error: Option<String>,
    ) -> (
        Option<crate::DefaultBranchDrift>,
        Option<String>,
        DefaultBranchRefreshEvidence,
    ) {
        let mut note = fetch_error.clone();
        let repo = self.repo_handle();
        let Some(commit) = repo.resolve_ref(&default.tracking_ref) else {
            return (
                None,
                Some(format!("{} is not fetched", default.tracking_ref)),
                DefaultBranchRefreshEvidence {
                    tracking_ref: default.tracking_ref.clone(),
                    display_ref: format!("{}/{}", default.remote, default.branch),
                    commit: None,
                    fetched: false,
                },
            );
        };
        let reference = format!("{}/{}", default.remote, default.branch);
        let catch_up = crate::main_drift::CatchUp::for_branch(repo, &default.remote, branch);
        let drift = match crate::main_drift::measure(repo, &reference, &commit, head, catch_up) {
            Ok(mut drift) => {
                if fetch_error.is_none()
                    && let Ok(session) = self.store_ref().session(session_id)
                {
                    crate::main_drift::assess_sync_impact(
                        repo,
                        Path::new(&session.worktree_path),
                        &mut drift,
                    );
                }
                if let Ok(raw) = serde_json::to_string(&drift) {
                    crate::warn_unrecorded(
                        "cache the default-branch comparison",
                        self.store()
                            .meta_set(&crate::main_drift::cache_key(session_id), &raw),
                    );
                }
                Some(drift)
            }
            Err(error) => {
                let comparison_note =
                    format!("could not compare the session with {reference}: {error}");
                note = Some(match note {
                    Some(fetch_note) => format!("{fetch_note}; {comparison_note}"),
                    None => comparison_note,
                });
                None
            }
        };
        (
            drift,
            note,
            DefaultBranchRefreshEvidence {
                tracking_ref: default.tracking_ref.clone(),
                display_ref: reference,
                commit: Some(commit),
                fetched: fetch_error.is_none(),
            },
        )
    }

    /// Find the open pull request for the session branch, or create one in the
    /// trusted configured mode. Existing pull requests are never changed.
    fn open_session_pull_request(
        &mut self,
        session: &crate::Session,
        default: &TrackedDefault,
        repository: &str,
        head: &str,
        open_as: SessionPullRequestOpenAs,
        cwd: &Path,
    ) -> Result<SessionPullRequest, BrokerOpError> {
        if let Some(existing) =
            self.find_session_pull_request(session.id, repository, &session.branch, cwd)?
        {
            return Ok(existing);
        }
        let commits = self
            .repo_handle()
            .commits_between(&default.tracking_ref, head)?;
        let title = commits
            .last()
            .map(|commit| commit.subject.clone())
            .filter(|subject| !subject.trim().is_empty())
            .or_else(|| session.task.clone())
            .unwrap_or_else(|| session.branch.clone());
        let base = BaseBranchFiles::read(self.repo_handle(), &default.tracking_ref);
        let messages: Vec<String> = commits
            .iter()
            .rev()
            .map(|commit| {
                self.repo_handle()
                    .commit_message(&commit.sha)
                    .unwrap_or_else(|_| commit.subject.clone())
            })
            .collect();
        let body_commits: Vec<crate::pr_body::BodyCommit<'_>> = commits
            .iter()
            .rev()
            .zip(&messages)
            .map(|(commit, message)| crate::pr_body::BodyCommit {
                sha: &commit.sha,
                subject: &commit.subject,
                message,
            })
            .collect();
        let body = crate::pr_body::compose(
            session.task.as_deref(),
            &body_commits,
            base.template.as_deref(),
        )
        .map_err(|error| refused(error.to_string()))?;
        let mut args = vec!["pr".into(), "create".into()];
        if open_as == SessionPullRequestOpenAs::Draft {
            args.push("--draft".into());
        }
        args.extend([
            "--head".into(),
            session.branch.clone(),
            "--base".into(),
            default.branch.clone(),
            "--title".into(),
            title,
            "--body".into(),
            body,
        ]);
        let create = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id: session.id,
                provider: OperationProvider::Github,
                repository: Some(repository.to_string()),
                resolved_target: None,
                scope: Some(format!("session-push:pr-create:{}", session.branch)),
                declared_effect: Some(OperationEffect::Write),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(format!("repository policy {SESSION_PUSH_POLICY_KEY}")),
                args,
            },
            cwd,
            push_operation_wait(),
        )?;
        if !create.ok() {
            return Err(operation_failure("pull request creation", &create));
        }
        // The provider, not gh's output text, is the evidence the PR exists.
        let mut created = self
            .find_session_pull_request(session.id, repository, &session.branch, cwd)?
            .ok_or_else(|| BrokerOpError::SessionPushFailed {
                phase: "pull request lookup after creation",
                operation_id: create.operation.id,
                status: create.operation.status.as_str(),
                stderr: format!(
                    "gh reported success but lists no open pull request for {}",
                    session.branch
                ),
            })?;
        created.created = true;
        created.ci_skips_drafts = base.ci_skips_drafts;
        // This push opened the pull request, so its opening is known now, not
        // only once someone watches it — and PR monitoring is off by default,
        // so without this most pull requests would never get a lifetime in
        // `broker advanced insights`. Recorded after the provider confirmed the
        // pull request exists, so the instant is never before the real one; a
        // later watch poll that reads the provider's `createdAt` can only move
        // it earlier (milestones keep the earliest sighting). Best-effort: the
        // push already succeeded and must not fail over telemetry.
        crate::warn_unrecorded(
            "record when the pull request opened",
            self.store().record_pull_request_opened(
                repository,
                created.number,
                Some(session.id),
                crate::clock::epoch_ms(),
            ),
        );
        Ok(created)
    }

    fn find_session_pull_request(
        &mut self,
        session_id: i64,
        repository: &str,
        branch: &str,
        cwd: &Path,
    ) -> Result<Option<SessionPullRequest>, BrokerOpError> {
        let list = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Github,
                repository: Some(repository.to_string()),
                resolved_target: None,
                scope: Some(format!("session-push:pr-list:{branch}")),
                declared_effect: Some(OperationEffect::Read),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: None,
                args: vec![
                    "pr".into(),
                    "list".into(),
                    "--head".into(),
                    branch.to_string(),
                    "--state".into(),
                    "open".into(),
                    "--json".into(),
                    "number,url,state,headRefName,isDraft".into(),
                ],
            },
            cwd,
            push_operation_wait(),
        )?;
        if !list.ok() {
            return Err(operation_failure("pull request lookup", &list));
        }
        parse_complete_pull_request_list(&list.stdout, branch, list.stdout_truncated)
    }
}

/// What the base branch says about the pull request about to be opened: its
/// template, and whether its CI skips drafts. Read from the fetched default
/// branch because that is the commit GitHub takes both from, and read without
/// a checkout. Unreadable means "no template" and "no note": both only shape
/// text, and must never fail a push that already happened.
struct BaseBranchFiles {
    template: Option<String>,
    ci_skips_drafts: bool,
}

impl BaseBranchFiles {
    fn read(repo: &GitRepo, base: &str) -> Self {
        let paths = repo.tracked_files_at(base).unwrap_or_default();
        let read = |path: &str| repo.file_at_commit(base, path).ok().flatten();
        let template = crate::pr_body::template_path(&paths).and_then(read);
        let ci_skips_drafts = paths
            .iter()
            .filter(|path| {
                path.starts_with(".github/workflows/")
                    && (path.ends_with(".yml") || path.ends_with(".yaml"))
            })
            .filter_map(|path| read(path))
            .any(|workflow| crate::pr_body::workflow_skips_drafts(&workflow));
        Self {
            template,
            ci_skips_drafts,
        }
    }
}

/// The open pull request whose head is exactly `branch`, from `gh pr list`
/// JSON. Anything unparseable is "none found" -- creation then fails loudly
/// on the post-create lookup rather than trusting a guess.
fn parse_pull_request_list(stdout: &str, branch: &str) -> Option<SessionPullRequest> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    value.as_array()?.iter().find_map(|pr| {
        (pr["headRefName"].as_str() == Some(branch)
            && pr["state"]
                .as_str()
                .is_some_and(|state| state.eq_ignore_ascii_case("open")))
        .then(|| SessionPullRequest {
            url: pr["url"].as_str().unwrap_or_default().to_string(),
            number: pr["number"].as_i64().unwrap_or_default(),
            state: pr["state"].as_str().unwrap_or_default().to_string(),
            created: false,
            ci_skips_drafts: false,
            draft: pr["isDraft"].as_bool().unwrap_or(false),
        })
    })
}

fn parse_complete_pull_request_list(
    stdout: &str,
    branch: &str,
    stdout_truncated: bool,
) -> Result<Option<SessionPullRequest>, BrokerOpError> {
    if stdout_truncated {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "GitHub pull request listing exceeded the broker capture limit; refusing to assume no matching pull request exists".into(),
        });
    }
    Ok(parse_pull_request_list(stdout, branch))
}

/// Measure a pushed change against `[review] pr_size` (#239).
///
/// Best-effort and advisory: an unreadable policy or diff yields `None`, and a
/// push that already happened is never failed over its size.
fn measure_pr_size(main_root: &Path, base_ref: &str, head: &str) -> Option<crate::PrSizeReport> {
    let policy = crate::ChangeClassificationPolicy::load(main_root).ok()?;
    let output = crate::git::git_command()
        .current_dir(main_root)
        .args(["diff", "--numstat", "-z", &format!("{base_ref}...{head}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = crate::parse_numstat_z(&String::from_utf8_lossy(&output.stdout));
    let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
    let inputs = crate::ChangeInputs {
        generated: crate::linguist_generated_paths(main_root, base_ref, &paths),
        files,
        ..Default::default()
    };
    let classification = crate::classify_change(&policy, &inputs);
    Some(crate::pr_size_report(&policy, &classification).with_diff_totals(&inputs.files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_reuses_only_a_successful_default_branch_fetch_inside_the_window() {
        let now = 1_000_000;
        let window = PUSH_FETCH_REUSE_WINDOW_MS;
        let at = |started: i64| started.to_string();
        assert_eq!(
            reusable_fetch_age_ms(Some(&at(now - 12_000)), now, window),
            Some(12_000)
        );
        assert_eq!(reusable_fetch_age_ms(Some(&at(now)), now, window), Some(0));
        assert_eq!(
            reusable_fetch_age_ms(Some(&at(now - window)), now, window),
            Some(window)
        );
        assert_eq!(
            reusable_fetch_age_ms(Some(&at(now - window - 1)), now, window),
            None,
            "an old fetch is not reused"
        );
        assert_eq!(reusable_fetch_age_ms(None, now, window), None, "no record");
        assert_eq!(
            reusable_fetch_age_ms(Some(DEFAULT_BRANCH_FETCH_FAILED), now, window),
            None,
            "the last fetch failed"
        );
        assert_eq!(
            reusable_fetch_age_ms(Some(&at(now + 1)), now, window),
            None,
            "a record in the future proves nothing"
        );
        assert_eq!(reusable_fetch_age_ms(Some("garbage"), now, window), None);
    }

    #[test]
    fn listing_age_is_whole_seconds_and_never_negative() {
        assert_eq!(listing_age_seconds(10_000, 12_999), 2);
        assert_eq!(listing_age_seconds(10_000, 10_000), 0);
        assert_eq!(listing_age_seconds(10_000, 9_000), 0);
    }

    #[test]
    fn only_a_remote_default_at_the_tracking_commit_proves_the_fetch_unneeded() {
        let seen = |oid: Option<&str>| crate::operations::RemoteDefaultObservation {
            remote_ref: "refs/heads/main".into(),
            oid: oid.map(str::to_string),
            read_started_at_ms: 1_000,
        };
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert_eq!(
            default_branch_proof(Some(&seen(Some(&a))), "refs/heads/main", Some(&a)),
            DefaultBranchProof::Unmoved {
                read_started_at_ms: 1_000
            }
        );
        assert_eq!(
            default_branch_proof(
                Some(&seen(Some(&a.to_ascii_uppercase()))),
                "refs/heads/main",
                Some(&a)
            ),
            DefaultBranchProof::Unmoved {
                read_started_at_ms: 1_000
            }
        );
        assert_eq!(
            default_branch_proof(Some(&seen(Some(&b))), "refs/heads/main", Some(&a)),
            DefaultBranchProof::Moved
        );
        // The remote lost its default branch, or nothing was fetched yet.
        assert_eq!(
            default_branch_proof(Some(&seen(None)), "refs/heads/main", Some(&a)),
            DefaultBranchProof::Moved
        );
        assert_eq!(
            default_branch_proof(Some(&seen(Some(&a))), "refs/heads/main", None),
            DefaultBranchProof::Moved
        );
        // A read of another ref, or no read, proves nothing.
        assert_eq!(
            default_branch_proof(Some(&seen(Some(&a))), "refs/heads/trunk", Some(&a)),
            DefaultBranchProof::Unavailable
        );
        assert_eq!(
            default_branch_proof(None, "refs/heads/main", Some(&a)),
            DefaultBranchProof::Unavailable
        );
    }

    #[test]
    fn an_unmoved_proof_records_its_read_time_unless_a_newer_fetch_is_recorded() {
        assert_eq!(record_after_unmoved_proof(None, 5_000), Some("5000".into()));
        assert_eq!(
            record_after_unmoved_proof(Some(DEFAULT_BRANCH_FETCH_FAILED), 5_000),
            Some("5000".into())
        );
        assert_eq!(
            record_after_unmoved_proof(Some("4000"), 5_000),
            Some("5000".into())
        );
        assert_eq!(record_after_unmoved_proof(Some("5000"), 5_000), None);
        assert_eq!(record_after_unmoved_proof(Some("6000"), 5_000), None);
    }

    #[test]
    fn default_branch_fetch_record_is_keyed_by_the_tracking_ref() {
        assert_eq!(
            default_branch_fetch_key("refs/remotes/origin/main"),
            "default_branch_fetch.refs/remotes/origin/main.started_at_ms"
        );
    }

    #[test]
    fn default_branch_refresh_has_a_bounded_total_admission_deadline() {
        assert!(matches!(fetch_operation_wait(), QueueWait::Seconds(30)));
    }

    #[test]
    fn new_pull_request_mode_defaults_to_draft_and_accepts_ready() {
        assert_eq!(
            SessionPullRequestPolicy::from_config_text(
                "[delivery]\npush_session_branches = true\n"
            )
            .unwrap()
            .open_as,
            SessionPullRequestOpenAs::Draft
        );
        assert_eq!(
            SessionPullRequestPolicy::from_config_text("[broker.pr]\n")
                .unwrap()
                .open_as,
            SessionPullRequestOpenAs::Draft
        );
        assert_eq!(
            SessionPullRequestPolicy::from_config_text("[broker.pr]\nopen_as = \"draft\"\n")
                .unwrap()
                .open_as,
            SessionPullRequestOpenAs::Draft
        );
        assert_eq!(
            SessionPullRequestPolicy::from_config_text("[broker.pr]\nopen_as = \"ready\"\n")
                .unwrap()
                .open_as,
            SessionPullRequestOpenAs::Ready
        );
    }

    #[test]
    fn automatic_watch_is_opt_in_and_boolean() {
        assert!(
            !SessionPullRequestPolicy::from_config_text("")
                .unwrap()
                .watch_on_start
        );
        assert!(
            SessionPullRequestPolicy::from_config_text("[broker.pr]\nwatch_on_start = true\n")
                .unwrap()
                .watch_on_start
        );
        assert!(
            SessionPullRequestPolicy::from_config_text("[broker.pr]\nwatch_on_start = \"true\"\n")
                .is_err()
        );
    }

    #[test]
    fn invalid_new_pull_request_mode_fails_closed() {
        for config in [
            "[broker.pr]\nopen_as = \"later\"\n",
            "[broker.pr]\nopen_as = true\n",
            "[broker.pr]\nother = \"value\"\n",
            "[broker.pr]\nopen_as = \"ready\"\nother = 1\n",
            "broker = false\n",
            "[broker.pr]\nopen_as = [\"ready\"]\n",
        ] {
            assert!(
                SessionPullRequestPolicy::from_config_text(config).is_err(),
                "accepted invalid config: {config}"
            );
        }
    }

    #[test]
    fn path_hint_needs_a_refused_push_not_any_not_found() {
        let hook = "env: node: No such file or directory\n\
                    error: failed to push some refs to 'https://example.invalid/r.git'";
        assert_eq!(
            missing_executable_path_hint("", hook),
            Some(PRE_PUSH_PATH_HINT)
        );
        for unrelated in [
            "HTTP 404: Not Found (https://api.github.com/repos/o/r/pulls/9)",
            "gh: Not Found (HTTP 404)",
            "fatal: could not read 'x': No such file or directory",
        ] {
            assert_eq!(
                missing_executable_path_hint("", unrelated),
                None,
                "{unrelated}"
            );
        }
        assert_eq!(
            missing_executable_path_hint("", "error: failed to push some refs to 'origin'"),
            None,
            "a refused push with no missing executable gets no hint"
        );
    }

    #[test]
    fn only_session_branches_are_publishable() {
        assert_eq!(branch_refusal("agent/fix-thing", "main"), None);
        for branch in [
            "main",
            "aethyme/integration",
            "v1.0.0",
            "feature/x",
            "refs/heads/agent/x",
        ] {
            assert!(
                branch_refusal(branch, "main").is_some(),
                "{branch} was allowed"
            );
        }
        assert!(branch_refusal("agent/", "main").is_some());
        assert!(branch_refusal("agent/../main", "main").is_some());
        assert!(branch_refusal("agent/x", "agent/x").is_some());
    }

    #[test]
    fn a_pull_request_listing_matches_the_exact_head_branch() {
        let listing = r#"[
            {"number":3,"url":"https://github.com/a/b/pull/3","state":"OPEN","headRefName":"agent/other"},
            {"number":7,"url":"https://github.com/a/b/pull/7","state":"OPEN","headRefName":"agent/mine"}
        ]"#;
        let found = parse_pull_request_list(listing, "agent/mine").unwrap();
        assert_eq!(found.number, 7);
        assert!(!found.created);
        assert!(parse_complete_pull_request_list(listing, "agent/mine", false).is_ok());
        assert!(
            parse_complete_pull_request_list(listing, "agent/mine", true).is_err(),
            "a valid JSON prefix must not be treated as a complete absence/presence check"
        );
        assert_eq!(parse_pull_request_list(listing, "agent/none"), None);
        assert_eq!(parse_pull_request_list("not json", "agent/mine"), None);
    }
}
