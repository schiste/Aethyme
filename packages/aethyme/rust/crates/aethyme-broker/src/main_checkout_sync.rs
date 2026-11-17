//! Keep the repository's primary checkout on its remote default branch.
//!
//! Agents and people read code in the primary checkout. On the busiest
//! repository it sat 4,716 commits behind its remote default branch for ten
//! days, because the only update attempt refused whenever the checkout had
//! any local change at all, and the broker's own leftovers under `.aethyme/`
//! counted as local changes.
//!
//! An attempt here moves the checkout only by fast-forward, only when it is on
//! the default branch with no commits of its own and no Git operation in
//! progress, and only when no path the update changes is also changed locally
//! (modified, staged, renamed, or an untracked file in the way). That overlap
//! is computed first so a refusal can name the paths, but Git stays the
//! arbiter: the update is a compare-and-swap of the branch ref followed by a
//! two-tree `read-tree -m -u`, which refuses rather than overwrite any local
//! change ([`GitRepo::fast_forward_checkout_preserving_local_changes`]).
//!
//! Attempts run from two places: before the everyday commands run in the
//! primary checkout (no fetch), and from the detached background worker on a
//! timer, which first fetches the default branch once. Every attempt records
//! its result in broker meta, which `status` turns into the
//! `main-checkout.behind` advice and the `main_checkout` field.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::git::GitRepo;

/// Broker meta key holding the last recorded [`MainCheckoutStatus`].
pub(crate) const STATE_META_KEY: &str = "main_checkout.sync_state";
/// Broker meta key: epoch ms at which the timer lane is next due.
pub(crate) const DUE_META_KEY: &str = "main_checkout.sync_due_at_ms";
/// Broker meta key: set by a launcher that claimed the timer slot, cleared
/// by the worker lane that serves it.
pub(crate) const REQUEST_META_KEY: &str = "main_checkout.sync_requested";
/// The lifecycle-lock lane name of the timer attempt.
const LIFECYCLE_LANE: &str = "main_checkout_sync";
/// Status advice id.
pub(crate) const ADVICE_ID: &str = "main-checkout.behind";
/// Blocking paths a record or advice names; the rest are counted.
pub(crate) const BLOCKING_PATH_LIMIT: usize = 10;
/// Longest wait between fetch attempts after repeated failures.
const FETCH_BACKOFF_MAX_MS: i64 = 6 * 60 * 60 * 1000;
/// Upper bound on the timer lane's one fetch.
const FETCH_BUDGET_MAX: Duration = Duration::from_secs(120);
/// `.aethyme/` entries one litter sweep inspects at most.
const LITTER_SCAN_LIMIT: usize = 512;
/// Files one maintenance-hold directory may hold to be swept.
const HOLD_FILE_LIMIT: usize = 64;
/// A staged GC lock file is stale only past this age.
const GC_LOCK_STAGING_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// A completed maintenance-hold directory is kept this long.
const MAINTENANCE_HOLD_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// `[main_checkout]` in `.aethyme/config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyncConfig {
    /// Whether attempts may move the checkout at all.
    pub(crate) auto_sync: bool,
    /// Minutes between timer attempts; 0 disables the timer.
    pub(crate) sync_interval_minutes: u64,
    /// Advice fires once the checkout is more than this many commits behind.
    pub(crate) behind_threshold_commits: u64,
    /// ...or behind at all and not synced for more than this many hours.
    pub(crate) behind_threshold_hours: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_sync: true,
            sync_interval_minutes: 15,
            behind_threshold_commits: 50,
            behind_threshold_hours: 24,
        }
    }
}

impl SyncConfig {
    fn interval_ms(self) -> Option<i64> {
        (self.auto_sync && self.sync_interval_minutes > 0).then(|| {
            i64::try_from(self.sync_interval_minutes.saturating_mul(60_000)).unwrap_or(i64::MAX)
        })
    }

    fn threshold_ms(self) -> i64 {
        i64::try_from(self.behind_threshold_hours.saturating_mul(3_600_000)).unwrap_or(i64::MAX)
    }
}

/// Read `[main_checkout]`. A missing file or table is the default; a key of
/// the wrong type keeps that key's default and is reported.
pub(crate) fn load_config(main_root: &Path) -> (SyncConfig, Option<String>) {
    let text = match aethyme_config::read_config_toml(
        &main_root.join(aethyme_config::CONFIG_DIR),
        "config",
    ) {
        Ok(Some((_, text))) => text,
        Ok(None) => return (SyncConfig::default(), None),
        Err(error) => return (SyncConfig::default(), Some(error.to_string())),
    };
    match toml::from_str::<toml::Value>(&text) {
        Ok(root) => parse_config(root.get("main_checkout")),
        Err(error) => (
            SyncConfig::default(),
            Some(format!("cannot parse .aethyme/config.toml: {error}")),
        ),
    }
}

fn parse_config(table: Option<&toml::Value>) -> (SyncConfig, Option<String>) {
    let mut config = SyncConfig::default();
    let Some(table) = table else {
        return (config, None);
    };
    let mut problems = Vec::new();
    let Some(table) = table.as_table() else {
        return (config, Some("[main_checkout] is not a table".into()));
    };
    if let Some(value) = table.get("auto_sync") {
        match value.as_bool() {
            Some(flag) => config.auto_sync = flag,
            None => problems.push("auto_sync must be true or false"),
        }
    }
    let mut integer = |key: &str, minimum: u64, slot: &mut u64, problem: &'static str| {
        if let Some(value) = table.get(key) {
            match value.as_integer().and_then(|n| u64::try_from(n).ok()) {
                Some(n) if n >= minimum => *slot = n,
                _ => problems.push(problem),
            }
        }
    };
    integer(
        "sync_interval_minutes",
        0,
        &mut config.sync_interval_minutes,
        "sync_interval_minutes must be a non-negative integer",
    );
    integer(
        "behind_threshold_commits",
        1,
        &mut config.behind_threshold_commits,
        "behind_threshold_commits must be a positive integer",
    );
    integer(
        "behind_threshold_hours",
        1,
        &mut config.behind_threshold_hours,
        "behind_threshold_hours must be a positive integer",
    );
    let problem =
        (!problems.is_empty()).then(|| format!("[main_checkout]: {}", problems.join("; ")));
    (config, problem)
}

