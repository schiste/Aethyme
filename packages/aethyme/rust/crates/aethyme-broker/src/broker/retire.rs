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
//! Moving into the trash is a `rename(2)` plus dropping the Git
//! registration (`checkout_trash`); the host deleter unlinks the tree later.
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
/// Meta key: the trash directories this repository has put checkouts into
/// (JSON array of paths).
const CHECKOUT_RETIREMENT_TRASH_DIRS_KEY: &str = "checkout_retirement.trash_dirs";
/// Meta key: progress of the last runs, for `broker status` (JSON).
pub(crate) const CHECKOUT_RETIREMENT_PROGRESS_KEY: &str = "checkout_retirement.progress";
/// Meta key: sessions whose recovery archive would exceed the cap (JSON
/// array of session ids).
pub(crate) const CHECKOUT_RETIREMENT_TOO_LARGE_KEY: &str = "checkout_retirement.archive_too_large";
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

#[cfg(test)]
thread_local! {
    /// Test seam: the archive cap.
    static RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
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
        "process_using_checkout" => "checkout_retirement_kept_process_using_checkout",
        "process_use_unknown" => "checkout_retirement_kept_process_use_unknown",
        "plan_context_unavailable" => "checkout_retirement_kept_plan_context_unavailable",
        "plan_refused" => "checkout_retirement_kept_plan_refused",
        "lifecycle_busy" => "checkout_retirement_kept_lifecycle_busy",
        "changed" => "checkout_retirement_kept_changed",
        "trash_failed" => "checkout_retirement_kept_trash_failed",
        "archive_too_large" => "checkout_retirement_kept_archive_too_large",
        "archive_failed" => "checkout_retirement_kept_archive_failed",
        "archive_cleanup_failed" => "checkout_retirement_kept_archive_cleanup_failed",
        "deadline" => "checkout_retirement_kept_deadline",
        _ => "checkout_retirement_kept_other",
    }
}

/// Evidence one retirement run takes once and shares with every session it
/// judges: no per-session re-reading of the live sessions.
pub(crate) struct RetireRun {
    open_paths: Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>,
    process_table: crate::session_holder::RecordedHolderProcessTable,
    plan_context: Option<crate::cleanup_resolve::CleanupResolveRetirementContext>,
    deadline: Instant,
}

