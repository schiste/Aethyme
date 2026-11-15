//! Judging closed checkouts cheaply before the removal proofs run.
//!
//! Selection judges each closed-checkout group with several Git processes and
//! worktree walks. Two kinds of group never need them:
//!
//! - **Decided by the database.** A group one of whose sessions is inside the
//!   closed-worktree grace, or whose checkout an open session still uses, is
//!   kept whatever its files say. [`database_keep`] decides that from the
//!   session rows, the retention policy and the open-session path index the
//!   pass already resolved.
//! - **Remembered.** A group kept for a reason that only a change of its
//!   checkout or of the default branch can lift is kept again while nothing
//!   it depended on changed ([`keep_memo_entry_holds`]). The change signal
//!   is read from files, never by starting Git.
//!
//! Both only ever keep a checkout: a group they do not keep goes through
//! every proof as before, and apply re-proves every selected checkout under
//! its locks.
//!
//! The module also times each check kind of the judgment
//! ([`CheckKind`], [`take_check_timings`]) for the slice record.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    AutoCleanupCheckTimings, AutoCleanupCheckoutGroup, AutoKeptCheckout, CheckTiming,
    KEEP_MEMO_MAX_AGE_MS, KeepMemoEntry, LiveOverlapEvidence, ResolvedWorktreePath,
    first_live_worktree_overlap, keep_memo_fingerprint, overlap_resolution_error,
    registration_head,
};
use crate::types::Session;

/// One kind of check in judging a closed checkout, timed for the slice
/// record and the phase telemetry (`auto_cleanup_check_<kind>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckKind {
    /// The database-only keep ([`database_keep`]).
    Database,
    /// The remembered-keep check ([`keep_memo_entry_holds`]).
    Memo,
    /// The group inspection: Git discovery, the spawn witness, ownership.
    Inspect,
    /// Reading the fetched remote default branch tip, once per pass.
    Upstream,
    /// The verdict's session rows and retention policy.
    Sessions,
    /// The closed checkout cohort and its aliases.
    Cohort,
    /// The verdict's ownership re-check (Git discovery and ownership).
    Ownership,
    /// Open sessions' checkout paths.
    LivePaths,
    /// Lease liveness.
    Leases,
    /// Gate records.
    Gates,
    /// `git status` and the in-progress and stash checks.
    Status,
    /// The ignored-path listing and its classification.
    Ignored,
    /// HEAD and the containment proof (ancestry, then the landing search).
    Containment,
    /// Listing every ignored blocker of a kept checkout for the report.
    BlockerListing,
}

const CHECK_KINDS: usize = 14;

impl CheckKind {
    fn index(self) -> usize {
        self as usize
    }

    fn phase(self) -> &'static str {
        match self {
            Self::Database => "auto_cleanup_check_database",
            Self::Memo => "auto_cleanup_check_memo",
            Self::Inspect => "auto_cleanup_check_inspect",
            Self::Upstream => "auto_cleanup_check_upstream",
            Self::Sessions => "auto_cleanup_check_sessions",
            Self::Cohort => "auto_cleanup_check_cohort",
            Self::Ownership => "auto_cleanup_check_ownership",
            Self::LivePaths => "auto_cleanup_check_live_paths",
            Self::Leases => "auto_cleanup_check_leases",
            Self::Gates => "auto_cleanup_check_gates",
            Self::Status => "auto_cleanup_check_status",
            Self::Ignored => "auto_cleanup_check_ignored",
            Self::Containment => "auto_cleanup_check_containment",
            Self::BlockerListing => "auto_cleanup_check_blocker_listing",
        }
    }
}

std::thread_local! {
    /// Count and microseconds per [`CheckKind`] since the last
    /// [`take_check_timings`]. Selection and apply run on one thread.
    static CHECK_TALLY: std::cell::Cell<[(u64, u64); CHECK_KINDS]> =
        const { std::cell::Cell::new([(0, 0); CHECK_KINDS]) };
}

/// Count one `kind` check that took `elapsed`.
pub(crate) fn note_check(kind: CheckKind, elapsed: Duration) {
    crate::phase_telemetry::add(kind.phase(), elapsed);
    CHECK_TALLY.with(|cell| {
        let mut tally = cell.get();
        let entry = &mut tally[kind.index()];
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry
            .1
            .saturating_add(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX));
        cell.set(tally);
    });
}

/// Counts one `kind` check when dropped, however the check ended.
pub(crate) struct CheckTimer {
    kind: CheckKind,
    started: Instant,
}

impl Drop for CheckTimer {
    fn drop(&mut self) {
        note_check(self.kind, self.started.elapsed());
    }
}