/// Why an attempt left the checkout where it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// A path the update changes is also changed locally, or an untracked
    /// file is in its way.
    LocalChangesOverlap,
    /// The checkout is on a branch other than the default branch.
    NotOnDefaultBranch,
    /// HEAD is detached.
    Detached,
    /// The checkout has commits its upstream does not contain.
    LocalCommits,
    /// A merge, rebase, cherry-pick, revert or bisect is in progress, or
    /// another Git process holds the index lock.
    OperationInProgress,
    /// Sparse checkout, assume-unchanged or skip-worktree entries hide local
    /// state from the overlap check.
    HiddenLocalState,
    /// A live broker session works in the primary checkout.
    LiveSession,
    /// Git refused the update (the arbiter caught what the preflight missed).
    GitRefused,
    /// HEAD or the upstream moved during the preflight.
    ChangedDuringPreflight,
    /// The checkout could not be inspected.
    InspectionFailed,
    /// Fetching the default branch failed.
    FetchFailed,
    /// `[main_checkout] auto_sync = false`.
    Disabled,
}

impl SyncReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalChangesOverlap => "local_changes_overlap",
            Self::NotOnDefaultBranch => "not_on_default_branch",
            Self::Detached => "detached",
            Self::LocalCommits => "local_commits",
            Self::OperationInProgress => "operation_in_progress",
            Self::HiddenLocalState => "hidden_local_state",
            Self::LiveSession => "live_session",
            Self::GitRefused => "git_refused",
            Self::ChangedDuringPreflight => "changed_during_preflight",
            Self::InspectionFailed => "inspection_failed",
            Self::FetchFailed => "fetch_failed",
            Self::Disabled => "disabled",
        }
    }

    /// The phase counter recording one held attempt for this reason.
    fn counter(self) -> &'static str {
        match self {
            Self::LocalChangesOverlap => "main_checkout_sync_held_local_changes_overlap",
            Self::NotOnDefaultBranch => "main_checkout_sync_held_not_on_default_branch",
            Self::Detached => "main_checkout_sync_held_detached",
            Self::LocalCommits => "main_checkout_sync_held_local_commits",
            Self::OperationInProgress => "main_checkout_sync_held_operation_in_progress",
            Self::HiddenLocalState => "main_checkout_sync_held_hidden_local_state",
            Self::LiveSession => "main_checkout_sync_held_live_session",
            Self::GitRefused => "main_checkout_sync_held_git_refused",
            Self::ChangedDuringPreflight => "main_checkout_sync_held_changed_during_preflight",
            Self::InspectionFailed => "main_checkout_sync_held_inspection_failed",
            Self::FetchFailed => "main_checkout_sync_held_fetch_failed",
            Self::Disabled => "main_checkout_sync_held_disabled",
        }
    }
}

/// One refusal: why, a sentence for people, the command that shows more, and
/// the paths in the way (overlaps only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) reason: SyncReason,
    pub(crate) message: String,
    pub(crate) command: String,
    pub(crate) paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// Already at its upstream.
    Current,
    /// Fast-forwarded.
    Advanced {
        from: String,
        to: String,
        commits: u64,
    },
    /// Left where it was.
    Held(Hold),
    /// No upstream: nothing to follow, nothing to report.
    NoUpstream,
}

/// What an attempt saw, whatever its outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    pub(crate) branch: Option<String>,
    pub(crate) upstream_ref: Option<String>,
    pub(crate) head: Option<String>,
    pub(crate) upstream_head: Option<String>,
    pub(crate) behind: Option<u64>,
    pub(crate) ahead: Option<u64>,
    /// Local changes outside the broker's own files.
    pub(crate) local_changes: Option<u64>,
    /// Local entries recognised as the broker's own state and ignored.
    pub(crate) broker_paths_ignored: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) outcome: SyncOutcome,
    pub(crate) observed: Observed,
    pub(crate) overlap_check: Option<Duration>,
    pub(crate) fast_forward: Option<Duration>,
}

/// The broker's own state and leftovers under `.aethyme/`: never a local
/// change of the person or agent using the checkout. The declared runtime
/// paths (database, logs, locks, run state, worktrees, reviews, GC journal),
/// the GC journal archive directory, staged GC lock files and maintenance
/// hold directories. Not `config.toml`, `overrides/` or any other file a
/// person edits.
pub(crate) fn is_broker_owned_path(path: &str) -> bool {
    if crate::runtime_paths::is_broker_runtime_path(path) {
        return true;
    }
    let Some(rest) = path.strip_prefix(".aethyme/") else {
        return false;
    };
    let first = rest.split('/').next().unwrap_or(rest);
    first == "gc-journal-archives"
        || parse_gc_lock_staging_name(first).is_some()
        || (is_maintenance_hold_name(first) && rest.len() > first.len())
}

/// `(pid, created_at_nanos)` of a `.gc.lock.<pid>.<nanos>` staging name.
fn parse_gc_lock_staging_name(name: &str) -> Option<(u32, u128)> {
    let (pid, nanos) = name.strip_prefix(".gc.lock.")?.split_once('.')?;
    if pid.is_empty() || nanos.is_empty() {
        return None;
    }
    if !pid.bytes().all(|b| b.is_ascii_digit()) || !nanos.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((pid.parse().ok()?, nanos.parse().ok()?))
}

fn is_maintenance_hold_name(name: &str) -> bool {
    name.strip_prefix(".operator-")
        .and_then(|rest| rest.split_once("-maintenance-hold-"))
        .is_some_and(|(release, suffix)| !release.is_empty() && !suffix.is_empty())
}

