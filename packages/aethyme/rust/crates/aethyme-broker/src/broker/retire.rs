//! Retire a closed session's checkout once, at close: decide, rename, delete.
//!
//! A closed checkout used to wait for a removal lane that re-read every live
//! session and re-resolved every live path per page, kept it for any of
//! some fifteen reasons, paused on memory pressure, and only began after a
//! 24 hour grace. On a busy repository that removed one checkout per 45
//! minutes against dozens created.
//!
//! [`Broker::retire_closed_checkout`] decides once, with hard preconditions
//! only (a broker-owned checkout of a closed session that no open session
//! uses, holding no lease, running no gate or operation, used by no
//! process). A failed precondition keeps the checkout for the next run under
//! a named counter. Then:
//!
//! - a checkout whose commits are all on a delivery target, with no tracked
//!   change, no untracked file and no protected ignored file, is moved into
//!   the trash at once (its content is in Git; ignored leftovers are
//!   catalogued regenerable output, or the plan refuses);
//! - any other checkout gets a recovery archive first (bundle of the
//!   unlanded commits, staged and unstaged patches, untracked files,
//!   protected ignored files), written and read back by the existing
//!   archive writer, which then moves the checkout into the trash. An
//!   archive larger than [`RETIRE_ARCHIVE_MAX_BYTES`] keeps the checkout
//!   and is shown by `broker status` by session id.
//!
//! Moving into the trash is a `renameat(2)` into the pool's private broker
//! trash plus dropping the Git registration, only for a spawned session's
//! checkout inside a broker-owned pool (`checkout_trash`); the host deleter
//! later unlinks only the entries recorded here at rename time.
//! [`Broker::run_checkout_retirement`] is the worker lane: it walks closed
//! sessions that still have a checkout by session-id cursor, with the open
//! sessions' paths resolved once per run, then drains the trash.

use super::*;
use std::time::Instant;

/// Meta key: when the retirement lane is next due (epoch ms). Absent means
/// due, so a repository's existing leftovers are judged on the first run.
pub(crate) const CHECKOUT_RETIREMENT_DUE_KEY: &str = "checkout_retirement.due_at_ms";
/// Meta key: the last session id the backlog sweep judged.
const CHECKOUT_RETIREMENT_CURSOR_KEY: &str = "checkout_retirement.cursor";
/// Meta key: the trash entries this repository recorded when it moved a
/// checkout in (JSON: trash directory, then entry name, then device, inode,
/// session, failed deletions and whether the rename is still unconfirmed).
/// The deleter deletes nothing else. Every change is a compare-and-set.
pub(crate) const CHECKOUT_RETIREMENT_TRASH_ROWS_KEY: &str = "checkout_retirement.trash_rows";
/// Meta key: progress of the last runs, for `broker status` (JSON).
pub(crate) const CHECKOUT_RETIREMENT_PROGRESS_KEY: &str = "checkout_retirement.progress";
/// Meta key: sessions whose recovery archive would exceed the cap (JSON
/// array of session ids).
pub(crate) const CHECKOUT_RETIREMENT_TOO_LARGE_KEY: &str = "checkout_retirement.archive_too_large";
/// Meta key: sessions kept because a merge, rebase, cherry-pick, revert or
/// bisect is in progress or the index holds unmerged entries (JSON array).
pub(crate) const CHECKOUT_RETIREMENT_IN_PROGRESS_KEY: &str =
    "checkout_retirement.operation_in_progress";
/// Meta key: remembered keeps (JSON: session id to key, reason, time and
/// attempts), so a kept checkout is not re-planned every lap.
pub(crate) const CHECKOUT_RETIREMENT_KEEPS_KEY: &str = "checkout_retirement.keeps";
/// Meta key: sessions whose checkout the broker confirmed moved into the
/// trash (JSON array), kept after the entry is deleted until the session is
/// recorded as cleaned: the tombstone that lets a lost cleaned record be
/// reconciled.
pub(crate) const CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY: &str =
    "checkout_retirement.trashed_sessions";
/// Meta key: the last pin `prune_retired_pins` checked (its cursor).
const CHECKOUT_RETIREMENT_PIN_CURSOR_KEY: &str = "checkout_retirement.pin_cursor";
/// Pins checked for containment per run, at most.
#[cfg(not(test))]
const RETIRED_PIN_CHECKS_PER_RUN: usize = 64;
#[cfg(test)]
const RETIRED_PIN_CHECKS_PER_RUN: usize = 2;
/// The largest recovery archive retirement writes on its own.
pub(crate) const RETIRE_ARCHIVE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Closed sessions read per page.
const RETIRE_PAGE: usize = 64;
/// Next run when nothing is left to do.
const RETIRE_IDLE_RECHECK_MS: i64 = 10 * 60 * 1_000;
/// Next run while a lap or the trash is unfinished.
const RETIRE_BUSY_RECHECK_MS: i64 = 60 * 1_000;
/// Runs in a row without shrinking before `broker status` says so.
pub(crate) const RETIRE_STALLED_RUNS: u32 = 3;
/// A remembered keep is judged again after this long, by attempt: 1 h, then
/// 6 h, then every 24 h. A changed HEAD or index is judged at once.
const RETIRE_KEEP_BACKOFF_MS: [i64; 3] = [3_600_000, 6 * 3_600_000, 24 * 3_600_000];
/// Remembered keeps at most; the oldest go first.
const RETIRE_KEEPS_MAX: usize = 4_096;
/// The longest the fresh process-use observation before a rename may take.
const RETIRE_PROCESS_USE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
/// Inline retirement after an automatic close runs only with at least this
/// much of the caller's deadline left, and uses at most this much.
const RETIRE_INLINE_MIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
const RETIRE_INLINE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