/// Start timing one `kind` check; it is counted when the timer is dropped.
pub(crate) fn check_timer(kind: CheckKind) -> CheckTimer {
    CheckTimer {
        kind,
        started: Instant::now(),
    }
}

/// Run `check` and count it as one `kind` check.
pub(crate) fn timed<T>(kind: CheckKind, check: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let value = check();
    note_check(kind, started.elapsed());
    value
}

/// The check timings since the last call, which it resets.
pub(crate) fn take_check_timings() -> AutoCleanupCheckTimings {
    let tally = CHECK_TALLY.with(|cell| cell.replace([(0, 0); CHECK_KINDS]));
    let timing = |kind: CheckKind| {
        let (count, micros) = tally[kind.index()];
        CheckTiming {
            count,
            ms: micros / 1_000,
        }
    };
    AutoCleanupCheckTimings {
        database: timing(CheckKind::Database),
        memo: timing(CheckKind::Memo),
        inspect: timing(CheckKind::Inspect),
        upstream: timing(CheckKind::Upstream),
        sessions: timing(CheckKind::Sessions),
        cohort: timing(CheckKind::Cohort),
        ownership: timing(CheckKind::Ownership),
        live_paths: timing(CheckKind::LivePaths),
        leases: timing(CheckKind::Leases),
        gates: timing(CheckKind::Gates),
        status: timing(CheckKind::Status),
        ignored: timing(CheckKind::Ignored),
        containment: timing(CheckKind::Containment),
        blocker_listing: timing(CheckKind::BlockerListing),
    }
}

/// The keep the database alone decides for `group`, before any Git process
/// or worktree walk: an open session using its checkout (`session_open`, or
/// an overlap that could not be resolved), then a session inside the
/// closed-worktree grace (`in_grace`). The reasons are the ones the full
/// judgment gives, so the keep codes are unchanged. `None` when neither
/// applies; the group then goes through every proof.
pub(crate) fn database_keep(
    group: &AutoCleanupCheckoutGroup,
    live: &[Session],
    live_worktrees: &[ResolvedWorktreePath],
    retention: &crate::RetentionPolicy,
    now_ms: i64,
) -> Option<AutoKeptCheckout> {
    let keep = |reason: String| {
        let code = super::kept_reason_code(&reason).to_string();
        Some(AutoKeptCheckout {
            worktree: group.worktree.to_string_lossy().into_owned(),
            sessions: group.sessions.iter().map(|session| session.id).collect(),
            reason,
            code,
            ..Default::default()
        })
    };
    if !live_worktrees.is_empty() {
        let candidate = ResolvedWorktreePath::resolve(&group.worktree);
        if let Some((index, evidence)) = first_live_worktree_overlap(&candidate, live_worktrees)
            && let Some(open) = live.get(index)
        {
            return match evidence {
                LiveOverlapEvidence::Overlap => keep(format!(
                    "session {} is still open ({}) in this worktree",
                    open.id,
                    open.status.as_str()
                )),
                LiveOverlapEvidence::Unknown => keep(format!(
                    "session {} worktree overlap could not be resolved: {}",
                    open.id,
                    overlap_resolution_error(&candidate, &live_worktrees[index])
                )),
                LiveOverlapEvidence::Disjoint => None,
            };
        }
    }
    let in_grace = group
        .sessions
        .iter()
        .find(|session| retention.closed_worktree_in_grace(session, now_ms))?;
    keep(format!(
        "session {} is within the {} hour closed-worktree grace period",
        in_grace.id, retention.closed_worktree_grace_hours
    ))
}

/// Keep codes the memo may remember besides `ignored_content`, each with the
/// change signal that lifts it.
pub(crate) const MEMO_UNCOMMITTED: &str = "uncommitted_changes";
pub(crate) const MEMO_UNMARKED_DELETIONS: &str = "unmarked_tracked_deletions";
pub(crate) const MEMO_NOT_CONTAINED: &str = "not_contained";
pub(crate) const MEMO_IGNORED: &str = "ignored_content";

/// Whether a kept checkout with `code` may be remembered by its change
/// signal (`ignored_content` keeps its own blocker fingerprint).
pub(crate) fn memoised_code(code: &str) -> bool {
    matches!(
        code,
        MEMO_UNCOMMITTED | MEMO_UNMARKED_DELETIONS | MEMO_NOT_CONTAINED
    )
}

/// How long before the judgment started a fingerprinted path must have been
/// last modified for a keep to be remembered: a path that changed while it
/// was judged may already differ from what the judgment saw.
const MEMO_SETTLE_MS: i64 = 2_000;