/// Attempt one fast-forward of the primary checkout at `main_root`.
/// `allow_move = false` measures and records without moving (`auto_sync`
/// off). Never fetches.
pub(crate) fn attempt(checkout: &GitRepo, main_root: &Path, allow_move: bool) -> Attempt {
    let mut observed = Observed::default();
    let root_text = main_root.to_string_lossy().into_owned();
    let quoted_root = crate::broker::shell_quote(&root_text);
    let status_command = format!("git -C {quoted_root} status --short --branch");
    let held = |observed: Observed, reason, message: &str, command: &str| Attempt {
        outcome: SyncOutcome::Held(Hold {
            reason,
            message: message.to_string(),
            command: command.to_string(),
            paths: Vec::new(),
        }),
        observed,
        overlap_check: None,
        fast_forward: None,
    };

    let Ok(branch) = checkout.current_branch() else {
        return held(
            observed,
            SyncReason::InspectionFailed,
            "could not determine the current branch",
            &status_command,
        );
    };
    if branch == "HEAD" {
        return held(
            observed,
            SyncReason::Detached,
            "the checkout is detached",
            &status_command,
        );
    }
    observed.branch = Some(branch.clone());
    match checkout.in_progress_operation() {
        Ok(None) => {}
        Ok(Some(operation)) => {
            return held(
                observed,
                SyncReason::OperationInProgress,
                operation,
                &status_command,
            );
        }
        Err(_) => {
            return held(
                observed,
                SyncReason::InspectionFailed,
                "could not verify that no Git operation is in progress",
                &status_command,
            );
        }
    }
    // A session adopted on the primary checkout means an agent works in it:
    // moving HEAD under that agent is never this attempt's decision.
    match live_session_in_checkout(main_root) {
        Ok(None) => {}
        Ok(Some(session)) => {
            return held(
                observed,
                SyncReason::LiveSession,
                &format!("live broker session {session} works in this checkout"),
                &format!("aethyme broker status --session {session}"),
            );
        }
        Err(()) => {
            return held(
                observed,
                SyncReason::InspectionFailed,
                "could not verify that no live broker session works in this checkout",
                "aethyme broker status",
            );
        }
    }
    let Some((upstream_ref, upstream_commit)) = checkout.tracking_upstream() else {
        return Attempt {
            outcome: SyncOutcome::NoUpstream,
            observed,
            overlap_check: None,
            fast_forward: None,
        };
    };
    observed.upstream_ref = Some(upstream_ref.clone());
    observed.upstream_head = Some(upstream_commit.clone());
    let default_ref = checkout
        .upstream_default()
        .map_or_else(|| upstream_ref.clone(), |(reference, _)| reference);
    let default_branch = default_ref
        .split_once('/')
        .map_or(default_ref.as_str(), |(_, branch)| branch)
        .to_string();
    if default_ref != upstream_ref || branch != default_branch {
        return held(
            observed,
            SyncReason::NotOnDefaultBranch,
            &format!(
                "the checkout is on {branch} (tracking {upstream_ref}), not on the default branch {default_ref}"
            ),
            &format!(
                "git -C {quoted_root} switch {}",
                crate::broker::shell_quote(&default_branch)
            ),
        );
    }
    let Ok(head) = checkout.head_commit() else {
        return held(
            observed,
            SyncReason::InspectionFailed,
            "could not determine the checkout HEAD",
            &status_command,
        );
    };
    observed.head = Some(head.clone());
    if head == upstream_commit {
        observed.behind = Some(0);
        observed.ahead = Some(0);
        return Attempt {
            outcome: SyncOutcome::Current,
            observed,
            overlap_check: None,
            fast_forward: None,
        };
    }
    observed.behind = checkout.commit_count_between(&head, &upstream_commit).ok();
    observed.ahead = checkout.commit_count_between(&upstream_commit, &head).ok();
    if checkout.is_ancestor(&upstream_commit, &head) {
        let range = format!("{upstream_ref}..HEAD");
        return held(
            observed,
            SyncReason::LocalCommits,
            "the checkout has commits that its upstream does not contain",
            &format!(
                "git -C {quoted_root} log --oneline --decorate {}",
                crate::broker::shell_quote(&range)
            ),
        );
    }
    if !checkout.is_ancestor(&head, &upstream_commit) {
        let range = format!("HEAD...{upstream_ref}");
        return held(
            observed,
            SyncReason::LocalCommits,
            "the checkout and its upstream have diverged",
            &format!(
                "git -C {quoted_root} log --oneline --decorate --left-right {}",
                crate::broker::shell_quote(&range)
            ),
        );
    }
    let merge_command = format!(
        "git -C {quoted_root} merge --ff-only {}",
        crate::broker::shell_quote(&upstream_ref)
    );
    if !allow_move {
        return held(
            observed,
            SyncReason::Disabled,
            "automatic main-checkout sync is off ([main_checkout] auto_sync = false)",
            &merge_command,
        );
    }
    match (
        checkout.config_get("core.sparseCheckout").as_deref(),
        checkout.has_hidden_index_entries(),
    ) {
        (Some("true"), _) | (_, Ok(true)) => {
            return held(
                observed,
                SyncReason::HiddenLocalState,
                "sparse checkout, assume-unchanged or skip-worktree entries hide local state",
                &format!("git -C {quoted_root} ls-files -v"),
            );
        }
        (_, Err(_)) => {
            return held(
                observed,
                SyncReason::InspectionFailed,
                "could not list index entry flags",
                &status_command,
            );
        }
        _ => {}
    }

    let overlap_started = Instant::now();
    let overlap = {
        let _span = crate::phase_telemetry::span("main_checkout_overlap_check");
        local_overlap(checkout, &head, &upstream_commit)
    };
    let overlap_elapsed = overlap_started.elapsed();
    let overlap = match overlap {
        Ok(overlap) => overlap,
        Err(()) => {
            let mut attempt = held(
                observed,
                SyncReason::InspectionFailed,
                "could not list the update's paths or the local changes",
                &status_command,
            );
            attempt.overlap_check = Some(overlap_elapsed);
            return attempt;
        }
    };
    observed.local_changes = Some(overlap.local_changes);
    observed.broker_paths_ignored = Some(overlap.broker_paths_ignored);
    if !overlap.paths.is_empty() {
        let total = overlap.paths.len();
        let named = overlap
            .paths
            .iter()
            .take(BLOCKING_PATH_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let more = total.saturating_sub(named.len());
        let message = format!(
            "{total} locally changed path(s) would be changed by the update: {}{}",
            named.join(", "),
            if more > 0 {
                format!(" (and {more} more)")
            } else {
                String::new()
            }
        );
        return Attempt {
            outcome: SyncOutcome::Held(Hold {
                reason: SyncReason::LocalChangesOverlap,
                message,
                command: status_command,
                paths: overlap.paths,
            }),
            observed,
            overlap_check: Some(overlap_elapsed),
            fast_forward: None,
        };
    }

    // Recheck the branch and refs immediately before the update; the ref
    // compare-and-swap and Git's two-tree merge remain the final guards.
    let still_same = checkout.current_branch().ok().as_deref() == Some(branch.as_str())
        && checkout.head_commit().ok().as_deref() == Some(head.as_str())
        && checkout
            .tracking_upstream()
            .is_some_and(|(current_ref, current_commit)| {
                current_ref == upstream_ref && current_commit == upstream_commit
            });
    if !still_same {
        let mut attempt = held(
            observed,
            SyncReason::ChangedDuringPreflight,
            "the checkout changed during fast-forward preflight",
            &status_command,
        );
        attempt.overlap_check = Some(overlap_elapsed);
        return attempt;
    }
    let forward_started = Instant::now();
    let forwarded = {
        let _span = crate::phase_telemetry::span("main_checkout_fast_forward");
        checkout.fast_forward_checkout_preserving_local_changes(&branch, &head, &upstream_commit)
    };
    let forward_elapsed = forward_started.elapsed();
    if let Err(error) = forwarded {
        let first_line = error.to_string().lines().next().unwrap_or("").to_string();
        let mut attempt = held(
            observed,
            SyncReason::GitRefused,
            &format!(
                "Git refused to fast-forward without touching local changes (HEAD moved, a local change or untracked file is in the way, or the index was locked): {first_line}"
            ),
            &merge_command,
        );
        attempt.overlap_check = Some(overlap_elapsed);
        attempt.fast_forward = Some(forward_elapsed);
        return attempt;
    }
    let commits = observed.behind.unwrap_or(0);
    observed.head = Some(upstream_commit.clone());
    observed.behind = Some(0);
    observed.ahead = Some(0);
    Attempt {
        outcome: SyncOutcome::Advanced {
            from: head,
            to: upstream_commit,
            commits,
        },
        observed,
        overlap_check: Some(overlap_elapsed),
        fast_forward: Some(forward_elapsed),
    }
}

struct Overlap {
    /// Sorted, deduplicated local paths in the update's way.
    paths: Vec<String>,
    local_changes: u64,
    broker_paths_ignored: u64,
}

/// Local paths the update from `from` to `to` would touch: a tracked path
/// changed both locally and by the update, or an untracked path at, above or
/// below a path the update writes. Compared case-insensitively, so a
/// collision a case-insensitive filesystem would see is never missed.
fn local_overlap(checkout: &GitRepo, from: &str, to: &str) -> Result<Overlap, ()> {
    let changed = checkout
        .changed_between_all_paths(from, to)
        .map_err(|_| ())?;
    let tracked = checkout.locally_changed_tracked_paths().map_err(|_| ())?;
    let untracked = checkout.untracked_paths_readonly().map_err(|_| ())?;
    let broker_paths_ignored = tracked
        .iter()
        .chain(untracked.iter())
        .filter(|path| is_broker_owned_path(path))
        .count() as u64;
    let local_changes = (tracked.len() + untracked.len()) as u64 - broker_paths_ignored;
    Ok(Overlap {
        paths: overlapping_paths(&changed, &tracked, &untracked),
        local_changes,
        broker_paths_ignored,
    })
}

fn fold(path: &str) -> String {
    path.to_lowercase()
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(index, _)| &path[..index])
}