/// What one retirement run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckoutRetirementReport {
    pub examined: usize,
    pub retired_clean: usize,
    pub retired_archived: usize,
    pub archive_bytes: u64,
    pub kept: std::collections::BTreeMap<String, usize>,
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
            Ok(Some(value)) => value.parse::<i64>().map_or(true, |due| due <= now_ms),
            Ok(None) => true,
            Err(_) => false,
        }
    }

    fn remember_trash_dir(&self, trash: &Path) {
        let mut dirs = self.checkout_retirement_trash_dirs();
        let trash = trash.to_path_buf();
        if !dirs.contains(&trash) {
            dirs.push(trash);
            crate::warn_unrecorded(
                "remember a checkout trash directory",
                self.store_ref().meta_set(
                    CHECKOUT_RETIREMENT_TRASH_DIRS_KEY,
                    &serde_json::to_string(&dirs).unwrap_or_default(),
                ),
            );
        }
    }

    /// The trash directories this repository has put checkouts into.
    pub(crate) fn checkout_retirement_trash_dirs(&self) -> Vec<PathBuf> {
        self.store_ref()
            .meta_get(CHECKOUT_RETIREMENT_TRASH_DIRS_KEY)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str::<Vec<PathBuf>>(&value).ok())
            .unwrap_or_default()
    }

    /// Progress of the last retirement runs.
    pub(crate) fn checkout_retirement_progress(&self) -> CheckoutRetirementProgress {
        self.store_ref()
            .meta_get(CHECKOUT_RETIREMENT_PROGRESS_KEY)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default()
    }

    /// Sessions whose recovery archive would exceed the cap.
    pub(crate) fn checkout_retirement_too_large(&self) -> std::collections::BTreeSet<i64> {
        self.store_ref()
            .meta_get(CHECKOUT_RETIREMENT_TOO_LARGE_KEY)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default()
    }

    fn set_checkout_retirement_too_large(&self, session_id: i64, too_large: bool) {
        let mut sessions = self.checkout_retirement_too_large();
        let changed = if too_large {
            sessions.insert(session_id)
        } else {
            sessions.remove(&session_id)
        };
        if changed {
            while sessions.len() > 4_096 {
                sessions.pop_first();
            }
            crate::warn_unrecorded(
                "remember checkouts whose archive is too large",
                self.store_ref().meta_set(
                    CHECKOUT_RETIREMENT_TOO_LARGE_KEY,
                    &serde_json::to_string(&sessions).unwrap_or_default(),
                ),
            );
        }
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
        let outcome = self.retire_closed_checkout_inner(session_id, run)?;
        crate::phase_telemetry::add(outcome.counter(), std::time::Duration::ZERO);
        Ok(outcome)
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
        if !preserves {
            // Everything is in Git: no archive. Re-check under the
            // lifecycle claim that nothing changed, then move it away.
            let Some(_claim) = self.try_claim_worktree_lifecycle()? else {
                return Ok(Kept("lifecycle_busy"));
            };
            let current = self.store_ref().session(session_id)?;
            if !current.status.is_closed()
                || current.cleanup_state != crate::SessionCleanupState::Closed
                || current.worktree_path != session.worktree_path
            {
                return Ok(Kept("changed"));
            }
            let head = crate::git::GitRepo::discover(&worktree)
                .ok()
                .and_then(|checkout| checkout.head_commit().ok());
            if head.as_deref() != Some(plan.head.as_str()) {
                return Ok(Kept("changed"));
            }
            if self
                .remove_checkout_through_trash(session_id, &worktree, false)
                .is_err()
                || worktree.exists()
            {
                return Ok(Kept("trash_failed"));
            }
            self.mark_removed_checkout_sessions_cleaned(&[session_id], session_id)?;
            self.set_checkout_retirement_too_large(session_id, false);
            return Ok(RetireOutcome::RetiredClean);
        }
        let bytes = plan
            .staged_patch_bytes
            .saturating_add(plan.unstaged_patch_bytes)
            .saturating_add(plan.untracked_bytes)
            .saturating_add(plan.ignored_bytes);
        if bytes > retire_archive_max_bytes() {
            self.set_checkout_retirement_too_large(session_id, true);
            return Ok(Kept("archive_too_large"));
        }
        let digest = plan.digest.clone();
        // The archive writer writes and reads back the archive, re-plans
        // under the lifecycle claim, and only then moves the checkout into
        // the trash; any failure keeps the checkout and the archive.
        match self.cleanup_resolve_retirement_apply(session_id, &digest, false) {
            Ok(outcome) if outcome.worktree_removed => {
                self.set_checkout_retirement_too_large(session_id, false);
                Ok(RetireOutcome::RetiredArchived { bytes })
            }
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

    /// Drain this repository's trash directories until `deadline`, as the
    /// one host-wide deleter; another process holding the deleter lock
    /// makes this a no-op (it drains them too, oldest first).
    pub(crate) fn drain_checkout_trash(
        &self,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let dirs = self.checkout_retirement_trash_dirs();
        if dirs.is_empty() {
            return crate::checkout_trash::TrashDrainReport::default();
        }
        let lock_path = crate::host_state::resolve_host_state_dir()
            .map(|(dir, _)| dir.join(crate::checkout_trash::DELETER_LOCK_FILE))
            .unwrap_or_else(|| dirs[0].join(crate::checkout_trash::DELETER_LOCK_FILE));
        let Ok(Some(_lock)) = crate::checkout_trash::try_deleter_lock(&lock_path) else {
            crate::phase_telemetry::add("checkout_trash_deleter_busy", std::time::Duration::ZERO);
            let mut report = crate::checkout_trash::TrashDrainReport::default();
            report.entries_pending = dirs
                .iter()
                .map(|dir| crate::checkout_trash::trash_entries(dir).len())
                .sum();
            return report;
        };
        let below_headroom = self.checkout_trash_below_headroom(&dirs[0]);
        let pace = crate::checkout_trash::DeleterPace::for_host(
            below_headroom,
            crate::checkout_trash::host_loaded(),
        );
        crate::checkout_trash::drain_trash(&dirs, deadline, pace)
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

    /// Remember the trash directory a checkout was moved into, so the
    /// deleter finds it.
    pub(crate) fn note_checkout_trashed(&self, trashed: &crate::checkout_trash::TrashedCheckout) {
        if let Some(trash) = trashed.path.parent() {
            self.remember_trash_dir(trash);
        }
        self.request_checkout_retirement_run();
    }
}