#[cfg(test)]
std::thread_local! {
    /// Test seam: the settle time in ms, so a fixture written a moment ago
    /// can be remembered.
    pub(crate) static MEMO_SETTLE_OVERRIDE_MS: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
}

fn memo_settle_ms() -> i64 {
    #[cfg(test)]
    if let Some(settle) = MEMO_SETTLE_OVERRIDE_MS.with(std::cell::Cell::get) {
        return settle;
    }
    MEMO_SETTLE_MS
}

/// A linked checkout's Git administrative directory and common directory,
/// read from its `.git` file and `commondir`, without Git. `None` for a
/// checkout whose pointer is not a `gitdir:` file or does not read cleanly.
fn checkout_admin_dirs(worktree: &Path) -> Option<(PathBuf, PathBuf)> {
    let pointer = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let target = pointer.trim().strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let admin = worktree.join(target);
    let common = match std::fs::read_to_string(admin.join("commondir")) {
        Ok(text) => admin.join(text.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => admin.clone(),
        Err(_) => return None,
    };
    Some((admin, common))
}

/// The commit a checkout's HEAD names, read from its `HEAD` file and the
/// loose or packed branch ref, without Git. `None` whenever the files do not
/// say it plainly (another ref backend, a symbolic or malformed ref), which
/// only ever makes the memo miss.
pub(crate) fn checkout_head_from_files(worktree: &Path) -> Option<String> {
    let (admin, common) = checkout_admin_dirs(worktree)?;
    registration_head(&common, &admin).1
}

/// Modification time (ns) and length of `path`, `-` when it does not exist
/// or cannot be read. Never follows a link.
fn file_state(path: &Path) -> String {
    std::fs::symlink_metadata(path)
        .ok()
        .and_then(|metadata| {
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            Some(format!("{}:{}", modified.as_nanos(), metadata.len()))
        })
        .unwrap_or_else(|| "-".to_string())
}

/// What a checkout looked like from its files, cheaply: HEAD's commit, the
/// index, the checkout root and the exclude files that decide what is
/// untracked. Equal signals before and after a judgment mean none of these
/// moved while it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckoutSignal {
    head: String,
    parts: Vec<String>,
}

pub(crate) fn checkout_signal(worktree: &Path) -> Option<CheckoutSignal> {
    let (admin, common) = checkout_admin_dirs(worktree)?;
    let head = registration_head(&common, &admin).1?;
    Some(CheckoutSignal {
        head,
        parts: vec![
            format!("index={}", file_state(&admin.join("index"))),
            format!("root={}", file_state(worktree)),
            format!(".gitignore={}", file_state(&worktree.join(".gitignore"))),
            format!("info/exclude={}", file_state(&common.join("info/exclude"))),
        ],
    })
}

/// The first changed path a `uncommitted_changes` or
/// `unmarked_tracked_deletions` reason names (`..., first: <path>`).
fn changed_path_of(reason: &str) -> Option<&str> {
    reason
        .split_once(", first: ")
        .map(|(_, path)| path.trim_end_matches('/'))
        .filter(|path| !path.is_empty())
}

/// The paths around the first changed path a keep named: the path itself,
/// its directory, and that directory's `.gitignore`. For the checkout to
/// become clean that path must be committed (HEAD and the index move),
/// staged (the index moves), reverted or deleted (it or its directory
/// moves), or ignored (an exclude file moves).
fn changed_path_files(worktree: &Path, first: &str) -> [PathBuf; 3] {
    let parent = first.rsplit_once('/').map_or("", |(parent, _)| parent);
    let directory = if parent.is_empty() {
        worktree.to_path_buf()
    } else {
        worktree.join(parent)
    };
    let gitignore = directory.join(".gitignore");
    [worktree.join(first), directory, gitignore]
}

fn changed_path_parts(worktree: &Path, first: &str) -> Vec<String> {
    let [path, directory, gitignore] = changed_path_files(worktree, first);
    vec![
        format!("path={}", file_state(&path)),
        format!("directory={}", file_state(&directory)),
        format!("directory/.gitignore={}", file_state(&gitignore)),
    ]
}

/// Whether `path` was modified at or after `since_ms` less the settle time.
fn modified_since(path: &Path, since_ms: i64) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|modified| {
            i64::try_from(modified.as_millis()).unwrap_or(i64::MAX)
                >= since_ms.saturating_sub(memo_settle_ms())
        })
}

/// What a remembered keep is checked against: the group as the pass sees it.
pub(crate) struct MemoContext<'a> {
    pub(crate) worktree: &'a Path,
    pub(crate) session_ids: &'a [i64],
    pub(crate) policy: &'a str,
    /// The fetched remote default branch tip this pass proves against.
    pub(crate) upstream_tip: Option<&'a str>,
    /// Whether any automatic removal attempt is recorded for this path.
    pub(crate) removal_attempt_recorded: bool,
    pub(crate) now_ms: i64,
}