fn overlapping_paths(changed: &[String], tracked: &[String], untracked: &[String]) -> Vec<String> {
    let changed_exact = changed
        .iter()
        .map(|path| fold(path))
        .collect::<std::collections::BTreeSet<_>>();
    let changed_dirs = changed
        .iter()
        .flat_map(|path| ancestors(path).map(fold).collect::<Vec<_>>())
        .collect::<std::collections::BTreeSet<_>>();
    let mut paths = std::collections::BTreeSet::new();
    for path in tracked {
        if changed_exact.contains(&fold(path)) {
            paths.insert(path.clone());
        }
    }
    for path in untracked {
        let folded = fold(path);
        // The update writes this path, writes a file where one of its parent
        // directories is, or needs this path as a directory.
        if changed_exact.contains(&folded)
            || changed_dirs.contains(&folded)
            || ancestors(&folded).any(|dir| changed_exact.contains(dir))
        {
            paths.insert(path.clone());
        }
    }
    paths.into_iter().collect()
}

/// The first non-closed broker session whose worktree is this primary
/// checkout. `Err` when the broker database exists but cannot be read: the
/// caller then cannot rule out an agent working here and must not move it.
fn live_session_in_checkout(main_root: &Path) -> Result<Option<i64>, ()> {
    let Ok(db) = crate::broker_db_path(main_root) else {
        return Err(());
    };
    if !db.exists() {
        return Ok(None);
    }
    let store = crate::BrokerStore::open_snapshot_in_repo(main_root).map_err(|_| ())?;
    let sessions = store.live_sessions().map_err(|_| ())?;
    let canonical_root = main_root.canonicalize().ok();
    Ok(sessions
        .into_iter()
        .filter(|session| !session.status.is_closed())
        .find(|session| {
            let path = Path::new(&session.worktree_path);
            path == main_root
                || path
                    .canonicalize()
                    .is_ok_and(|canonical| Some(canonical) == canonical_root)
        })
        .map(|session| session.id))
}

/// The primary checkout's sync state as `status --json` reports it
/// (`main_checkout`) and as the broker records it after every attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MainCheckoutStatus {
    pub branch: Option<String>,
    pub upstream_ref: Option<String>,
    pub head: Option<String>,
    pub upstream_head: Option<String>,
    /// Upstream commits the checkout lacks, as of the last attempt.
    pub behind: Option<u64>,
    /// Checkout commits the upstream lacks, as of the last attempt.
    pub ahead: Option<u64>,
    /// When an attempt last found the checkout at its upstream or moved it
    /// there (epoch ms).
    pub last_sync_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    /// `current`, `advanced` or `held`.
    pub last_result: Option<String>,
    /// Why the last attempt held the checkout.
    pub reason: Option<SyncReason>,
    pub detail: Option<String>,
    /// Up to ten paths in the update's way.
    #[serde(default)]
    pub blocking_paths: Vec<String>,
    #[serde(default)]
    pub blocking_paths_omitted: u64,
    /// The command that shows or resolves what held the checkout.
    pub next_command: Option<String>,
    /// What ran the last attempt: `broker.<command>` or `broker.worker`.
    pub trigger: Option<String>,
    /// The last successful fetch of the default branch by the timer lane.
    pub last_fetch_at: Option<i64>,
    #[serde(default)]
    pub fetch_failures: u32,
    pub fetch_error: Option<String>,
    /// When the current run of fetch failures began.
    pub fetch_failing_since: Option<i64>,
    /// No fetch is tried before this time after failures (backoff).
    pub next_fetch_at: Option<i64>,
    /// When the checkout was first seen behind since its last sync.
    pub behind_since: Option<i64>,
}