thread_local! {
    /// Set while the retirement lane (or inline retirement) runs on this
    /// thread: a refused trash move then keeps the checkout instead of
    /// falling back to a synchronous `git worktree remove`.
    static RETIREMENT_LANE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the retirement lane runs on this thread.
pub(super) fn in_retirement_lane() -> bool {
    RETIREMENT_LANE.with(std::cell::Cell::get)
}

/// Marks this thread as the retirement lane until dropped.
struct RetirementLaneScope(bool);

impl RetirementLaneScope {
    fn enter() -> Self {
        Self(RETIREMENT_LANE.with(|cell| cell.replace(true)))
    }
}

impl Drop for RetirementLaneScope {
    fn drop(&mut self) {
        RETIREMENT_LANE.with(|cell| cell.set(self.0));
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: the archive cap.
    static RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    /// Test seam: runs after the plan, before the lifecycle claim.
    pub(super) static BEFORE_RETIRE_CLAIM: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Use `cap` as the archive cap on this test thread until the guard drops.
#[cfg(test)]
pub(crate) fn retire_archive_cap_for_test(cap: u64) -> RetireArchiveCapRestore {
    RetireArchiveCapRestore(RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(|cell| cell.replace(Some(cap))))
}

/// Restores the previous archive cap when dropped.
#[cfg(test)]
pub(crate) struct RetireArchiveCapRestore(Option<u64>);

#[cfg(test)]
impl Drop for RetireArchiveCapRestore {
    fn drop(&mut self) {
        RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(|cell| cell.set(self.0));
    }
}

fn retire_archive_max_bytes() -> u64 {
    #[cfg(test)]
    if let Some(cap) = RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(std::cell::Cell::get) {
        return cap;
    }
    RETIRE_ARCHIVE_MAX_BYTES
}

#[cfg(test)]
fn run_before_retire_claim() {
    BEFORE_RETIRE_CLAIM.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_retire_claim() {}

/// How one retirement attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetireOutcome {
    /// Its content is in Git: moved into the trash without an archive.
    RetiredClean,
    /// Archived, verified, then moved into the trash.
    RetiredArchived { bytes: u64 },
    /// Kept for now; the code names the precondition (counter
    /// `checkout_retirement_kept_<code>`).
    Kept(&'static str),
}

impl RetireOutcome {
    fn counter(self) -> &'static str {
        match self {
            Self::RetiredClean => "checkout_retirement_retired_clean",
            Self::RetiredArchived { .. } => "checkout_retirement_retired_archived",
            Self::Kept(code) => kept_counter(code),
        }
    }
}

/// The phase counter of one keep, by code.
fn kept_counter(code: &'static str) -> &'static str {
    match code {
        "not_closed" => "checkout_retirement_kept_not_closed",
        "adopted" => "checkout_retirement_kept_adopted",
        "checkout_missing" => "checkout_retirement_kept_checkout_missing",
        "not_owned" => "checkout_retirement_kept_not_owned",
        "ownership_unknown" => "checkout_retirement_kept_ownership_unknown",
        "open_session" => "checkout_retirement_kept_open_session",
        "open_session_unknown" => "checkout_retirement_kept_open_session_unknown",
        "lease_held" => "checkout_retirement_kept_lease_held",
        "lease_unknown" => "checkout_retirement_kept_lease_unknown",
        "gate_running" => "checkout_retirement_kept_gate_running",
        "gates_unknown" => "checkout_retirement_kept_gates_unknown",
        "operation_unresolved" => "checkout_retirement_kept_operation_unresolved",
        "operations_unknown" => "checkout_retirement_kept_operations_unknown",
        "operation_in_progress" => "checkout_retirement_kept_operation_in_progress",
        "operation_state_unknown" => "checkout_retirement_kept_operation_state_unknown",
        "process_using_checkout" => "checkout_retirement_kept_process_using_checkout",
        "process_use_unknown" => "checkout_retirement_kept_process_use_unknown",
        "plan_context_unavailable" => "checkout_retirement_kept_plan_context_unavailable",
        "plan_refused" => "checkout_retirement_kept_plan_refused",
        "lifecycle_busy" => "checkout_retirement_kept_lifecycle_busy",
        "changed" => "checkout_retirement_kept_changed",
        "trash_refused" => "checkout_retirement_kept_trash_refused",
        "archive_too_large" => "checkout_retirement_kept_archive_too_large",
        "archive_failed" => "checkout_retirement_kept_archive_failed",
        "archive_cleanup_failed" => "checkout_retirement_kept_archive_cleanup_failed",
        "deadline" => "checkout_retirement_kept_deadline",
        "deferred_to_lane" => "checkout_retirement_kept_deferred_to_lane",
        _ => "checkout_retirement_kept_other",
    }
}

/// Keeps about the checkout's content or ownership, which only a change to
/// its HEAD or index (or time) lifts: remembered, so the next lap does not
/// plan them again. Every other keep (a process, a lease, a gate, an open
/// session, contention, time) is asked again on the next run.
fn remembered_keep(code: &str) -> bool {
    matches!(
        code,
        "plan_refused"
            | "archive_too_large"
            | "archive_failed"
            | "archive_cleanup_failed"
            | "operation_in_progress"
            | "not_owned"
            | "adopted"
            | "trash_refused"
    )
}

/// One remembered keep.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetireKeep {
    /// HEAD and index modification time when it was kept.
    pub key: String,
    pub reason: String,
    pub at_ms: i64,
    pub attempts: u32,
}

/// Evidence one retirement run takes once and shares with every session it
/// judges: no per-session re-reading of the live sessions.
pub(crate) struct RetireRun {
    open_paths: Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>,
    process_table: crate::session_holder::RecordedHolderProcessTable,
    plan_context: Option<crate::cleanup_resolve::CleanupResolveRetirementContext>,
    deadline: Instant,
    /// Inline retirement at close: only the clean, no-archive path; anything
    /// needing an archive is left to the lane.
    inline: bool,
}

/// What one retirement run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckoutRetirementReport {
    pub examined: usize,
    pub retired_clean: usize,
    pub retired_archived: usize,
    pub archive_bytes: u64,
    pub kept: std::collections::BTreeMap<String, usize>,
    /// Kept checkouts not judged again this run (remembered keep, same key).
    pub keep_memo_skips: usize,
    /// Sessions whose checkout is in the trash, recorded as cleaned.
    pub reconciled_trashed: usize,
    pub lap_complete: bool,
    pub trash: crate::checkout_trash::TrashDrainReport,
    /// How many times the open sessions' paths were resolved (once per run).
    pub open_path_resolutions: usize,
}

/// Progress kept across runs for `broker status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutRetirementProgress {
    pub trash_pending: usize,
    pub kept: usize,
    pub stalled_runs: u32,
    pub reported_trash_entries: Vec<String>,
    #[serde(default)]
    pub unrecognized_trash_entries: Vec<String>,
    /// Refs under `refs/aethyme/retired/` still pinning reflog-only commits.
    #[serde(default)]
    pub retired_pins: usize,
    /// Pending trash entries over a day old nothing but `gc apply` confirms.
    #[serde(default)]
    pub stale_pending_trash_entries: Vec<String>,
}

/// Run Git in `dir`, bounded by the general Git limit, and return stdout.
fn run_git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = crate::git::git_command();
    command.args(args).current_dir(dir);
    match crate::bounded_output::output_limited(
        &mut command,
        crate::git::git_timeout(),
        64 * 1024 * 1024,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(_) => Err(format!("git {}: stopped at its limit", args.join(" "))),
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}

/// The key a remembered keep is bound to: the checkout's HEAD and index
/// modification time, read from its files.
fn checkout_keep_key(worktree: &Path) -> Option<String> {
    let git_dir = crate::git::checked_worktree_git_dir(worktree)
        .ok()
        .flatten()?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let index_mtime = std::fs::metadata(git_dir.join("index"))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    Some(format!("{}:{index_mtime}", head.trim()))
}

impl Broker {
    /// Ask for a retirement run as soon as the worker can take it: a
    /// checkout went into the trash, or a session closed.
    pub(crate) fn request_checkout_retirement_run(&self) {
        crate::warn_unrecorded(
            "request a checkout retirement run",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_DUE_KEY,
                &crate::clock::epoch_ms().to_string(),
            ),
        );
    }

    /// Whether the retirement lane is due at `now_ms`.
    pub(crate) fn checkout_retirement_due(&self, now_ms: i64) -> bool {
        match self.store_ref().meta_get(CHECKOUT_RETIREMENT_DUE_KEY) {
            Ok(Some(value)) => value.parse::<i64>().ok().is_none_or(|due| due <= now_ms),
            Ok(None) => true,
            Err(_) => false,
        }
    }

    fn meta_json<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.store_ref()
            .meta_get(key)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default()
    }

    /// Change the JSON value at `key` atomically: read, change, then
    /// compare-and-set, retried when another writer got in between.
    fn update_meta_json<T>(&self, key: &str, mut change: impl FnMut(&mut T)) -> Result<(), String>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        for _ in 0..32 {
            let current = self
                .store_ref()
                .meta_get(key)
                .map_err(|error| error.to_string())?;
            let mut value = current
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default();
            change(&mut value);
            let next = serde_json::to_string(&value).map_err(|error| error.to_string())?;
            if self
                .store_ref()
                .meta_compare_and_set(key, current.as_deref(), &next)
                .map_err(|error| error.to_string())?
            {
                return Ok(());
            }
        }
        Err(format!("{key} kept changing under concurrent writers"))
    }

    /// The trash entries this repository recorded.
    pub(crate) fn checkout_trash_rows(&self) -> crate::checkout_trash::TrashRows {
        self.meta_json(CHECKOUT_RETIREMENT_TRASH_ROWS_KEY)
    }

    /// Record a trash entry before the rename (pending).
    pub(crate) fn record_checkout_trash_row(
        &self,
        trash_dir: &Path,
        entry: &str,
        row: crate::checkout_trash::TrashRow,
    ) -> Result<(), String> {
        self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |rows: &mut crate::checkout_trash::TrashRows| {
                rows.entry(trash_dir.to_path_buf())
                    .or_default()
                    .insert(entry.to_string(), row);
            },
        )
    }

    /// Confirm a checkout just moved into the trash and ask for a run.
    pub(crate) fn note_checkout_trashed(&self, trashed: &crate::checkout_trash::TrashedCheckout) {
        crate::warn_unrecorded(
            "confirm a checkout trash entry",
            self.record_checkout_trash_row(&trashed.trash_dir, &trashed.entry, trashed.row),
        );
        self.mark_session_in_set(
            CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
            trashed.row.session_id,
            true,
        );
        self.request_checkout_retirement_run();
    }

    /// Whether the broker proved it moved `session_id`'s checkout into the
    /// trash: a confirmed row, or the tombstone left after its deletion.
    fn checkout_proven_trashed(&self, session_id: i64) -> bool {
        self.checkout_trash_rows()
            .values()
            .flat_map(|entries| entries.values())
            .any(|row| row.session_id == session_id && !row.pending)
            || self
                .meta_json::<std::collections::BTreeSet<i64>>(
                    CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                )
                .contains(&session_id)
    }

    /// Record `session_id` as cleaned after its checkout went into the trash;
    /// a failure leaves the tombstone, so the next run reconciles it.
    fn record_trashed_session_cleaned(&mut self, session_id: i64) -> bool {
        match self.mark_removed_checkout_sessions_cleaned(&[session_id], session_id) {
            Ok(()) => {
                self.mark_session_in_set(
                    CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                    session_id,
                    false,
                );
                true
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "record a retired checkout's session as cleaned",
                    Err(error),
                );
                false
            }
        }
    }

    /// Confirm pending trash rows nothing else can confirm (their creator's
    /// start time was unreadable) once they are a day old, when the entry
    /// still has its recorded identity and no open session uses the
    /// session's path: the explicit `gc apply` path. Returns how many.
    pub(crate) fn confirm_stale_pending_trash_rows(&self) -> usize {
        let now = crate::clock::epoch_ms();
        let mut eligible = Vec::new();
        for (dir, entries) in self.checkout_trash_rows() {
            let Some(pool) = dir.parent().map(Path::to_path_buf) else {
                continue;
            };
            for (name, row) in entries {
                if !crate::checkout_trash::stale_unconfirmable(&name, &row, now)
                    || !crate::checkout_trash::entry_matches(&pool, &name, &row)
                {
                    continue;
                }
                let Ok(session) = self.store_ref().session(row.session_id) else {
                    continue;
                };
                let open_elsewhere = self
                    .store_ref()
                    .other_open_session_at_path(row.session_id, &session.worktree_path)
                    .unwrap_or(true);
                if session.cleanup_state == crate::SessionCleanupState::Open || open_elsewhere {
                    continue;
                }
                eligible.push((dir.clone(), name, row));
            }
        }
        if eligible.is_empty() {
            return 0;
        }
        let applied = self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |rows: &mut crate::checkout_trash::TrashRows| {
                for (dir, name, row) in &eligible {
                    if let Some(stored) =
                        rows.get_mut(dir).and_then(|entries| entries.get_mut(name))
                        && stored.pending
                        && (stored.device, stored.inode) == (row.device, row.inode)
                    {
                        stored.pending = false;
                    }
                }
            },
        );
        match applied {
            Ok(()) => {
                for (_, _, row) in &eligible {
                    self.mark_session_in_set(
                        CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                        row.session_id,
                        true,
                    );
                }
                eligible.len()
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>("confirm stale trash entries", Err(error));
                0
            }
        }
    }

    /// Whether a trash row names `session_id`.
    fn checkout_in_trash(&self, session_id: i64) -> bool {
        self.checkout_trash_rows()
            .values()
            .flat_map(|entries| entries.values())
            .any(|row| row.session_id == session_id)
    }

    /// Progress of the last retirement runs.
    pub(crate) fn checkout_retirement_progress(&self) -> CheckoutRetirementProgress {
        self.meta_json(CHECKOUT_RETIREMENT_PROGRESS_KEY)
    }

    /// Sessions whose recovery archive would exceed the cap.
    pub(crate) fn checkout_retirement_too_large(&self) -> std::collections::BTreeSet<i64> {
        self.meta_json(CHECKOUT_RETIREMENT_TOO_LARGE_KEY)
    }

    /// Sessions kept for an operation in progress in their checkout.
    pub(crate) fn checkout_retirement_in_progress(&self) -> std::collections::BTreeSet<i64> {
        self.meta_json(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY)
    }

    fn mark_session_in_set(&self, key: &str, session_id: i64, member: bool) {
        let current: std::collections::BTreeSet<i64> = self.meta_json(key);
        if current.contains(&session_id) == member {
            return;
        }
        crate::warn_unrecorded(
            "remember a retirement keep by session",
            self.update_meta_json(key, |sessions: &mut std::collections::BTreeSet<i64>| {
                if member {
                    sessions.insert(session_id);
                } else {
                    sessions.remove(&session_id);
                }
                while sessions.len() > RETIRE_KEEPS_MAX {
                    sessions.pop_first();
                }
            }),
        );
    }

    /// The remembered keeps.
    pub(crate) fn checkout_retirement_keeps(&self) -> std::collections::BTreeMap<i64, RetireKeep> {
        self.meta_json(CHECKOUT_RETIREMENT_KEEPS_KEY)
    }

    /// Whether the checkout retirement lane owns `session_id`: it kept it on
    /// purpose or moved its checkout into the trash. The old removal lanes
    /// skip such sessions, so old and new never act on one checkout.
    pub(crate) fn checkout_retirement_owns(&self, session_id: i64) -> bool {
        self.checkout_retirement_keeps().contains_key(&session_id)
            || self.checkout_in_trash(session_id)
    }

    /// Whether a remembered keep still holds for `session` (same HEAD and
    /// index, backoff not elapsed).
    fn remembered_keep_holds(&self, session: &Session, now_ms: i64) -> bool {
        let Some(keep) = self.checkout_retirement_keeps().remove(&session.id) else {
            return false;
        };
        let backoff = RETIRE_KEEP_BACKOFF_MS[usize::try_from(keep.attempts.saturating_sub(1))
            .unwrap_or(2)
            .min(2)];
        checkout_keep_key(Path::new(&session.worktree_path)).as_deref() == Some(keep.key.as_str())
            && now_ms.saturating_sub(keep.at_ms) < backoff
    }

    fn remember_keep(&self, session: &Session, reason: &'static str) {
        let key = checkout_keep_key(Path::new(&session.worktree_path));
        crate::warn_unrecorded(
            "remember a checkout retirement keep",
            self.update_meta_json(
                CHECKOUT_RETIREMENT_KEEPS_KEY,
                |keeps: &mut std::collections::BTreeMap<i64, RetireKeep>| match key.as_ref() {
                    Some(key) => {
                        let attempts = keeps
                            .get(&session.id)
                            .filter(|keep| &keep.key == key && keep.reason == reason)
                            .map_or(1, |keep| keep.attempts.saturating_add(1));
                        keeps.insert(
                            session.id,
                            RetireKeep {
                                key: key.clone(),
                                reason: reason.to_string(),
                                at_ms: crate::clock::epoch_ms(),
                                attempts,
                            },
                        );
                        while keeps.len() > RETIRE_KEEPS_MAX {
                            let Some((&oldest, _)) =
                                keeps.iter().min_by_key(|(_, keep)| keep.at_ms)
                            else {
                                break;
                            };
                            keeps.remove(&oldest);
                        }
                    }
                    None => {
                        keeps.remove(&session.id);
                    }
                },
            ),
        );
    }

    fn forget_keep(&self, session_id: i64) {
        if self.checkout_retirement_keeps().contains_key(&session_id) {
            crate::warn_unrecorded(
                "forget a checkout retirement keep",
                self.update_meta_json(
                    CHECKOUT_RETIREMENT_KEEPS_KEY,
                    |keeps: &mut std::collections::BTreeMap<i64, RetireKeep>| {
                        keeps.remove(&session_id);
                    },
                ),
            );
        }
    }

    /// What must hold immediately before a checkout is moved into the trash,
    /// checked under the caller's claims: no merge, rebase, cherry-pick,
    /// revert or bisect in progress and no unmerged index entry (that state
    /// lives in the registration the move removes); a fresh process-use
    /// observation shows nobody in the checkout; and commits only the
    /// checkout's reflog reaches are pinned under
    /// `refs/aethyme/retired/<session>/`.
    pub(super) fn prepare_checkout_for_trash(
        &self,
        session: &Session,
        worktree: &Path,
    ) -> Result<(), BrokerOpError> {
        let refuse = |reason: String| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        };
        match GitRepo::discover(worktree).and_then(|checkout| checkout.operation_in_progress()) {
            Ok(None) => {}
            Ok(Some(_)) => {
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session.id, true);
                return Err(refuse(
                    "a merge, rebase, cherry-pick, revert or bisect is in progress or the index holds unmerged entries".into(),
                ));
            }
            Err(error) => return Err(refuse(format!("cannot read the operation state: {error}"))),
        }
        let mut fresh = crate::session_holder::RecordedHolderProcessTable::default();
        match fresh.process_use_snapshot_for_retirement(
            Path::new("/"),
            Some(Instant::now() + RETIRE_PROCESS_USE_BUDGET),
        ) {
            Some(snapshot) => {
                if let Some(reason) =
                    crate::reclaim::open_file_snapshot_blocker_for_worktree(snapshot, worktree)
                {
                    return Err(refuse(format!("a process uses the checkout: {reason}")));
                }
            }
            None => return Err(refuse("process use of the checkout is unknown".into())),
        }
        self.pin_reflog_only_commits(session.id, worktree)
            .map_err(|reason| refuse(format!("reflog commits could not be pinned: {reason}")))
    }

    /// Pin the commits only `worktree`'s reflogs reach (none of the
    /// repository's refs, remote-tracking ones included, holds them) under
    /// `refs/aethyme/retired/<session_id>/<commit>`, one ref per tip, so
    /// removing the worktree's registration and reflog loses no commit.
    fn pin_reflog_only_commits(&self, session_id: i64, worktree: &Path) -> Result<(), String> {
        let run = run_git_text;
        let listing = run(
            worktree,
            &["rev-list", "--parents", "--reflog", "--not", "--all"],
        )?;
        let mut commits = Vec::new();
        let mut parents = std::collections::BTreeSet::new();
        for line in listing.lines() {
            let mut ids = line.split_whitespace();
            if let Some(commit) = ids.next() {
                commits.push(commit.to_string());
                parents.extend(ids.map(str::to_string));
            }
        }
        for tip in commits.iter().filter(|commit| !parents.contains(*commit)) {
            if tip.is_empty() || !tip.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("unexpected commit id {tip:?}"));
            }
            run(
                self.main_root(),
                &[
                    "update-ref",
                    &format!("refs/aethyme/retired/{session_id}/{tip}"),
                    tip,
                ],
            )?;
            crate::phase_telemetry::add(
                "checkout_trash_reflog_commits_pinned",
                std::time::Duration::ZERO,
            );
        }
        Ok(())
    }

    /// Expire `refs/aethyme/retired/<session>/<commit>` pins: a pin goes once
    /// its commit is contained in a delivery target (the default branch or
    /// its remote-tracking ref) or another remote-tracking ref, which a
    /// session branch or scratch ref deleted later cannot undo; or once its
    /// session has been cleaned for `recovery_archive_days` (0 keeps pins
    /// forever). Never otherwise. At most `RETIRED_PIN_CHECKS_PER_RUN` pins
    /// are checked per run, from a cursor. Returns the pins left.
    pub(crate) fn prune_retired_pins(&self) -> Result<usize, String> {
        let root = self.main_root().to_path_buf();
        let listing = run_git_text(
            &root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/aethyme/retired/",
            ],
        )?;
        let pins = listing
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(refname, commit)| (refname.to_string(), commit.to_string()))
            .collect::<Vec<_>>();
        if pins.is_empty() {
            return Ok(0);
        }
        let mut targets = vec!["refs/remotes/".to_string()];
        if let Some((upstream_ref, _)) = self.repo_handle().upstream_default()
            && let Some((_, branch)) = upstream_ref
                .trim_start_matches("refs/remotes/")
                .split_once('/')
        {
            targets.push(format!("refs/heads/{branch}"));
        }
        let days = crate::load_retention_policy(&root)
            .map(|policy| policy.recovery_archive_days)
            .unwrap_or(30);
        let now = crate::clock::epoch_ms();
        let cursor = self
            .store_ref()
            .meta_get(CHECKOUT_RETIREMENT_PIN_CURSOR_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let start = pins
            .iter()
            .position(|(refname, _)| refname.as_str() > cursor.as_str())
            .unwrap_or(0);
        let mut removed = 0;
        let mut last = cursor.clone();
        for (refname, commit) in pins
            .iter()
            .cycle()
            .skip(start)
            .take(RETIRED_PIN_CHECKS_PER_RUN.min(pins.len()))
        {
            last.clone_from(refname);
            let mut args = vec![
                "for-each-ref",
                "--count=1",
                "--contains",
                commit.as_str(),
                "--format=%(refname)",
            ];
            args.extend(targets.iter().map(String::as_str));
            let contained = !run_git_text(&root, &args)?.trim().is_empty();
            let session_cleaned_long_ago = days > 0
                && refname
                    .strip_prefix("refs/aethyme/retired/")
                    .and_then(|rest| rest.split('/').next())
                    .and_then(|id| id.parse::<i64>().ok())
                    .and_then(|id| self.store_ref().session(id).ok())
                    .is_some_and(|session| {
                        session.cleanup_state == crate::SessionCleanupState::Cleaned
                            && session.cleanup_completed_at.is_some_and(|cleaned| {
                                now.saturating_sub(cleaned) > i64::from(days) * 86_400_000
                            })
                    });
            if contained || session_cleaned_long_ago {
                run_git_text(&root, &["update-ref", "-d", refname, commit])?;
                removed += 1;
                crate::phase_telemetry::add(
                    "checkout_retirement_pins_expired",
                    std::time::Duration::ZERO,
                );
            }
        }
        crate::warn_unrecorded(
            "advance the retired-pin cursor",
            self.store_ref()
                .meta_set(CHECKOUT_RETIREMENT_PIN_CURSOR_KEY, &last),
        );
        Ok(pins.len() - removed)
    }

    /// Take the evidence one retirement run shares: the open sessions'
    /// checkout paths, resolved once. `None` when they could not be resolved
    /// before `deadline`.
    pub(crate) fn begin_checkout_retirement_run(
        &self,
        deadline: Instant,
    ) -> Result<Option<RetireRun>, BrokerOpError> {
        let open = self
            .store_ref()
            .live_sessions()?
            .into_iter()
            .filter(|session| session.cleanup_state == crate::SessionCleanupState::Open)
            .collect::<Vec<_>>();
        let _span = crate::phase_telemetry::span("checkout_retirement_open_paths");
        let Some(paths) = crate::auto_cleanup::resolve_live_worktree_paths(&open, Some(deadline))
        else {
            return Ok(None);
        };
        Ok(Some(RetireRun {
            open_paths: open.iter().map(|session| session.id).zip(paths).collect(),
            process_table: crate::session_holder::RecordedHolderProcessTable::default(),
            plan_context: None,
            deadline,
            inline: false,
        }))
    }

    /// Retire the checkout of the closed session `session_id`, judged with
    /// the evidence of `run`. See the module documentation for the rules.
    pub(crate) fn retire_closed_checkout(
        &mut self,
        session_id: i64,
        run: &mut RetireRun,
    ) -> Result<RetireOutcome, BrokerOpError> {
        let _span = crate::phase_telemetry::span("checkout_retirement_judge");
        let _lane = RetirementLaneScope::enter();
        let outcome = self.retire_closed_checkout_inner(session_id, run)?;
        crate::phase_telemetry::add(outcome.counter(), std::time::Duration::ZERO);
        match outcome {
            RetireOutcome::Kept(code) if remembered_keep(code) => {
                if let Ok(session) = self.store_ref().session(session_id) {
                    self.remember_keep(&session, code);
                }
            }
            RetireOutcome::Kept(_) => {}
            RetireOutcome::RetiredClean | RetireOutcome::RetiredArchived { .. } => {
                self.forget_keep(session_id);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session_id, false);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session_id, false);
            }
        }
        Ok(outcome)
    }

    /// Retire `session_id`'s checkout right after an automatic close, when
    /// the caller has time left (a budgeted maintenance pass). Anything the
    /// inline attempt keeps is retried by the lane, which a close requests
    /// anyway.
    pub(crate) fn retire_inline_after_close(&mut self, session_id: i64, deadline: Option<Instant>) {
        let Some(deadline) = deadline else {
            return;
        };
        if deadline.saturating_duration_since(Instant::now()) < RETIRE_INLINE_MIN_BUDGET {
            return;
        }
        let inline_deadline = deadline.min(Instant::now() + RETIRE_INLINE_BUDGET);
        let outcome = self
            .begin_checkout_retirement_run(inline_deadline)
            .and_then(|run| match run {
                Some(mut run) => {
                    run.inline = true;
                    self.retire_closed_checkout(session_id, &mut run).map(Some)
                }
                None => Ok(None),
            });
        match outcome {
            Ok(Some(RetireOutcome::RetiredClean | RetireOutcome::RetiredArchived { .. })) => {
                crate::phase_telemetry::add(
                    "checkout_retirement_inline_retired",
                    std::time::Duration::ZERO,
                );
            }
            Ok(_) => {}
            Err(error) => {
                crate::warn_unrecorded::<(), _>("retire a checkout at close", Err(error));
            }
        }
    }

    fn retire_closed_checkout_inner(
        &mut self,
        session_id: i64,
        run: &mut RetireRun,
    ) -> Result<RetireOutcome, BrokerOpError> {
        use RetireOutcome::Kept;
        if Instant::now() >= run.deadline {
            return Ok(Kept("deadline"));
        }
        let _git_deadline = crate::git::limit_git_until(run.deadline);
        let session = self.store_ref().session(session_id)?;
        if !session.status.is_closed()
            || session.cleanup_state != crate::SessionCleanupState::Closed
        {
            return Ok(Kept("not_closed"));
        }
        if session.origin != crate::SessionOrigin::Spawned {
            return Ok(Kept("adopted"));
        }
        let worktree = PathBuf::from(&session.worktree_path);
        match std::fs::symlink_metadata(&worktree) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Ok(Kept("checkout_missing")),
        }
        match self.is_broker_owned_worktree_checked(&session, &worktree) {
            Ok(true) => {}
            Ok(false) => return Ok(Kept("not_owned")),
            Err(_) => return Ok(Kept("ownership_unknown")),
        }
        let candidate = crate::auto_cleanup::ResolvedWorktreePath::resolve(&worktree);
        for (open_id, open_path) in &run.open_paths {
            if *open_id == session_id {
                continue;
            }
            match crate::auto_cleanup::live_worktree_overlap(&candidate, open_path) {
                crate::auto_cleanup::LiveOverlapEvidence::Disjoint => {}
                crate::auto_cleanup::LiveOverlapEvidence::Overlap => {
                    return Ok(Kept("open_session"));
                }
                crate::auto_cleanup::LiveOverlapEvidence::Unknown => {
                    return Ok(Kept("open_session_unknown"));
                }
            }
        }
        match self
            .store_ref()
            .unreleased_unexpired_leases_for_sessions_at(&[session_id], crate::clock::epoch_ms())
        {
            Ok(leases) if leases.is_empty() => {}
            Ok(_) => return Ok(Kept("lease_held")),
            Err(_) => return Ok(Kept("lease_unknown")),
        }
        let deadline = run.deadline;
        match crate::gates::checked_running_session_gates_until(
            self.main_root(),
            session_id,
            || Instant::now() >= deadline,
            256,
        ) {
            Some(gates) if gates.is_empty() => {}
            Some(_) => return Ok(Kept("gate_running")),
            None => return Ok(Kept("gates_unknown")),
        }
        match self.session_operations_unresolved_since_boot(session_id) {
            Ok(false) => {}
            Ok(true) => return Ok(Kept("operation_unresolved")),
            Err(_) => return Ok(Kept("operations_unknown")),
        }
        // A merge, rebase, cherry-pick, revert or bisect in progress, or
        // unmerged index entries: that state lives in the registration and
        // is neither archived nor in Git. Asked again under the claim.
        match GitRepo::discover(&worktree).and_then(|checkout| checkout.operation_in_progress()) {
            Ok(None) => {}
            Ok(Some(_)) => {
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session_id, true);
                return Ok(Kept("operation_in_progress"));
            }
            Err(_) => return Ok(Kept("operation_state_unknown")),
        }
        match run
            .process_table
            .process_use_snapshot_for_retirement(Path::new("/"), Some(run.deadline))
        {
            Some(snapshot) => {
                if crate::reclaim::open_file_snapshot_blocker_for_worktree(snapshot, &worktree)
                    .is_some()
                {
                    return Ok(Kept("process_using_checkout"));
                }
            }
            None => return Ok(Kept("process_use_unknown")),
        }
        if run.plan_context.is_none() {
            match self.cleanup_resolve_retirement_context_until(Some(run.deadline)) {
                Ok(context) => run.plan_context = Some(context),
                Err(_) => return Ok(Kept("plan_context_unavailable")),
            }
        }
        let Some(context) = run.plan_context.as_ref() else {
            return Ok(Kept("plan_context_unavailable"));
        };
        let plan =
            match self.cleanup_resolve_retirement_plan_with_context(session_id, false, context) {
                Ok(plan) => plan,
                Err(_) => return Ok(Kept("plan_refused")),
            };
        let preserves = !plan.unlanded_commits.is_empty()
            || plan.staged_patch_bytes > 0
            || plan.unstaged_patch_bytes > 0
            || !plan.untracked_files.is_empty()
            || !plan.ignored_files.is_empty();
        if preserves && run.inline {
            // The archive writer is not bounded by a close's pass.
            return Ok(Kept("deferred_to_lane"));
        }
        run_before_retire_claim();
        if !preserves {
            // Everything is in Git: no archive. Re-check under the
            // lifecycle claim that nothing changed (ignored files included),
            // then move it away. The cleanup request's grace hours are
            // ignored on purpose: the work is in Git, so the directory gets
            // no grace.
            let Some(_claim) = self.try_claim_worktree_lifecycle("retire")? else {
                return Ok(Kept("lifecycle_busy"));
            };
            let current = self.store_ref().session(session_id)?;
            if !current.status.is_closed()
                || current.cleanup_state != crate::SessionCleanupState::Closed
                || current.worktree_path != session.worktree_path
            {
                return Ok(Kept("changed"));
            }
            let head = GitRepo::discover(&worktree)
                .ok()
                .and_then(|checkout| checkout.head_commit().ok());
            if head.as_deref() != Some(plan.head.as_str()) {
                return Ok(Kept("changed"));
            }
            let clean = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
                .ok()
                .and_then(|policy| {
                    crate::auto_cleanup::unclean_reason(
                        &worktree,
                        Some(current.branch.as_str()),
                        self.main_root(),
                        &policy,
                    )
                    .ok()
                })
                .is_some_and(|reason| reason.is_none());
            if !clean {
                return Ok(Kept("changed"));
            }
            match self.remove_checkout_through_trash(&current, &worktree, true) {
                Ok(()) if !worktree.exists() => {}
                Ok(()) => return Ok(Kept("trash_refused")),
                Err(_) => {
                    let in_progress = GitRepo::discover(&worktree)
                        .and_then(|checkout| checkout.operation_in_progress())
                        .is_ok_and(|state| state.is_some());
                    return Ok(Kept(if in_progress {
                        "operation_in_progress"
                    } else {
                        "trash_refused"
                    }));
                }
            }
            // The checkout is in the trash either way; a failed record is
            // reconciled by the next run from the tombstone.
            self.record_trashed_session_cleaned(session_id);
            return Ok(RetireOutcome::RetiredClean);
        }
        let bytes = plan
            .staged_patch_bytes
            .saturating_add(plan.unstaged_patch_bytes)
            .saturating_add(plan.untracked_bytes)
            .saturating_add(plan.ignored_bytes);
        if bytes > retire_archive_max_bytes() {
            self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session_id, true);
            return Ok(Kept("archive_too_large"));
        }
        let digest = plan.digest.clone();
        // The archive writer writes and reads back the archive, re-plans
        // under the lifecycle claim, and only then moves the checkout into
        // the trash (with the checks of `prepare_checkout_for_trash`); any
        // failure keeps the checkout and the archive.
        match self.cleanup_resolve_retirement_apply(session_id, &digest, false) {
            Ok(outcome) if outcome.worktree_removed => Ok(RetireOutcome::RetiredArchived { bytes }),
            Ok(_) => Ok(Kept("archive_cleanup_failed")),
            Err(_) => Ok(Kept("archive_failed")),
        }
    }

    /// The worker lane: judge closed sessions that still have a checkout,
    /// by session-id cursor, until `deadline` (two thirds of the time), then
    /// drain the trash with the rest.
    pub(crate) fn run_checkout_retirement(
        &mut self,
        deadline: Instant,
    ) -> Result<CheckoutRetirementReport, BrokerOpError> {
        let _span = crate::phase_telemetry::span("checkout_retirement_run");
        let _lane = RetirementLaneScope::enter();
        let started = Instant::now();
        let sweep_deadline = started + deadline.saturating_duration_since(started) * 2 / 3;
        let mut report = CheckoutRetirementReport::default();
        if let Some(mut run) = self.begin_checkout_retirement_run(sweep_deadline)? {
            report.open_path_resolutions = 1;
            let mut cursor = self
                .store_ref()
                .meta_get(CHECKOUT_RETIREMENT_CURSOR_KEY)
                .ok()
                .flatten()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            'lap: loop {
                if Instant::now() >= sweep_deadline {
                    break;
                }
                let page = self
                    .store_ref()
                    .closed_spawned_sessions_after(cursor, RETIRE_PAGE)?;
                if page.is_empty() {
                    cursor = 0;
                    report.lap_complete = true;
                    break;
                }
                for session in page {
                    if Instant::now() >= sweep_deadline {
                        break 'lap;
                    }
                    cursor = session.id;
                    if !Path::new(&session.worktree_path).exists() {
                        // Moved into the trash but the cleaned record was
                        // lost: record it now. Any other missing checkout
                        // is the missing-checkout lane's, which proves the
                        // absence first.
                        if self.checkout_proven_trashed(session.id)
                            && self.record_trashed_session_cleaned(session.id)
                        {
                            report.reconciled_trashed += 1;
                            crate::phase_telemetry::add(
                                "checkout_retirement_reconciled_trashed",
                                std::time::Duration::ZERO,
                            );
                        }
                        continue;
                    }
                    if self.remembered_keep_holds(&session, crate::clock::epoch_ms()) {
                        report.keep_memo_skips += 1;
                        crate::phase_telemetry::add(
                            "checkout_retirement_keep_memo_skips",
                            std::time::Duration::ZERO,
                        );
                        continue;
                    }
                    report.examined += 1;
                    match self.retire_closed_checkout(session.id, &mut run)? {
                        RetireOutcome::RetiredClean => report.retired_clean += 1,
                        RetireOutcome::RetiredArchived { bytes } => {
                            report.retired_archived += 1;
                            report.archive_bytes = report.archive_bytes.saturating_add(bytes);
                        }
                        RetireOutcome::Kept(code) => {
                            *report.kept.entry(code.to_string()).or_default() += 1;
                        }
                    }
                }
            }
            crate::warn_unrecorded(
                "advance the checkout retirement cursor",
                self.store_ref()
                    .meta_set(CHECKOUT_RETIREMENT_CURSOR_KEY, &cursor.to_string()),
            );
        } else {
            crate::phase_telemetry::add(
                "checkout_retirement_open_paths_unresolved",
                std::time::Duration::ZERO,
            );
        }
        report.trash = self.drain_checkout_trash(deadline);
        let retired_pins = match self.prune_retired_pins() {
            Ok(remaining) => remaining,
            Err(error) => {
                crate::warn_unrecorded::<(), _>("expire retired-commit pins", Err(error));
                self.checkout_retirement_progress().retired_pins
            }
        };
        let kept = report.kept.values().sum::<usize>();
        let previous = self.checkout_retirement_progress();
        let shrinking = report.retired_clean + report.retired_archived > 0
            || report.trash.entries_removed > 0
            || (report.trash.entries_pending == 0 && kept == 0);
        let progress = CheckoutRetirementProgress {
            trash_pending: report.trash.entries_pending,
            kept,
            stalled_runs: if shrinking {
                0
            } else {
                previous.stalled_runs.saturating_add(1)
            },
            reported_trash_entries: report.trash.reported.clone(),
            unrecognized_trash_entries: report.trash.unrecognized_names.clone(),
            retired_pins,
            stale_pending_trash_entries: report.trash.stale_pending.clone(),
        };
        crate::warn_unrecorded(
            "record checkout retirement progress",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_PROGRESS_KEY,
                &serde_json::to_string(&progress).unwrap_or_default(),
            ),
        );
        let busy = !report.lap_complete || report.trash.entries_pending > 0;
        let next = crate::clock::epoch_ms().saturating_add(if busy {
            RETIRE_BUSY_RECHECK_MS
        } else {
            RETIRE_IDLE_RECHECK_MS
        });
        crate::warn_unrecorded(
            "schedule the next checkout retirement run",
            self.store_ref()
                .meta_set(CHECKOUT_RETIREMENT_DUE_KEY, &next.to_string()),
        );
        Ok(report)
    }

    /// Drain this repository's recorded trash entries until `deadline`, as
    /// the one host-wide deleter (the lock lives in the host state
    /// directory). When that lock is held or cannot be taken, this run
    /// deletes nothing: there is never a second deleter.
    pub(crate) fn drain_checkout_trash(
        &self,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let lock_path = crate::host_state::resolve_host_state_dir()
            .map(|(dir, _)| dir.join(crate::checkout_trash::DELETER_LOCK_FILE));
        self.drain_checkout_trash_with_lock(lock_path.as_deref(), deadline)
    }

    pub(crate) fn drain_checkout_trash_with_lock(
        &self,
        lock_path: Option<&Path>,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let mut rows = self.checkout_trash_rows();
        let pending = rows.values().map(std::collections::BTreeMap::len).sum();
        let Some(first) = rows.keys().next().cloned() else {
            return crate::checkout_trash::TrashDrainReport::default();
        };
        let lock =
            lock_path.and_then(|path| crate::checkout_trash::try_deleter_lock(path).ok().flatten());
        let Some(_lock) = lock else {
            crate::phase_telemetry::add("checkout_trash_deleter_busy", std::time::Duration::ZERO);
            return crate::checkout_trash::TrashDrainReport {
                entries_pending: pending,
                ..crate::checkout_trash::TrashDrainReport::default()
            };
        };
        let before = rows.clone();
        let pace = crate::checkout_trash::DeleterPace::for_host(
            self.checkout_trash_below_headroom(&first),
            crate::checkout_trash::host_loaded(),
        );
        let mut report = crate::checkout_trash::drain_trash(&mut rows, deadline, pace);
        // Entries recorded by another process during the drain are kept:
        // only what this run removed, forgot, confirmed or counted is
        // applied, atomically.
        let applied = self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |current: &mut crate::checkout_trash::TrashRows| {
                for (dir, entries) in &before {
                    for name in entries.keys() {
                        match rows.get(dir).and_then(|after| after.get(name)) {
                            Some(row) => {
                                if let Some(stored) =
                                    current.get_mut(dir).and_then(|stored| stored.get_mut(name))
                                {
                                    stored.failures = row.failures;
                                    stored.pending = row.pending;
                                }
                            }
                            None => {
                                if let Some(stored) = current.get_mut(dir) {
                                    stored.remove(name);
                                }
                            }
                        }
                    }
                }
                current.retain(|_, entries| !entries.is_empty());
            },
        );
        crate::warn_unrecorded("record the checkout trash drain", applied);
        report.entries_pending = self
            .checkout_trash_rows()
            .values()
            .map(std::collections::BTreeMap::len)
            .sum();
        report
    }

    /// Whether the volume holding `trash` is below the artifact-sweep
    /// headroom (bytes or inodes): the deleter then runs unpaced.
    fn checkout_trash_below_headroom(&self, trash: &Path) -> bool {
        let required = crate::load_retention_policy(self.main_root())
            .map(|policy| policy.artifact_sweep_headroom_bytes)
            .unwrap_or(0)
            .max(crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES);
        let headroom =
            crate::disk_headroom::available_headroom_at_or_above_for(self.main_root(), trash);
        crate::disk_headroom::sweep_urgency_with_inodes(
            headroom.map(|value| value.bytes),
            required,
            headroom.and_then(|value| value.inodes),
            crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
        ) == crate::disk_headroom::SweepUrgency::Pressured
    }

    /// Recreate the checkout of a session whose checkout was retired, at its
    /// recorded `path`, from the session branch, so `start --reuse` can go on
    /// there. `Ok(false)` when no retired session names `path`. When a
    /// recovery archive holds leftovers of that session, nothing is created
    /// and the error names the archive and its restore steps.
    pub(crate) fn recreate_retired_checkout(&self, path: &Path) -> Result<bool, BrokerOpError> {
        let Some(session) = self
            .store_ref()
            .latest_session_for_worktree_any_state(&path.to_string_lossy())?
        else {
            return Ok(false);
        };
        if session.cleanup_state != crate::SessionCleanupState::Cleaned
            || session.origin != crate::SessionOrigin::Spawned
            || path.exists()
        {
            return Ok(false);
        }
        let refuse = |reason: String| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        };
        let Some(_claim) = self.try_claim_worktree_lifecycle("retire")? else {
            return Err(refuse(
                "another checkout lifecycle operation is running; retry in a moment".into(),
            ));
        };
        if let Ok(root) = self.recovery_archive_root()
            && let Ok(listing) = std::fs::read_dir(&root)
        {
            let prefix = format!("{}-", session.id);
            if let Some(archive) = listing
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|archive| {
                    archive
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(&prefix) && !name.ends_with(".partial")
                        })
                })
            {
                return Err(refuse(format!(
                    "the checkout was retired with a recovery archive; recreate it with `git -C {} worktree add {} {}` and restore its leftovers as {} describes",
                    self.main_root().display(),
                    path.display(),
                    session.branch,
                    archive.join("README.md").display()
                )));
            }
        }
        if self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{}", session.branch))
            .is_none()
        {
            return Err(refuse(format!(
                "the checkout was retired and its branch {} no longer exists",
                session.branch
            )));
        }
        let mut command = crate::git::git_command();
        command
            .args(["worktree", "add"])
            .arg(path)
            .arg(&session.branch)
            .current_dir(self.main_root());
        match crate::bounded_output::output_limited(
            &mut command,
            crate::git::git_timeout(),
            1024 * 1024,
        ) {
            Ok(crate::bounded_output::LimitedOutput::Completed(output))
                if output.status.success() =>
            {
                crate::phase_telemetry::add(
                    "checkout_retirement_recreated_for_reuse",
                    std::time::Duration::ZERO,
                );
                Ok(true)
            }
            Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(refuse(format!(
                "cannot recreate the retired checkout: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
            Ok(_) => Err(refuse(
                "cannot recreate the retired checkout in time".into(),
            )),
            Err(error) => Err(refuse(format!(
                "cannot recreate the retired checkout: {error}"
            ))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git_status(root: &Path, args: &[&str]) -> bool {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        command
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .success()
    }

    fn set_hook(hook: Option<Box<dyn FnMut()>>) {
        super::BEFORE_RETIRE_CLAIM.with(|cell| *cell.borrow_mut() = hook);
    }

    /// A repository with a local `origin` whose default branch is known, no
    /// closed-checkout grace, an ignore rule for a file nothing regenerates,
    /// and no process using any checkout.
    fn fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Retire test"]);
        git(&root, &["config", "user.email", "retire@example.invalid"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\nsecrets.local\n").unwrap();
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", ".gitignore", "base.txt"]);
        git(&root, &["commit", "-qm", "fixture"]);
        aethyme_testkit::ensure_origin(&root);
        git(&root, &["remote", "set-head", "origin", "main"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        (owner, storage, broker)
    }

    /// Start a session, let `setup` work in its checkout, then close it.
    fn closed(
        broker: &mut crate::Broker,
        label: &str,
        setup: impl FnOnce(&Path),
    ) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        setup(&worktree);
        broker
            .close_abandoning_unpushed(session.id, "retire fixture")
            .unwrap();
        (session, worktree)
    }

    fn run(broker: &mut crate::Broker) -> CheckoutRetirementReport {
        broker
            .run_checkout_retirement(Instant::now() + Duration::from_secs(600))
            .unwrap()
    }

    fn is_cleaned(broker: &crate::Broker, session_id: i64) -> bool {
        broker
            .store_ref()
            .session(session_id)
            .unwrap()
            .cleanup_state
            == crate::SessionCleanupState::Cleaned
    }

    /// The archives written for `session_id`.
    fn archives(broker: &crate::Broker, session_id: i64) -> Vec<PathBuf> {
        let root = broker.recovery_archive_root().unwrap();
        let Ok(listing) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        listing
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(&format!("{session_id}-"))
                                && !name.ends_with(".partial")
                        })
            })
            .collect()
    }

    /// A clean checkout whose commits are all on the default branch is
    /// retired without an archive: renamed into the trash, drained, the row
    /// cleaned and the branch ref kept.
    #[test]
    fn a_clean_contained_checkout_is_retired_without_an_archive() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "clean", |_| {});

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        assert!(
            archives(&broker, session.id).is_empty(),
            "no archive needed"
        );
        assert_eq!(report.trash.entries_pending, 0, "{report:#?}");
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", session.branch),
            ],
        );
    }

    /// Only a spawned session's checkout inside a broker-owned pool may go
    /// into a trash; a checkout anywhere else is removed by Git as before.
    #[test]
    fn a_checkout_outside_a_broker_pool_is_never_trashed() {
        let (owner, _storage, mut broker) = fixture();
        let session = broker.start_worktree("pooled", None).unwrap();
        let elsewhere = owner.path().join("elsewhere");
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "elsewhere",
                elsewhere.to_str().unwrap(),
            ],
        );
        let mut outside = session.clone();
        outside.worktree_path = elsewhere.to_string_lossy().into_owned();

        assert!(broker.trash_pool_for(&outside, &elsewhere).is_none());
        assert!(
            broker
                .trash_pool_for(&session, Path::new(&session.worktree_path))
                .is_some()
        );
        let mut adopted = session.clone();
        adopted.origin = crate::SessionOrigin::Adopted;
        assert!(
            broker
                .trash_pool_for(&adopted, Path::new(&session.worktree_path))
                .is_none()
        );
    }

    /// A dirty checkout (a tracked change, an untracked file, an ignored
    /// file nothing regenerates) is archived, the archive read back, and
    /// only then removed; the archive holds every file.
    #[test]
    fn a_dirty_checkout_is_archived_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "dirty", |worktree| {
            std::fs::write(worktree.join("base.txt"), "changed, not committed\n").unwrap();
            std::fs::write(worktree.join("notes.txt"), "untracked notes\n").unwrap();
            std::fs::write(worktree.join("secrets.local"), "only here\n").unwrap();
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        let archive = archives(&broker, session.id)
            .pop()
            .expect("a verified archive was kept");
        assert_eq!(
            std::fs::read_to_string(archive.join("untracked/notes.txt")).unwrap(),
            "untracked notes\n"
        );
        assert_eq!(
            std::fs::read_to_string(archive.join("ignored/secrets.local")).unwrap(),
            "only here\n"
        );
        let patch = std::fs::read_to_string(archive.join("unstaged.patch")).unwrap();
        assert!(patch.contains("+changed, not committed"), "{patch}");
    }

    /// A commit no delivery target holds is bundled; the bundle restores it.
    #[test]
    fn an_unlanded_commit_is_bundled_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let mut head = String::new();
        let (session, worktree) = closed(&mut broker, "unlanded", |worktree| {
            std::fs::write(worktree.join("work.txt"), "unlanded\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "unlanded work"]);
            head = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        let archive = archives(&broker, session.id).pop().expect("an archive");
        let bundle = archive.join("commits.bundle");
        let root = broker.main_root().to_path_buf();
        git(&root, &["bundle", "verify", bundle.to_str().unwrap()]);
        let heads = git(&root, &["bundle", "list-heads", bundle.to_str().unwrap()]);
        assert!(heads.contains(&head), "{heads}");
    }

    /// An archive over the cap keeps the checkout and is shown by session id.
    #[test]
    fn an_archive_over_the_cap_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "too large", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "more than one byte\n").unwrap();
        });
        let _cap = retire_archive_cap_for_test(1);

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("archive_too_large"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.join("notes.txt").exists(), "kept untouched");
        assert!(broker.checkout_retirement_too_large().contains(&session.id));
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        let advice = view
            .advice
            .iter()
            .find(|advice| advice.id == "cleanup.retire-archive-too-large")
            .expect("shown");
        assert_eq!(advice.session_id, Some(session.id));
        let shown = serde_json::to_string(advice).unwrap();
        assert!(!shown.contains(&session.worktree_path), "{shown}");
    }

    /// An open session's checkout is never touched, and a closed one a
    /// process still uses is kept under that precondition.
    #[test]
    fn open_or_used_checkouts_are_never_touched() {
        let (_owner, _storage, mut broker) = fixture();
        let open = broker.start_worktree("still open", None).unwrap();
        let (used, used_worktree) = closed(&mut broker, "in use", |_| {});
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "build".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&used_worktree).unwrap(),
            }]),
        ));

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("process_using_checkout"),
            Some(&1),
            "{report:#?}"
        );
        assert!(used_worktree.exists());
        assert!(!is_cleaned(&broker, used.id));
        assert!(Path::new(&open.worktree_path).join("base.txt").exists());
        assert_eq!(report.examined, 1, "the open session is never judged");
    }

    /// A backlog of closed checkouts is cleared in one run, with the open
    /// sessions' paths resolved once for the whole run.
    #[test]
    fn a_backlog_is_cleared_in_one_run_resolving_open_paths_once() {
        let (_owner, _storage, mut broker) = fixture();
        let _open = broker.start_worktree("open meanwhile", None).unwrap();
        let backlog = (0..5)
            .map(|index| closed(&mut broker, &format!("backlog {index}"), |_| {}))
            .collect::<Vec<_>>();
        let recorder = crate::phase_telemetry::Recorder::default();

        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            run(&mut broker)
        };

        assert_eq!(report.examined, 5, "{report:#?}");
        assert_eq!(report.retired_clean, 5, "{report:#?}");
        assert!(report.lap_complete);
        assert_eq!(report.open_path_resolutions, 1);
        assert_eq!(
            recorder
                .totals()
                .1
                .get("checkout_retirement_open_paths")
                .copied(),
            Some(1),
            "open paths resolved once per run, not per page or session"
        );
        for (session, worktree) in backlog {
            assert!(!worktree.exists());
            assert!(is_cleaned(&broker, session.id));
        }
    }

    /// A rebase stopped on conflicts keeps the checkout: its state lives in
    /// the registration and is neither in Git nor archived.
    #[test]
    fn a_checkout_mid_rebase_is_kept_untouched() {
        let (_owner, _storage, mut broker) = fixture();
        let root = broker.main_root().to_path_buf();
        let (session, worktree) = closed(&mut broker, "mid rebase", |worktree| {
            std::fs::write(worktree.join("base.txt"), "session side\n").unwrap();
            git(worktree, &["commit", "-qam", "session side"]);
            std::fs::write(root.join("base.txt"), "main side\n").unwrap();
            git(&root, &["commit", "-qam", "main side"]);
            assert!(
                !git_status(worktree, &["rebase", "main"]),
                "the rebase conflicts"
            );
        });

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("operation_in_progress"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.join("base.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(
            broker
                .checkout_retirement_in_progress()
                .contains(&session.id)
        );
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "cleanup.retire-operation-in-progress"
                    && advice.session_id == Some(session.id))
        );
    }

    /// An ignored file nothing regenerates, written after the plan, keeps
    /// the checkout: the re-check under the claim sees ignored files.
    #[test]
    fn an_ignored_file_written_after_the_plan_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "late ignored file", |_| {});
        let late = worktree.clone();
        set_hook(Some(Box::new(move || {
            std::fs::write(late.join("secrets.local"), "written late\n").unwrap();
        })));
        let report = run(&mut broker);
        set_hook(None);

        assert_eq!(report.kept.get("changed"), Some(&1), "{report:#?}");
        assert!(worktree.join("secrets.local").exists());
        assert!(!is_cleaned(&broker, session.id));
    }

    /// A process that starts using the checkout after the sweep's snapshot
    /// keeps it: the observation is taken again right before the rename.
    #[test]
    fn a_process_arriving_before_the_rename_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "late process", |_| {});
        let used = std::fs::canonicalize(&worktree).unwrap();
        set_hook(Some(Box::new(move || {
            crate::session_holder::set_process_use_snapshot_for_tests(Some(
                crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "build".into(),
                    descriptor: "cwd".into(),
                    file_type: Some("DIR".into()),
                    path: used.clone(),
                }]),
            ));
        })));
        let report = run(&mut broker);
        set_hook(None);

        assert_eq!(report.kept.get("trash_refused"), Some(&1), "{report:#?}");
        assert!(worktree.join("base.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
    }

    /// A worktree its user locked is kept, never removed synchronously by
    /// Git inside the lane.
    #[test]
    fn a_refused_trash_move_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "user locked", |_| {});
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "worktree",
                "lock",
                "--reason",
                "mine",
                worktree.to_str().unwrap(),
            ],
        );
        let report = run(&mut broker);
        assert_eq!(report.kept.get("trash_refused"), Some(&1), "{report:#?}");
        assert!(worktree.join("base.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(
            broker.checkout_retirement_owns(session.id),
            "the old lanes skip it"
        );
    }

    /// A commit only the checkout's reflog reaches is pinned under a ref
    /// before the registration (and its reflog) goes.
    #[test]
    fn a_reflog_only_commit_is_pinned_before_retirement() {
        let (_owner, _storage, mut broker) = fixture();
        let mut lost = String::new();
        let (session, worktree) = closed(&mut broker, "reflog only", |worktree| {
            std::fs::write(worktree.join("work.txt"), "only in the reflog\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "soon unreachable"]);
            lost = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            git(worktree, &["reset", "-q", "--hard", "HEAD~1"]);
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        let root = broker.main_root().to_path_buf();
        let pinned = git(
            &root,
            &[
                "rev-parse",
                &format!("refs/aethyme/retired/{}/{lost}", session.id),
            ],
        );
        assert_eq!(pinned.trim(), lost);
    }

    /// A kept checkout is not planned again every lap; a change to its
    /// index lifts the remembered keep at once.
    #[test]
    fn a_remembered_keep_skips_until_the_checkout_changes() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "remembered", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "more than one byte\n").unwrap();
        });
        let _cap = retire_archive_cap_for_test(1);
        let first = run(&mut broker);
        assert_eq!(first.kept.get("archive_too_large"), Some(&1), "{first:#?}");

        let second = run(&mut broker);
        assert_eq!(second.keep_memo_skips, 1, "{second:#?}");
        assert_eq!(second.examined, 0);

        git(&worktree, &["add", "notes.txt"]);
        let third = run(&mut broker);
        assert_eq!(third.keep_memo_skips, 0, "{third:#?}");
        assert_eq!(third.examined, 1);
        assert!(broker.checkout_retirement_owns(session.id));
    }

    /// A session whose checkout is in the trash but whose cleaned record was
    /// lost is recorded as cleaned, not skipped forever.
    #[test]
    fn a_trashed_session_missing_its_cleaned_record_is_reconciled() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "lost record", |_| {});
        std::fs::rename(&worktree, owner.path().join("gone")).unwrap();
        broker
            .record_checkout_trash_row(
                &owner.path().join("pool/.aethyme-checkout-trash"),
                "0000000000001-1-1-1",
                crate::checkout_trash::TrashRow {
                    device: 1,
                    inode: 1,
                    session_id: session.id,
                    failures: 0,
                    pending: false,
                    creator_pid: 0,
                    creator_started_ms: 0,
                },
            )
            .unwrap();
        let report = run(&mut broker);
        assert_eq!(report.reconciled_trashed, 1, "{report:#?}");
        assert!(is_cleaned(&broker, session.id));
    }

    /// The deleter never runs without the host-wide lock: held elsewhere,
    /// or unavailable, it deletes nothing (no weaker fallback lock).
    #[test]
    fn the_deleter_never_runs_without_the_host_lock() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "locked out", |_| {});
        let current = broker.store_ref().session(session.id).unwrap();
        let _lane = super::RetirementLaneScope::enter();
        broker
            .remove_checkout_through_trash(&current, &worktree, true)
            .unwrap();
        let lock = owner.path().join(crate::checkout_trash::DELETER_LOCK_FILE);
        let held = crate::checkout_trash::try_deleter_lock(&lock)
            .unwrap()
            .unwrap();
        let far = Instant::now() + Duration::from_secs(600);

        let busy = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        let unavailable = broker.drain_checkout_trash_with_lock(None, far);
        drop(held);
        let drained = broker.drain_checkout_trash_with_lock(Some(&lock), far);

        assert_eq!((busy.entries_removed, busy.entries_pending), (0, 1));
        assert_eq!(
            (unavailable.entries_removed, unavailable.entries_pending),
            (0, 1)
        );
        assert_eq!(drained.entries_removed, 1, "{drained:?}");
    }

    /// An automatic close with time left retires the checkout inline.
    #[test]
    fn a_close_with_time_left_retires_inline() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "inline", |_| {});
        broker
            .retire_inline_after_close(session.id, Some(Instant::now() + Duration::from_secs(600)));
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
    }

    /// `start --reuse` on a retired checkout recreates it from the session
    /// branch; one with archived leftovers is refused with the archive.
    #[test]
    fn a_retired_checkout_is_recreated_for_reuse() {
        let (_owner, _storage, mut broker) = fixture();
        let (clean, clean_path) = closed(&mut broker, "reuse clean", |_| {});
        let (dirty, dirty_path) = closed(&mut broker, "reuse dirty", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "leftover\n").unwrap();
        });
        let report = run(&mut broker);
        assert_eq!(
            report.retired_clean + report.retired_archived,
            2,
            "{report:#?}"
        );
        assert!(is_cleaned(&broker, clean.id) && is_cleaned(&broker, dirty.id));

        assert!(broker.recreate_retired_checkout(&clean_path).unwrap());
        assert!(clean_path.join("base.txt").exists());
        let error = broker
            .recreate_retired_checkout(&dirty_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("README.md"), "{error}");
        assert!(!dirty_path.exists());
    }

    /// Recreating a retired checkout takes the lifecycle claim: while
    /// another holder has it, nothing is created.
    #[test]
    fn recreating_a_retired_checkout_takes_the_lifecycle_claim() {
        let (_owner, _storage, mut broker) = fixture();
        let (_session, path) = closed(&mut broker, "claimed", |_| {});
        assert_eq!(run(&mut broker).retired_clean, 1);
        let held = broker
            .try_claim_worktree_lifecycle("retire")
            .unwrap()
            .unwrap();
        let error = broker
            .recreate_retired_checkout(&path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("lifecycle"), "{error}");
        assert!(!path.exists());
        drop(held);
        assert!(broker.recreate_retired_checkout(&path).unwrap());
    }

    /// A retired-commit pin expires once another ref holds its commit, is
    /// counted in status meanwhile, and is never pruned otherwise.
    #[test]
    fn retired_commit_pins_expire_only_when_held_elsewhere_or_old() {
        let (_owner, _storage, mut broker) = fixture();
        let mut lost = String::new();
        let (session, _) = closed(&mut broker, "pinned", |worktree| {
            std::fs::write(worktree.join("work.txt"), "only in the reflog\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "soon unreachable"]);
            lost = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            git(worktree, &["reset", "-q", "--hard", "HEAD~1"]);
        });
        assert_eq!(run(&mut broker).retired_clean, 1);
        let pin = format!("refs/aethyme/retired/{}/{lost}", session.id);
        let root = broker.main_root().to_path_buf();
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            1,
            "kept while only it holds the commit"
        );
        assert_eq!(broker.checkout_retirement_progress().retired_pins, 1);
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "cleanup.retired-pins")
        );

        // Another local branch (a session's, deleted later) is not enough.
        git(&root, &["branch", "keeps-it-for-now", &lost]);
        assert_eq!(broker.prune_retired_pins().unwrap(), 1);
        assert!(git_status(&root, &["rev-parse", "--verify", &pin]));
        // A remote-tracking ref holds it: the pin expires.
        git(
            &root,
            &["update-ref", "refs/remotes/origin/keeps-it", &lost],
        );
        assert_eq!(broker.prune_retired_pins().unwrap(), 0);
        assert!(!git_status(&root, &["rev-parse", "--verify", &pin]));
    }

    /// Inline retirement at close takes only the clean path: a checkout that
    /// needs an archive is left to the lane.
    #[test]
    fn inline_retirement_leaves_archives_to_the_lane() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "needs an archive", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "leftover\n").unwrap();
        });
        broker
            .retire_inline_after_close(session.id, Some(Instant::now() + Duration::from_secs(600)));
        assert!(worktree.join("notes.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(archives(&broker, session.id).is_empty());
        let report = run(&mut broker);
        assert_eq!(report.retired_archived, 1, "the lane archives it");
    }

    /// Pins are checked a bounded number per run, from a cursor: every pin
    /// is reached across runs.
    #[test]
    fn retired_pins_are_checked_in_bounded_batches() {
        let (_owner, _storage, broker) = fixture();
        let root = broker.main_root().to_path_buf();
        let base = git(&root, &["rev-parse", "origin/main"]).trim().to_string();
        for session in [901, 902, 903] {
            git(
                &root,
                &[
                    "update-ref",
                    &format!("refs/aethyme/retired/{session}/{base}"),
                    &base,
                ],
            );
        }
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            1,
            "two checked this run"
        );
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            0,
            "the cursor reaches the third"
        );
    }

    /// A pending row nothing could confirm (its mover's start time unknown)
    /// is confirmed by the explicit gc path once a day old, when its entry
    /// still matches and no open session uses the path; then it is drained.
    #[test]
    fn gc_confirms_a_stale_unconfirmable_trash_row() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "unconfirmable", |_| {});
        let pool = worktree.parent().unwrap().to_path_buf();
        let common = broker.main_git_common_dir().unwrap();
        let repo = crate::git::GitRepo::discover(broker.main_root()).unwrap();
        let mut pending = None;
        let trashed = crate::checkout_trash::move_worktree_to_trash(
            &repo,
            &common,
            &pool,
            &worktree,
            session.id,
            &mut |_, _, row| {
                pending = Some(row);
                Ok(())
            },
        )
        .unwrap();
        let mut row = pending.unwrap();
        row.creator_started_ms = 0;
        let old = format!("0000000000001-{}-1-1", session.id);
        std::fs::rename(trashed.path(), trashed.trash_dir.join(&old)).unwrap();
        broker
            .record_checkout_trash_row(&trashed.trash_dir, &old, row)
            .unwrap();

        let report = broker.drain_checkout_trash(Instant::now() + Duration::from_secs(600));
        assert_eq!(report.stale_pending, vec![old.clone()], "{report:?}");
        assert_eq!(report.entries_removed, 0);

        assert_eq!(broker.confirm_stale_pending_trash_rows(), 1);
        let report = broker.drain_checkout_trash(Instant::now() + Duration::from_secs(600));
        assert_eq!(report.entries_removed, 1, "{report:?}");
    }

    /// A session whose cleaned record was lost after its checkout was
    /// trashed and drained is reconciled from the tombstone.
    #[test]
    fn a_lost_cleaned_record_is_reconciled_after_the_drain() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "tombstone", |_| {});
        let first = run(&mut broker);
        assert_eq!(first.retired_clean, 1, "{first:#?}");
        assert!(!worktree.exists());
        assert!(broker.checkout_trash_rows().is_empty(), "drained");
        // The cleaned record is lost, as a database error would leave it.
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET cleanup_state = 'closed', cleanup_completed_at = NULL WHERE id = ?1",
                [session.id],
            )
            .unwrap();
        broker.mark_session_in_set(CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY, session.id, true);

        let second = run(&mut broker);
        assert_eq!(second.reconciled_trashed, 1, "{second:#?}");
        assert!(is_cleaned(&broker, session.id));
    }
}