/// Whether the remembered keep `entry` still holds: recorded within the
/// re-proof age, for the same sessions and cleanup policy, and nothing its
/// code depends on changed:
///
/// - `ignored_content` (or an entry from before codes were recorded): HEAD
///   and the blocker fingerprint.
/// - `uncommitted_changes`: HEAD, the index, the checkout root, the exclude
///   files and the first changed path with its directory.
/// - `unmarked_tracked_deletions`: the same, and still no removal attempt
///   recorded for the path (one would make the deletions resumable).
/// - `not_contained`: HEAD and the remote default branch tip.
///
/// A hit only ever keeps the checkout; any other code never hits.
pub(crate) fn keep_memo_entry_holds(entry: &KeepMemoEntry, context: &MemoContext<'_>) -> bool {
    if context.now_ms.saturating_sub(entry.recorded_at_ms) >= KEEP_MEMO_MAX_AGE_MS
        || entry.policy != context.policy
        || entry.sessions != context.session_ids
    {
        return false;
    }
    match entry.code.as_str() {
        "" | MEMO_IGNORED => {
            !entry.blockers.is_empty()
                && keep_memo_fingerprint(context.worktree, &entry.blockers) == entry.fingerprint
                && checkout_head_from_files(context.worktree).as_deref()
                    == Some(entry.head.as_str())
        }
        MEMO_UNCOMMITTED | MEMO_UNMARKED_DELETIONS => {
            if entry.code == MEMO_UNMARKED_DELETIONS && context.removal_attempt_recorded {
                return false;
            }
            let Some(first) = changed_path_of(&entry.reason) else {
                return false;
            };
            let Some(signal) = checkout_signal(context.worktree) else {
                return false;
            };
            let mut fingerprint = signal.parts;
            fingerprint.extend(changed_path_parts(context.worktree, first));
            signal.head == entry.head && fingerprint == entry.fingerprint
        }
        MEMO_NOT_CONTAINED => {
            context
                .upstream_tip
                .is_some_and(|tip| entry.upstream.as_deref() == Some(tip))
                && checkout_head_from_files(context.worktree).as_deref()
                    == Some(entry.head.as_str())
        }
        _ => false,
    }
}

/// The memo entry for a checkout the judgment kept with `code` and
/// `reason`, or `None` when it must not be remembered: a code without a
/// change signal, a checkout whose signal moved between `before` (taken
/// before the judgment started at `judged_from_ms`) and now, a changed path
/// modified around the judgment, a removal attempt on record, or no
/// remote default branch tip.
pub(crate) fn keep_memo_entry_for(
    code: &str,
    reason: &str,
    context: &MemoContext<'_>,
    before: Option<&CheckoutSignal>,
    judged_from_ms: i64,
) -> Option<KeepMemoEntry> {
    if !memoised_code(code) {
        return None;
    }
    let before = before?;
    if checkout_signal(context.worktree).as_ref() != Some(before) {
        return None;
    }
    let (fingerprint, upstream) = match code {
        MEMO_NOT_CONTAINED => (Vec::new(), Some(context.upstream_tip?.to_string())),
        _ => {
            if code == MEMO_UNMARKED_DELETIONS && context.removal_attempt_recorded {
                return None;
            }
            let first = changed_path_of(reason)?;
            if changed_path_files(context.worktree, first)
                .iter()
                .any(|path| modified_since(path, judged_from_ms))
            {
                return None;
            }
            let mut fingerprint = before.parts.clone();
            fingerprint.extend(changed_path_parts(context.worktree, first));
            (fingerprint, None)
        }
    };
    Some(KeepMemoEntry {
        head: before.head.clone(),
        policy: context.policy.to_string(),
        fingerprint,
        blockers: Vec::new(),
        sessions: context.session_ids.to_vec(),
        reason: reason.to_string(),
        recorded_at_ms: context.now_ms,
        code: code.to_string(),
        upstream,
    })
}

/// The kept entry a remembered keep reports.
pub(crate) fn remembered_kept(worktree: &str, entry: &KeepMemoEntry) -> AutoKeptCheckout {
    let code = if entry.code.is_empty() {
        MEMO_IGNORED.to_string()
    } else {
        entry.code.clone()
    };
    AutoKeptCheckout {
        worktree: worktree.to_string(),
        sessions: entry.sessions.clone(),
        reason: entry.reason.clone(),
        code,
        ignored_blockers: entry.blockers.clone(),
        ownership_reproof: None,
        gate_keep: None,
    }
}