/// The recorded state, `None` when no attempt has been recorded.
pub(crate) fn load_state(store: &crate::BrokerStore) -> Option<MainCheckoutStatus> {
    store
        .meta_get(STATE_META_KEY)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn save_state(store: &crate::BrokerStore, state: &MainCheckoutStatus) {
    let saved = serde_json::to_string(state)
        .map_err(|error| error.to_string())
        .and_then(|text| {
            store
                .meta_set(STATE_META_KEY, &text)
                .map_err(|error| error.to_string())
        });
    crate::warn_unrecorded("record the main-checkout sync state", saved);
}

/// Fold one attempt into the recorded state.
pub(crate) fn apply_attempt(
    mut state: MainCheckoutStatus,
    attempt: &Attempt,
    trigger: &str,
    now_ms: i64,
) -> MainCheckoutStatus {
    let observed = &attempt.observed;
    state.branch = observed.branch.clone();
    state.upstream_ref = observed.upstream_ref.clone();
    state.head = observed.head.clone();
    state.upstream_head = observed.upstream_head.clone();
    state.behind = observed.behind;
    state.ahead = observed.ahead;
    state.last_attempt_at = Some(now_ms);
    state.trigger = Some(trigger.to_string());
    state.blocking_paths.clear();
    state.blocking_paths_omitted = 0;
    match &attempt.outcome {
        SyncOutcome::Current | SyncOutcome::Advanced { .. } => {
            let result = if matches!(attempt.outcome, SyncOutcome::Current) {
                "current"
            } else {
                "advanced"
            };
            state.last_result = Some(result.to_string());
            state.last_sync_at = Some(now_ms);
            state.reason = None;
            state.detail = None;
            state.next_command = None;
            state.behind_since = None;
        }
        SyncOutcome::Held(hold) => {
            state.last_result = Some("held".into());
            state.reason = Some(hold.reason);
            state.detail = Some(hold.message.clone());
            state.next_command = Some(hold.command.clone());
            state.blocking_paths = hold
                .paths
                .iter()
                .take(BLOCKING_PATH_LIMIT)
                .cloned()
                .collect();
            state.blocking_paths_omitted =
                hold.paths.len().saturating_sub(BLOCKING_PATH_LIMIT) as u64;
            if state.behind.unwrap_or(0) > 0 {
                state.behind_since.get_or_insert(now_ms);
            } else {
                state.behind_since = None;
            }
        }
        SyncOutcome::NoUpstream => {
            state.last_result = Some("no_upstream".into());
            state.reason = None;
            state.detail = None;
            state.next_command = None;
            state.behind_since = None;
        }
    }
    state
}

/// Record phase metrics for one attempt.
fn record_attempt_telemetry(attempt: &Attempt, trigger: &str, extra: serde_json::Value) {
    crate::phase_telemetry::add("main_checkout_sync_attempt", Duration::ZERO);
    let (result, reason, commits) = match &attempt.outcome {
        SyncOutcome::Current => ("current", None, 0),
        SyncOutcome::Advanced { commits, .. } => ("advanced", None, *commits),
        SyncOutcome::Held(hold) => ("held", Some(hold.reason), 0),
        SyncOutcome::NoUpstream => ("no_upstream", None, 0),
    };
    crate::phase_telemetry::add(
        match result {
            "current" => "main_checkout_sync_current",
            "advanced" => "main_checkout_sync_advanced",
            "no_upstream" => "main_checkout_sync_no_upstream",
            _ => reason.map_or("main_checkout_sync_held", SyncReason::counter),
        },
        Duration::ZERO,
    );
    let mut details = serde_json::json!({
        "trigger": trigger,
        "result": result,
        "reason": reason.map(SyncReason::as_str),
        "commits_advanced": commits,
        "behind": attempt.observed.behind,
        "local_changes": attempt.observed.local_changes,
        "broker_paths_ignored": attempt.observed.broker_paths_ignored,
        "overlap_check_ms": attempt.overlap_check.map(|d| d.as_millis() as u64),
        "fast_forward_ms": attempt.fast_forward.map(|d| d.as_millis() as u64),
    });
    if let (Some(details), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            details.insert(key.clone(), value.clone());
        }
    }
    crate::phase_telemetry::attach("main_checkout_sync", details);
}

/// The attempt the everyday commands make before they run in the primary
/// checkout: no fetch, no lock wait. Records the result when the broker
/// database exists and returns the attempt for the caller to report.
pub(crate) fn foreground_attempt(checkout: &GitRepo, main_root: &Path, trigger: &str) -> Attempt {
    let (config, _) = load_config(main_root);
    let attempt = attempt(checkout, main_root, config.auto_sync);
    record_attempt_telemetry(&attempt, trigger, serde_json::json!({}));
    let database_exists = crate::broker_db_path(main_root).is_ok_and(|db| db.exists());
    if database_exists && let Ok(store) = crate::BrokerStore::open_in_repo(main_root) {
        let state = apply_attempt(
            load_state(&store).unwrap_or_default(),
            &attempt,
            trigger,
            crate::clock::epoch_ms(),
        );
        save_state(&store, &state);
    }
    attempt
}

/// Claim the timer slot for a launcher: true at most once per interval per
/// repository (compare-and-set on the due time), and only while the timer
/// is enabled. The claim leaves a durable request the worker lane serves.
pub(crate) fn claim_timer_slot(store: &crate::BrokerStore, main_root: &Path, now_ms: i64) -> bool {
    let current = match store.meta_get(DUE_META_KEY) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if current
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|due| due > now_ms)
    {
        return false;
    }
    let (config, _) = load_config(main_root);
    let Some(interval) = config.interval_ms() else {
        return false;
    };
    let next = now_ms.saturating_add(interval).to_string();
    if !store
        .meta_compare_and_set(DUE_META_KEY, current.as_deref(), &next)
        .unwrap_or(false)
    {
        return false;
    }
    crate::warn_unrecorded(
        "request the main-checkout sync lane",
        store.meta_set(REQUEST_META_KEY, &now_ms.to_string()),
    );
    true
}

/// Take a pending timer request: true once per request.
fn take_timer_request(store: &crate::BrokerStore) -> bool {
    let Ok(Some(current)) = store.meta_get(REQUEST_META_KEY) else {
        return false;
    };
    if current.is_empty() {
        return false;
    }
    store
        .meta_compare_and_set(REQUEST_META_KEY, Some(&current), "")
        .unwrap_or(false)
}

fn fetch_backoff_ms(config: SyncConfig, failures: u32) -> i64 {
    let base = config.interval_ms().unwrap_or(15 * 60_000).max(60_000);
    base.saturating_mul(1_i64 << failures.min(10))
        .min(FETCH_BACKOFF_MAX_MS)
}

/// What one timer lane run did. The worker records it through phase
/// telemetry; the fields are read by the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LaneReport {
    pub(crate) ran: bool,
    pub(crate) fetch_attempted: bool,
    pub(crate) fetch_failed: bool,
    pub(crate) yielded_to_foreground: bool,
    pub(crate) outcome: Option<SyncOutcome>,
    pub(crate) litter: LitterSweep,
}

/// The detached worker's timer lane: when a launcher claimed the slot, sweep
/// the broker's own litter, fetch the default branch once (with backoff
/// after failures), then attempt the fast-forward under a nonblocking
/// lifecycle claim that yields to any queued foreground command. Runs on the
/// worker's own thread: Git deadlines are thread-local.
pub(crate) fn run_timer_lane(
    store: &crate::BrokerStore,
    main_root: &Path,
    now_ms: i64,
) -> LaneReport {
    let mut report = LaneReport::default();
    if !take_timer_request(store) {
        return report;
    }
    let (config, _) = load_config(main_root);
    if !config.auto_sync {
        return report;
    }
    report.ran = true;
    let litter_started = Instant::now();
    report.litter = {
        let _span = crate::phase_telemetry::span("main_checkout_litter_sweep");
        sweep_broker_litter(main_root, SystemTime::now())
    };
    let litter_ms = litter_started.elapsed().as_millis() as u64;
    let Ok(checkout) = GitRepo::discover(main_root) else {
        return report;
    };
    let mut state = load_state(store).unwrap_or_default();
    let mut fetch_ms = None;
    if let Some(default) = crate::session_push::tracked_default(&checkout)
        && state.next_fetch_at.is_none_or(|next| next <= now_ms)
    {
        report.fetch_attempted = true;
        let started = Instant::now();
        let fetched = {
            let _span = crate::phase_telemetry::span("main_checkout_fetch");
            checkout.fetch_branch_into_tracking_ref(
                &default.remote,
                &default.branch,
                crate::git::git_timeout().min(FETCH_BUDGET_MAX),
            )
        };
        fetch_ms = Some(started.elapsed().as_millis() as u64);
        match fetched {
            Ok(()) => {
                state.last_fetch_at = Some(now_ms);
                state.fetch_failures = 0;
                state.fetch_error = None;
                state.next_fetch_at = None;
                state.fetch_failing_since = None;
            }
            Err(error) => {
                report.fetch_failed = true;
                state.fetch_failures = state.fetch_failures.saturating_add(1);
                state.fetch_failing_since.get_or_insert(now_ms);
                state.fetch_error = Some(
                    error
                        .to_string()
                        .lines()
                        .next()
                        .unwrap_or("unknown error")
                        .chars()
                        .take(200)
                        .collect(),
                );
                state.next_fetch_at =
                    Some(now_ms.saturating_add(fetch_backoff_ms(config, state.fetch_failures)));
                crate::phase_telemetry::add("main_checkout_fetch_failed", Duration::ZERO);
            }
        }
    }
    let lock =
        match crate::file_lock::try_acquire_repository_lifecycle_lock(main_root, LIFECYCLE_LANE) {
            Ok(Some(lock)) => lock,
            Ok(None) | Err(_) => {
                // A foreground command waits or holds the lock: skip quietly and
                // keep the request for the next worker run.
                report.yielded_to_foreground = true;
                crate::phase_telemetry::add("main_checkout_sync_yielded", Duration::ZERO);
                crate::warn_unrecorded(
                    "keep the main-checkout sync request",
                    store.meta_set(REQUEST_META_KEY, &now_ms.to_string()),
                );
                save_state(store, &state);
                return report;
            }
        };
    let attempt = attempt(&checkout, main_root, true);
    drop(lock);
    record_attempt_telemetry(
        &attempt,
        "broker.worker",
        serde_json::json!({
            "fetch_ms": fetch_ms,
            "fetch_failed": report.fetch_failed,
            "litter_sweep_ms": litter_ms,
            "litter_swept": report.litter.swept,
            "litter_kept_by_reason": report.litter.kept_by_reason,
        }),
    );
    let state = apply_attempt(state, &attempt, "broker.worker", now_ms);
    save_state(store, &state);
    report.outcome = Some(attempt.outcome);
    report
}

/// Whether the recorded state calls for the `main-checkout.behind` advice,
/// and the reason class to show.
fn behind_verdict(
    state: &MainCheckoutStatus,
    config: SyncConfig,
    now_ms: i64,
) -> Option<&'static str> {
    let behind = state.behind.unwrap_or(0);
    let threshold = config.threshold_ms();
    let since = state.last_sync_at.or(state.behind_since);
    let stale = since.is_some_and(|at| now_ms.saturating_sub(at) > threshold);
    // A fetch failing since the last success (or since it began failing,
    // when none succeeded yet) for longer than the threshold.
    let fetch_stale = state.fetch_failures > 0
        && state
            .last_fetch_at
            .or(state.fetch_failing_since)
            .is_some_and(|at| now_ms.saturating_sub(at) > threshold);
    let over = behind > config.behind_threshold_commits || (behind > 0 && stale);
    if !over && !fetch_stale {
        return None;
    }
    Some(match state.reason {
        Some(reason) => reason.as_str(),
        None if fetch_stale => SyncReason::FetchFailed.as_str(),
        None if !config.auto_sync => SyncReason::Disabled.as_str(),
        None => "not_attempted",
    })
}

fn human_age(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    if minutes < 120 {
        format!("{minutes} min")
    } else if minutes < 48 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} d", minutes / (24 * 60))
    }
}

/// `main-checkout.behind`: the primary checkout is behind its default branch
/// by more than the configured threshold, or the timer lane's fetch has been
/// failing that long. Read from the recorded state only (no Git).
pub(crate) fn status_advice(
    store: &crate::BrokerStore,
    main_root: &Path,
    now_ms: i64,
) -> Option<crate::StatusAdvice> {
    let state = load_state(store)?;
    let (config, config_problem) = load_config(main_root);
    let reason = behind_verdict(&state, config, now_ms)?;
    let root_text = main_root.to_string_lossy().into_owned();
    let quoted_root = crate::broker::shell_quote(&root_text);
    let upstream = state
        .upstream_ref
        .clone()
        .unwrap_or_else(|| "its upstream".into());
    let behind = state.behind.unwrap_or(0);
    let since = state
        .last_sync_at
        .map(|at| format!("last synced {} ago", human_age(now_ms - at)))
        .unwrap_or_else(|| "never synced by the broker".into());
    let path_count = state.blocking_paths.len() as u64 + state.blocking_paths_omitted;
    let summary = format!(
        "main checkout is {behind} commit(s) behind {upstream} ({since}); held: {reason}{}. Read code in your session worktree, not the main checkout",
        if path_count > 0 {
            format!(
                " by {path_count} path(s): {}{}",
                state.blocking_paths.join(", "),
                if state.blocking_paths_omitted > 0 {
                    format!(" (and {} more)", state.blocking_paths_omitted)
                } else {
                    String::new()
                }
            )
        } else {
            String::new()
        }
    );
    let mut evidence = vec![
        format!("checkout: {root_text}"),
        format!("branch: {}", state.branch.as_deref().unwrap_or("unknown")),
        format!(
            "behind: {behind}; ahead: {}",
            state.ahead.map_or("unknown".into(), |n| n.to_string())
        ),
        format!("reason: {reason}"),
    ];
    if let Some(at) = state.last_sync_at {
        evidence.push(format!("last_sync_at: {at}"));
    }
    if let Some(at) = state.last_attempt_at {
        evidence.push(format!(
            "last_attempt_at: {at} ({})",
            state.trigger.as_deref().unwrap_or("unknown trigger")
        ));
    }
    if let Some(detail) = &state.detail {
        evidence.push(format!("detail: {detail}"));
    }
    for path in &state.blocking_paths {
        evidence.push(format!("blocking path: {path}"));
    }
    if state.blocking_paths_omitted > 0 {
        evidence.push(format!(
            "blocking paths not listed: {}",
            state.blocking_paths_omitted
        ));
    }
    if state.fetch_failures > 0 {
        evidence.push(format!(
            "fetch failures in a row: {}{}",
            state.fetch_failures,
            state
                .fetch_error
                .as_deref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        ));
    }
    if let Some(problem) = config_problem {
        evidence.push(format!("config: {problem}"));
    }
    let mut commands = Vec::new();
    if let Some(command) = &state.next_command {
        commands.push(command.clone());
    }
    match reason {
        "local_changes_overlap" if !state.blocking_paths.is_empty() => {
            commands.push(format!(
                "git -C {quoted_root} diff --stat -- {}",
                state
                    .blocking_paths
                    .iter()
                    .map(|path| crate::broker::shell_quote(path.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        "fetch_failed" => {
            if let Some((remote, branch)) = upstream.split_once('/') {
                commands.push(format!(
                    "git -C {quoted_root} fetch {} {}",
                    crate::broker::shell_quote(remote),
                    crate::broker::shell_quote(branch)
                ));
            }
        }
        "disabled" => {
            commands.push("set [main_checkout] auto_sync = true in .aethyme/config.toml".into())
        }
        _ => {}
    }
    commands.push("aethyme broker status --full --json".into());
    commands.dedup();
    Some(crate::StatusAdvice {
        id: ADVICE_ID,
        severity: crate::StatusAdviceSeverity::Warning,
        reason: "the primary checkout trails its remote default branch, so anything reading it sees stale code",
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands,
    })
}

/// The one line `broker start` prints when the primary checkout is behind by
/// more than the threshold. Measures the commit count fresh (start has just
/// fetched the default branch); the time since the last sync comes from the
/// recorded state.
pub(crate) fn start_reminder(
    store: &crate::BrokerStore,
    main_root: &Path,
    now_ms: i64,
) -> Option<String> {
    let checkout = GitRepo::discover(main_root).ok()?;
    let (upstream, upstream_commit) = checkout.upstream_default()?;
    let head = checkout.head_commit().ok()?;
    if head == upstream_commit {
        return None;
    }
    let behind = checkout
        .commit_count_between(&head, &upstream_commit)
        .ok()?;
    let (config, _) = load_config(main_root);
    let state = load_state(store).unwrap_or_default();
    let mut probe = state.clone();
    probe.behind = Some(behind);
    probe.fetch_failures = 0;
    behind_verdict(&probe, config, now_ms)?;
    Some(format!(
        "note: the main checkout {} is {behind} commit(s) behind {upstream}{}; read code in your session worktree, not the main checkout (details: aethyme broker status --full --json)",
        main_root.display(),
        state
            .last_sync_at
            .map(|at| format!(" (last synced {} ago)", human_age(now_ms - at)))
            .unwrap_or_default()
    ))
}

/// What one litter sweep removed and kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LitterSweep {
    pub(crate) swept: u64,
    /// Litter left in place, counted per reason.
    pub(crate) kept_by_reason: BTreeMap<&'static str, u64>,
    /// `.aethyme/`-relative names of what was left, for diagnostics.
    pub(crate) kept: Vec<String>,
}

impl LitterSweep {
    fn keep(&mut self, name: &str, reason: &'static str) {
        *self.kept_by_reason.entry(reason).or_default() += 1;
        if self.kept.len() < BLOCKING_PATH_LIMIT {
            self.kept.push(format!("{name}: {reason}"));
        }
    }
}

/// Count `n` occurrences of `name` in the command's phase metrics.
fn add_count(name: &'static str, n: u64) {
    for _ in 0..n.min(10_000) {
        crate::phase_telemetry::add(name, Duration::ZERO);
    }
}

fn age_of(metadata: &std::fs::Metadata, now: SystemTime) -> Option<Duration> {
    now.duration_since(metadata.modified().ok()?).ok()
}

/// Remove the broker's own leftovers from `<main_root>/.aethyme/`, bounded
/// and failing closed:
///
/// - `.gc.lock.<pid>.<nanos>`: the PID file `GcLock::acquire` stages and
///   hard-links into place, then removes. It leaks only when its process is
///   killed in between. Removed when older than an hour (by its name and its
///   modification time), its PID is dead or names a process that started
///   after the file was written (a reused PID), and `gc.lock` does not name
///   that same live process.
/// - `.operator-<release>-maintenance-hold-<n>-<hex>/`: a maintenance hold an
///   operator workflow leaves beside the broker's state. Removed only when
///   its `manifest.json` says `phase: complete` for this repository, it has
///   been complete for a day, and it holds nothing but that manifest and its
///   `item-<n>.json` records. No broker operation references these
///   directories, so a complete manifest is the release marker.
///
/// Anything else, any entry that is not a plain file or directory, and
/// anything that cannot be proven is left and reported.
pub(crate) fn sweep_broker_litter(main_root: &Path, now: SystemTime) -> LitterSweep {
    let mut sweep = LitterSweep::default();
    let state = main_root.join(".aethyme");
    let Ok(entries) = std::fs::read_dir(&state) else {
        return sweep;
    };
    let gc_lock_holder = std::fs::read_to_string(state.join("gc.lock"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    for entry in entries.flatten().take(LITTER_SCAN_LIMIT) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if let Some((pid, nanos)) = parse_gc_lock_staging_name(&name) {
            match judge_gc_lock_staging(&path, pid, nanos, gc_lock_holder, now) {
                Ok(()) => match std::fs::remove_file(&path) {
                    Ok(()) => sweep.swept += 1,
                    Err(_) => sweep.keep(&name, "remove_failed"),
                },
                Err(reason) => sweep.keep(&name, reason),
            }
        } else if is_maintenance_hold_name(&name) {
            match judge_maintenance_hold(&path, main_root, now) {
                Ok(files) => {
                    let removed = files.iter().all(|file| std::fs::remove_file(file).is_ok())
                        && std::fs::remove_dir(&path).is_ok();
                    if removed {
                        sweep.swept += 1;
                    } else {
                        sweep.keep(&name, "remove_failed");
                    }
                }
                Err(reason) => sweep.keep(&name, reason),
            }
        }
    }
    add_count("main_checkout_litter_swept", sweep.swept);
    for (reason, count) in &sweep.kept_by_reason {
        add_count(
            match *reason {
                "pid_alive" => "main_checkout_litter_kept_pid_alive",
                "too_young" => "main_checkout_litter_kept_too_young",
                "lock_held" => "main_checkout_litter_kept_lock_held",
                "hold_incomplete" => "main_checkout_litter_kept_hold_incomplete",
                "hold_recent" => "main_checkout_litter_kept_hold_recent",
                "unknown_content" => "main_checkout_litter_kept_unknown_content",
                "remove_failed" => "main_checkout_litter_kept_remove_failed",
                _ => "main_checkout_litter_kept_other",
            },
            *count,
        );
    }
    sweep
}

fn judge_gc_lock_staging(
    path: &Path,
    pid: u32,
    created_nanos: u128,
    gc_lock_holder: Option<u32>,
    now: SystemTime,
) -> Result<(), &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "unreadable")?;
    if !metadata.file_type().is_file() {
        return Err("not_a_file");
    }
    let created = SystemTime::UNIX_EPOCH
        + Duration::from_nanos(u64::try_from(created_nanos).map_err(|_| "unreadable")?);
    let named_age = now.duration_since(created).unwrap_or_default();
    let modified_age = age_of(&metadata, now).unwrap_or_default();
    if named_age <= GC_LOCK_STAGING_MAX_AGE || modified_age <= GC_LOCK_STAGING_MAX_AGE {
        return Err("too_young");
    }
    let alive = crate::broker::pid_alive(i64::from(pid));
    if alive && gc_lock_holder == Some(pid) {
        return Err("lock_held");
    }
    if alive {
        // A live PID is the creator unless the process provably started
        // after the file was written (a reused PID). Where the start time is
        // unknown, the live process is taken for the creator.
        let created_ms = i64::try_from(created_nanos / 1_000_000).unwrap_or(i64::MAX);
        let reused = i32::try_from(pid)
            .ok()
            .and_then(crate::gates::process_started_at_ms)
            .is_some_and(|started| started > created_ms);
        if !reused {
            return Err("pid_alive");
        }
    }
    Ok(())
}

/// The files to remove when the hold directory at `path` may go.
fn judge_maintenance_hold(
    path: &Path,
    main_root: &Path,
    now: SystemTime,
) -> Result<Vec<std::path::PathBuf>, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "unreadable")?;
    if !metadata.file_type().is_dir() {
        return Err("not_a_directory");
    }
    let mut files = Vec::new();
    let mut newest = Duration::MAX;
    let mut manifest = None;
    for entry in std::fs::read_dir(path).map_err(|_| "unreadable")? {
        let entry = entry.map_err(|_| "unreadable")?;
        if files.len() >= HOLD_FILE_LIMIT {
            return Err("unknown_content");
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_metadata = std::fs::symlink_metadata(entry.path()).map_err(|_| "unreadable")?;
        let is_item = name
            .strip_prefix("item-")
            .and_then(|rest| rest.strip_suffix(".json"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !file_metadata.file_type().is_file() || !(is_item || name == "manifest.json") {
            return Err("unknown_content");
        }
        newest = newest.min(age_of(&file_metadata, now).unwrap_or_default());
        if name == "manifest.json" {
            manifest = Some(entry.path());
        }
        files.push(entry.path());
    }
    newest = newest.min(age_of(&metadata, now).unwrap_or_default());
    let manifest = manifest.ok_or("hold_incomplete")?;
    let text = std::fs::read_to_string(&manifest).map_err(|_| "unreadable")?;
    let document: serde_json::Value = serde_json::from_str(&text).map_err(|_| "hold_incomplete")?;
    if document.get("phase").and_then(serde_json::Value::as_str) != Some("complete")
        || document
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        return Err("hold_incomplete");
    }
    if let Some(repository) = document
        .get("repository")
        .and_then(serde_json::Value::as_str)
    {
        let same = Path::new(repository).canonicalize().ok() == main_root.canonicalize().ok();
        if !same {
            return Err("foreign_repository");
        }
    }
    if newest <= MAINTENANCE_HOLD_RETENTION {
        return Err("hold_recent");
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
