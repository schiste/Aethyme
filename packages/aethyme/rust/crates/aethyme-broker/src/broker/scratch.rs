//! The broker's records of per-session scratch directories
//! ([`crate::session_scratch`]): created at `start` (or on first use by a
//! command the broker runs for an older session), replaced by a fresh one on
//! `start --reuse`, and retired by the checkout retirement lane once the
//! session is closed, finished or cleaned (or the directory was replaced).
//!
//! Retirement never deletes: it proves no process has its working directory
//! or an open file inside (the lane's one open-file observation), takes the
//! repository lifecycle claim without waiting, verifies the directory is the
//! one recorded, and renames it into the pool's checkout trash. The paced
//! host-wide deleter unlinks it later, after its own open-file observation.
//! Anything kept is retried on the next run, its reason recorded.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::retire::RetireRun;
use crate::Broker;
use crate::session_scratch::{Refusal, ScratchRecord, ScratchRecords};

/// Broker metadata key holding every recorded scratch directory.
pub(crate) const SESSION_SCRATCH_RECORDS_KEY: &str = "session_scratch.records";
/// Bytes the deleter freed removing scratch directories, all runs.
const SESSION_SCRATCH_FREED_KEY: &str = "session_scratch.bytes_freed_total";
/// The longest one retirement run spends retiring scratch directories (the
/// moves are O(1) renames; this bounds the per-directory proofs).
pub(crate) const SCRATCH_RETIRE_BUDGET: Duration = Duration::from_secs(5);
/// The longest one retirement run spends measuring live scratch directories.
const SCRATCH_MEASURE_BUDGET: Duration = Duration::from_secs(2);
/// The fresh open-file observation a replaced directory gets right before
/// its rename.
const SUPERSEDED_PROCESS_USE_BUDGET: Duration = Duration::from_secs(10);
/// Unrecorded container entries named per run, at most.
const UNRECORDED_NAMES_MAX: usize = 20;
/// Finished command runs moved per scratch directory per run, at most.
const FINISHED_RUNS_PER_PASS: usize = 64;
/// Finished command runs counted per scratch directory for the ledger.
const FINISHED_RUNS_COUNTED_MAX: usize = 4096;

/// The instruction shown with an available scratch directory.
pub(crate) const SCRATCH_INSTRUCTION: &str = "Put build output (pass it as a tool's --target-dir, --out-dir or output path) and temporary files in this directory instead of the system temp folder; commands the broker runs for the session get it as $AETHYME_SESSION_SCRATCH_DIR, and the broker removes it when the session is finished, closed or cleaned.";

/// The instruction shown with an available scratch directory.
#[cfg(test)]
pub(crate) fn scratch_instruction() -> &'static str {
    SCRATCH_INSTRUCTION
}

/// What `start`, `--reuse` and `--adopt` report about the session's scratch
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionScratch {
    /// The directory, when the session has one.
    pub(crate) path: Option<PathBuf>,
    /// The instruction when it has one; why not otherwise.
    pub(crate) note: String,
}

impl SessionScratch {
    fn available(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            note: SCRATCH_INSTRUCTION.to_string(),
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        crate::phase_telemetry::add("session_scratch_unavailable", Duration::ZERO);
        Self {
            path: None,
            note: reason.into(),
        }
    }
}

/// What one retirement run did with scratch directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScratchRetireReport {
    /// Directories judged (their session closed, or replaced).
    pub examined: usize,
    /// Moved into the trash.
    pub retired: usize,
    /// Records of directories already gone, dropped.
    pub forgotten_missing: usize,
    /// Kept, by reason.
    pub kept: std::collections::BTreeMap<String, usize>,
    /// Entries in a scratch container the broker did not record (left alone).
    pub unrecorded: Vec<String>,
    /// The run's deadline stopped the pass; the next run goes on.
    pub more_remaining: bool,
    /// Finished command runs ([`crate::command_temp`]) of open sessions
    /// moved into the trash.
    pub command_temps_retired: usize,
    /// Finished command runs kept (in use, or refused), retried next run.
    pub command_temps_kept: usize,
}

/// The phase counter of one keep, by reason.
fn kept_counter(code: &str) -> &'static str {
    match code {
        "session_unknown" => "session_scratch_kept_session_unknown",
        "process_using_scratch" => "session_scratch_kept_process_using_scratch",
        "process_use_unknown" => "session_scratch_kept_process_use_unknown",
        "lifecycle_busy" => "session_scratch_kept_lifecycle_busy",
        "changed" => "session_scratch_kept_changed",
        "not_a_recorded_scratch" => "session_scratch_kept_not_a_recorded_scratch",
        "not_a_real_directory" => "session_scratch_kept_not_a_real_directory",
        "foreign_container" => "session_scratch_kept_foreign_container",
        "identity_changed" => "session_scratch_kept_identity_changed",
        "crosses_device" => "session_scratch_kept_crosses_device",
        "move_failed" => "session_scratch_kept_move_failed",
        _ => "session_scratch_kept_other",
    }
}

/// Keeps that no retry lifts by itself: the directory is not the one
/// recorded (or could not be re-identified after a remount).
fn is_refusal_keep(code: &str) -> bool {
    matches!(
        code,
        "not_a_recorded_scratch"
            | "not_a_real_directory"
            | "foreign_container"
            | "identity_changed"
            | "crosses_device"
    )
}

impl Broker {
    /// Every recorded scratch directory of this repository.
    pub(crate) fn session_scratch_records(&self) -> ScratchRecords {
        self.meta_json(SESSION_SCRATCH_RECORDS_KEY)
    }

    /// The session's current (not replaced) record.
    fn current_scratch_record(&self, session_id: i64) -> Option<ScratchRecord> {
        self.session_scratch_records()
            .into_values()
            .filter(|record| record.session_id == session_id && !record.superseded)
            .max_by_key(|record| record.created_ms)
    }

    /// The session's scratch directory, when one is recorded and still a
    /// directory. Read-only: never creates one.
    pub(crate) fn session_scratch_dir(&self, session_id: i64) -> Option<PathBuf> {
        let path = self.current_scratch_record(session_id)?.path();
        path.is_dir().then_some(path)
    }

    /// The session's scratch directory, created when it has none (or the
    /// recorded one is gone) and the session is open with its checkout in a
    /// broker-owned pool. Never fails the caller: an unavailable directory is
    /// reported with its reason.
    pub(crate) fn ensure_session_scratch(&self, session_id: i64) -> SessionScratch {
        let session = match self.store_ref().session(session_id) {
            Ok(session) => session,
            Err(error) => {
                return SessionScratch::unavailable(format!("session lookup failed: {error}"));
            }
        };
        if let Some(record) = self.current_scratch_record(session_id) {
            match crate::session_scratch::verify(&record) {
                Ok(_) => {
                    let path = record.path();
                    crate::session_scratch::remember_for_commands(session_id, &path);
                    crate::phase_telemetry::add("session_scratch_existing", Duration::ZERO);
                    return SessionScratch::available(path);
                }
                Err(Refusal::Missing) => {
                    // Removed by hand: forget it and make a fresh one below.
                    self.forget_scratch_record(&record);
                    crate::phase_telemetry::add("session_scratch_recreated", Duration::ZERO);
                }
                Err(Refusal::IdentityChanged | Refusal::CrossesDevice)
                    if self.reidentify_scratch_record(&record).is_some() =>
                {
                    let path = record.path();
                    crate::session_scratch::remember_for_commands(session_id, &path);
                    return SessionScratch::available(path);
                }
                Err(refusal) => {
                    return SessionScratch::unavailable(format!(
                        "the recorded scratch directory {} is refused ({}); it is kept for review",
                        record.path().display(),
                        refusal.code()
                    ));
                }
            }
        }
        if session.status.is_closed() || session.cleanup_state != crate::SessionCleanupState::Open {
            return SessionScratch::unavailable("the session is closed");
        }
        let worktree = Path::new(&session.worktree_path);
        let Some(pool) = self.trash_pool_for(&session, worktree) else {
            crate::phase_telemetry::add("session_scratch_unavailable_not_in_pool", Duration::ZERO);
            return SessionScratch::unavailable(
                "the session's checkout is not in a broker-owned worktree pool (adopted checkouts get none)",
            );
        };
        let record = match crate::session_scratch::create(&pool, session_id) {
            Ok(record) => record,
            Err(error) => {
                crate::phase_telemetry::add("session_scratch_create_failed", Duration::ZERO);
                return SessionScratch::unavailable(format!("cannot create it: {error}"));
            }
        };
        let path = record.path();
        let key = record.key();
        let stored = self.update_meta_json(
            SESSION_SCRATCH_RECORDS_KEY,
            |records: &mut ScratchRecords| {
                records.insert(key.clone(), record.clone());
            },
        );
        if let Err(error) = stored {
            // Never leave a directory nothing would retire: it is empty, so
            // one `rmdir` undoes it.
            crate::warn_unrecorded(
                "remove an unrecorded scratch directory",
                std::fs::remove_dir(&path),
            );
            crate::phase_telemetry::add("session_scratch_record_failed", Duration::ZERO);
            return SessionScratch::unavailable(format!("cannot record it: {error}"));
        }
        crate::session_scratch::remember_for_commands(session_id, &path);
        SessionScratch::available(path)
    }

    /// The variable naming the session's scratch directory, for a command
    /// the broker runs for it (created on first use for a session started
    /// before scratch directories existed). Empty when it has none. The
    /// temp-directory variables (`TMPDIR`, `TMP`, `TEMP`) are deliberately
    /// left alone: see [`crate::session_scratch`].
    pub(crate) fn session_scratch_environment(&self, session_id: i64) -> Option<(String, String)> {
        let path = self.ensure_session_scratch(session_id).path?;
        Some((
            crate::session_scratch::SESSION_SCRATCH_ENV.to_owned(),
            path.to_string_lossy().into_owned(),
        ))
    }

    /// A temp directory for one command the broker runs for the session
    /// ([`crate::command_temp`]): `None` when the repository opted out, the
    /// session has no scratch directory, or the command keeps the inherited
    /// temp folder (each counted). Dropping it ends the run.
    pub(crate) fn command_temp_for_session(
        &self,
        session_id: i64,
    ) -> Option<crate::command_temp::CommandTemp> {
        let _span = crate::phase_telemetry::span("command_temp_prepare");
        if !self.remember_command_temp_policy(session_id) {
            crate::phase_telemetry::add("command_temp_opted_out", Duration::ZERO);
            return None;
        }
        let Some(scratch) = self.ensure_session_scratch(session_id).path else {
            crate::phase_telemetry::add("command_temp_no_scratch", Duration::ZERO);
            return None;
        };
        // Temp writes land on the pool's volume: below the gate headroom
        // (the classifier the storage sweeps use) the command keeps the
        // inherited temp folder.
        if crate::command_temp::pressured(crate::disk_headroom::available_headroom(&scratch)) {
            crate::phase_telemetry::add("command_temp_skipped_pressure", Duration::ZERO);
            return None;
        }
        crate::command_temp::CommandTemp::create(&scratch)
    }

    /// Read the repository's `[command_temp]` policy and remember it for the
    /// commands this process spawns for `session_id` (gates); true when on.
    /// Also asks for the retirement run when the session's finished runs
    /// are past their threshold.
    pub(crate) fn remember_command_temp_policy(&self, session_id: i64) -> bool {
        let enabled = crate::command_temp::CommandTempPolicy::load(self.main_root()).enabled;
        crate::command_temp::remember_enabled(session_id, enabled);
        self.request_retirement_for_command_temps(session_id);
        enabled
    }

    /// End a command's run (dropping its guard) and check the session's
    /// finished runs against the request thresholds.
    pub(crate) fn finish_command_temp(
        &self,
        session_id: i64,
        temp: Option<crate::command_temp::CommandTemp>,
    ) {
        if let Some(temp) = temp {
            drop(temp);
            self.request_retirement_for_command_temps(session_id);
        }
    }

    /// Request the checkout retirement run (the same request finish and
    /// close make) when a run finished past a threshold in this process, or
    /// when `session_id`'s finished runs awaiting removal reach
    /// [`crate::command_temp::RETIREMENT_REQUEST_RUNS`] or
    /// [`crate::command_temp::RETIREMENT_REQUEST_BYTES`]. Nothing is deleted
    /// here; below the thresholds the periodic run takes them.
    pub(crate) fn request_retirement_for_command_temps(&self, session_id: i64) {
        let _span = crate::phase_telemetry::span("command_temp_backlog_check");
        let mut wanted = crate::command_temp::take_retirement_wanted();
        if !wanted && let Some(record) = self.current_scratch_record(session_id) {
            let finished = crate::session_scratch::finished_runs(
                &record,
                crate::command_temp::RETIREMENT_REQUEST_RUNS,
            );
            let bytes = finished
                .iter()
                .map(|run| run.marker.bytes)
                .fold(0_u64, u64::saturating_add);
            wanted = finished.len() >= crate::command_temp::RETIREMENT_REQUEST_RUNS
                || bytes >= crate::command_temp::RETIREMENT_REQUEST_BYTES
                || finished.iter().any(|run| run.marker.large());
        }
        if wanted {
            crate::phase_telemetry::add("command_temp_retirement_requested", Duration::ZERO);
            self.request_checkout_retirement_run();
        }
    }

    /// Replace the session's scratch directory with a fresh one (`start
    /// --reuse`): the old one is marked replaced, so the retirement lane
    /// removes it once no process uses it, and a new one is created.
    pub(crate) fn fresh_session_scratch(&self, session_id: i64) -> SessionScratch {
        let mut replaced = 0_usize;
        let marked = self.update_meta_json(
            SESSION_SCRATCH_RECORDS_KEY,
            |records: &mut ScratchRecords| {
                replaced = 0;
                for record in records.values_mut() {
                    if record.session_id == session_id && !record.superseded {
                        record.superseded = true;
                        replaced += 1;
                    }
                }
            },
        );
        if let Err(error) = marked {
            return SessionScratch::unavailable(format!(
                "cannot replace the previous scratch directory: {error}"
            ));
        }
        if replaced > 0 {
            crate::phase_telemetry::add("session_scratch_superseded", Duration::ZERO);
            self.request_checkout_retirement_run();
        }
        self.ensure_session_scratch(session_id)
    }

    /// A session closed: when it has a scratch directory, ask for a
    /// retirement run so it is removed soon rather than at the next idle run.
    pub(crate) fn note_session_scratch_closed(&self, session_id: i64) {
        if self.current_scratch_record(session_id).is_some() {
            self.request_checkout_retirement_run();
        }
    }

    /// After a remount changed the volume's device number, re-identify the
    /// record ([`crate::session_scratch::reidentify`]) and store its new
    /// device. `None` when it cannot be re-identified (the caller keeps it).
    fn reidentify_scratch_record(&self, record: &ScratchRecord) -> Option<ScratchRecord> {
        let updated = crate::session_scratch::reidentify(record).ok()?;
        let key = record.key();
        let stored = self.update_meta_json(
            SESSION_SCRATCH_RECORDS_KEY,
            |records: &mut ScratchRecords| {
                if let Some(stored) = records.get_mut(&key)
                    && (stored.device, stored.inode) == (record.device, record.inode)
                {
                    stored.device = updated.device;
                }
            },
        );
        match stored {
            Ok(()) => Some(updated),
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "record a re-identified scratch directory",
                    Err(error),
                );
                None
            }
        }
    }

    fn forget_scratch_record(&self, record: &ScratchRecord) {
        let key = record.key();
        crate::warn_unrecorded(
            "forget a scratch directory record",
            self.update_meta_json(
                SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut ScratchRecords| {
                    if records
                        .get(&key)
                        .is_some_and(|stored| stored.inode == record.inode)
                    {
                        records.remove(&key);
                    }
                },
            ),
        );
    }

    fn remember_scratch_keep(
        &self,
        record: &ScratchRecord,
        code: &str,
        report: &mut ScratchRetireReport,
    ) {
        *report.kept.entry(code.to_string()).or_default() += 1;
        crate::phase_telemetry::add(kept_counter(code), Duration::ZERO);
        let key = record.key();
        crate::warn_unrecorded(
            "record a kept scratch directory",
            self.update_meta_json(
                SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut ScratchRecords| {
                    if let Some(stored) = records.get_mut(&key) {
                        stored.keep = Some(code.to_string());
                        stored.attempts = stored.attempts.saturating_add(1);
                    }
                },
            ),
        );
    }

    /// Retire scratch directories whose session is closed (or gone), and
    /// those replaced by a fresh one, until `deadline`. See the module
    /// documentation for the rules.
    pub(crate) fn retire_session_scratch(
        &self,
        run: &mut RetireRun,
        deadline: Instant,
    ) -> ScratchRetireReport {
        let _span = crate::phase_telemetry::span("session_scratch_retire_pass");
        let records = self.session_scratch_records();
        let mut report = ScratchRetireReport::default();
        let mut pools =
            std::collections::BTreeMap::<PathBuf, std::collections::BTreeSet<String>>::new();
        for record in records.values() {
            pools
                .entry(record.pool.clone())
                .or_default()
                .insert(record.entry.clone());
        }
        for record in records.values() {
            if Instant::now() >= deadline {
                report.more_remaining = true;
                break;
            }
            let eligible = record.superseded
                || match self.store_ref().session(record.session_id) {
                    Ok(session) => session.status.is_closed(),
                    Err(crate::BrokerError::SessionNotFound(_)) => true,
                    Err(_) => {
                        self.remember_scratch_keep(record, "session_unknown", &mut report);
                        continue;
                    }
                };
            if !eligible {
                self.retire_finished_command_temps(record, run, deadline, &mut report);
                continue;
            }
            report.examined += 1;
            let path = record.path();
            let snapshot = match run.process_use() {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    self.remember_scratch_keep(record, "process_use_unknown", &mut report);
                    continue;
                }
            };
            if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &path).is_some() {
                self.remember_scratch_keep(record, "process_using_scratch", &mut report);
                continue;
            }
            // A replaced directory belongs to a session that is still open,
            // whose agent may have entered it since the run's observation
            // (which can be minutes old): ask again, right before the claim
            // and the rename.
            if record.superseded {
                crate::phase_telemetry::add(
                    "session_scratch_superseded_fresh_observation",
                    Duration::ZERO,
                );
                let fresh = crate::session_holder::retirement_run_process_use_snapshot(
                    SUPERSEDED_PROCESS_USE_BUDGET,
                );
                if matches!(fresh, crate::reclaim::OpenFileSnapshot::Incomplete { .. }) {
                    self.remember_scratch_keep(record, "process_use_unknown", &mut report);
                    continue;
                }
                if crate::reclaim::open_file_snapshot_blocker_for_worktree(&fresh, &path).is_some()
                {
                    self.remember_scratch_keep(record, "process_using_scratch", &mut report);
                    continue;
                }
            }
            // The rename, and the record it rests on, under the lifecycle
            // claim taken without waiting: a foreground command never queues
            // behind this pass.
            let _claim = match self.try_claim_worktree_lifecycle("retire") {
                Ok(Some(claim)) => claim,
                Ok(None) | Err(_) => {
                    self.remember_scratch_keep(record, "lifecycle_busy", &mut report);
                    continue;
                }
            };
            let unchanged = self
                .session_scratch_records()
                .get(&record.key())
                .is_some_and(|stored| {
                    stored.inode == record.inode && stored.device == record.device
                });
            if !unchanged {
                self.remember_scratch_keep(record, "changed", &mut report);
                continue;
            }
            // A remount gives the volume a new device number: re-identify
            // under the claim before judging the identity.
            let mut current = record.clone();
            if matches!(
                crate::session_scratch::verify(&current),
                Err(Refusal::IdentityChanged | Refusal::CrossesDevice)
            ) && let Some(updated) = self.reidentify_scratch_record(&current)
            {
                current = updated;
            }
            let moved = crate::session_scratch::move_to_trash(&current, &mut |dir, entry, row| {
                self.record_checkout_trash_row(dir, entry, row)
            });
            match moved {
                Ok(moved) => {
                    crate::warn_unrecorded(
                        "confirm a scratch directory's trash entry",
                        self.record_checkout_trash_row(&moved.trash_dir, &moved.entry, moved.row),
                    );
                    self.forget_scratch_record(record);
                    report.retired += 1;
                    crate::phase_telemetry::add("session_scratch_retired", Duration::ZERO);
                }
                Err(Refusal::Missing) => {
                    self.forget_scratch_record(record);
                    report.forgotten_missing += 1;
                    crate::phase_telemetry::add(
                        "session_scratch_missing_forgotten",
                        Duration::ZERO,
                    );
                }
                Err(refusal) => self.remember_scratch_keep(record, refusal.code(), &mut report),
            }
        }
        crate::phase_telemetry::attach(
            "session_scratch_retire",
            serde_json::json!({
                "examined": report.examined,
                "retired": report.retired,
                "forgotten_missing": report.forgotten_missing,
                "kept": report.kept,
                "more_remaining": report.more_remaining,
                "command_temps_retired": report.command_temps_retired,
                "command_temps_kept": report.command_temps_kept,
            }),
        );
        for (pool, recorded) in &pools {
            for name in
                crate::session_scratch::unrecorded_entries(pool, recorded, UNRECORDED_NAMES_MAX)
            {
                if report.unrecorded.len() >= UNRECORDED_NAMES_MAX {
                    break;
                }
                crate::phase_telemetry::add("session_scratch_unrecorded", Duration::ZERO);
                report.unrecorded.push(name);
            }
        }
        report
    }

    /// Move the finished command runs of an open session's scratch
    /// directory into the pool's trash: none may be in use (the run's
    /// open-file observation), and each must verify
    /// ([`crate::session_scratch::move_finished_run_to_trash`]). The paced
    /// host deleter unlinks them later, after its own observation.
    fn retire_finished_command_temps(
        &self,
        record: &ScratchRecord,
        run: &mut RetireRun,
        deadline: Instant,
        report: &mut ScratchRetireReport,
    ) {
        let finished = crate::session_scratch::finished_runs(record, FINISHED_RUNS_PER_PASS);
        if finished.is_empty() {
            return;
        }
        let _span = crate::phase_telemetry::span("command_temp_retire");
        let Ok(snapshot) = run.process_use() else {
            report.command_temps_kept += finished.len();
            crate::phase_telemetry::add("command_temp_kept_process_use_unknown", Duration::ZERO);
            return;
        };
        for finished_run in &finished {
            if Instant::now() >= deadline {
                report.more_remaining = true;
                return;
            }
            let path = finished_run.path(record);
            if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &path).is_some() {
                report.command_temps_kept += 1;
                crate::phase_telemetry::add("command_temp_kept_in_use", Duration::ZERO);
                continue;
            }
            let moved = crate::session_scratch::move_finished_run_to_trash(
                record,
                finished_run,
                &mut |dir, entry, row| self.record_checkout_trash_row(dir, entry, row),
            );
            match moved {
                Ok(moved) => {
                    crate::warn_unrecorded(
                        "confirm a command temp directory's trash entry",
                        self.record_checkout_trash_row(&moved.trash_dir, &moved.entry, moved.row),
                    );
                    if let Some(alias) = &finished_run.marker.alias {
                        crate::command_temp::remove_alias(alias, &path);
                    }
                    crate::session_scratch::forget_finished_run(record, finished_run);
                    report.command_temps_retired += 1;
                    crate::phase_telemetry::add("command_temp_retired", Duration::ZERO);
                }
                Err(Refusal::Missing) => {
                    // The run is gone (removed by hand): drop its marker and
                    // its alias, which no longer resolves.
                    if let Some(alias) = &finished_run.marker.alias {
                        crate::command_temp::remove_alias(alias, &path);
                    }
                    crate::session_scratch::forget_finished_run(record, finished_run);
                    crate::phase_telemetry::add("command_temp_missing_forgotten", Duration::ZERO);
                }
                Err(refusal) => {
                    report.command_temps_kept += 1;
                    crate::phase_telemetry::add(
                        if refusal == Refusal::Failed {
                            "command_temp_kept_move_failed"
                        } else {
                            "command_temp_kept_refused"
                        },
                        Duration::ZERO,
                    );
                }
            }
        }
    }

    /// Measure the live scratch directories (bounded) and write this
    /// repository's scratch usage into the storage ledger; `freed` is what
    /// the deleter freed removing scratch directories in this run.
    pub(crate) fn record_session_scratch_usage(&self, freed: u64) {
        let _span = crate::phase_telemetry::span("session_scratch_measure");
        let freed_total = self
            .store_ref()
            .meta_get(SESSION_SCRATCH_FREED_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_add(freed);
        if freed > 0 {
            crate::warn_unrecorded(
                "record bytes freed from scratch directories",
                self.store_ref()
                    .meta_set(SESSION_SCRATCH_FREED_KEY, &freed_total.to_string()),
            );
        }
        let records = self.session_scratch_records();
        let deadline = Instant::now() + SCRATCH_MEASURE_BUDGET;
        let mut usage = crate::storage_ledger::SessionScratchUsage {
            bytes_freed_total: freed_total,
            observed_at_ms: crate::clock::epoch_ms(),
            ..crate::storage_ledger::SessionScratchUsage::default()
        };
        for record in records.values() {
            usage.directories += 1;
            if Instant::now() >= deadline {
                usage.partial = true;
                continue;
            }
            let (bytes, complete) = crate::session_scratch::measure(&record.path(), deadline);
            usage.partial |= !complete;
            usage.bytes = usage.bytes.saturating_add(bytes);
            if bytes > usage.largest_bytes {
                usage.largest_bytes = bytes;
                usage.largest_session_id = Some(record.session_id);
            }
            if record.keep.is_some() {
                usage.kept += 1;
            }
            if record.keep.as_deref().is_some_and(is_refusal_keep) {
                usage.refused += 1;
            }
            // Finished command runs awaiting removal, from their markers
            // (sizes measured at finish; nothing is walked again here).
            for finished in crate::session_scratch::finished_runs(record, FINISHED_RUNS_COUNTED_MAX)
            {
                usage.finished_runs += 1;
                usage.finished_run_bytes = usage
                    .finished_run_bytes
                    .saturating_add(finished.marker.bytes);
            }
        }
        crate::phase_telemetry::attach(
            "session_scratch_usage",
            serde_json::json!({
                "directories": usage.directories,
                "bytes": usage.bytes,
                "largest_bytes": usage.largest_bytes,
                "kept": usage.kept,
                "refused": usage.refused,
                "partial": usage.partial,
                "bytes_freed_this_run": freed,
                "finished_runs": usage.finished_runs,
                "finished_run_bytes": usage.finished_run_bytes,
            }),
        );
        if let Some((host_state, _)) = crate::host_state::resolve_host_state_dir() {
            crate::warn_unrecorded(
                "record session scratch usage in the storage ledger",
                crate::storage_ledger::record_session_scratch_usage(
                    &host_state,
                    &self.main_root().to_string_lossy(),
                    usage,
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn git(root: &Path, args: &[&str]) -> String {
        let output = crate::git::git_command()
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A repository with a local `origin`, no closed-checkout grace, and no
    /// process using anything.
    fn fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Scratch test"]);
        git(&root, &["config", "user.email", "scratch@example.invalid"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
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

    fn run_retirement(
        broker: &mut crate::Broker,
    ) -> super::super::retire::CheckoutRetirementReport {
        run(broker)
    }

    fn run(broker: &mut crate::Broker) -> super::super::retire::CheckoutRetirementReport {
        broker
            .run_checkout_retirement(Instant::now() + Duration::from_secs(600))
            .unwrap()
    }

    fn started(broker: &mut crate::Broker, label: &str) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let scratch = broker.ensure_session_scratch(session.id);
        let path = scratch
            .path
            .unwrap_or_else(|| panic!("no scratch: {}", scratch.note));
        (session, path)
    }

    fn using(path: &Path) {
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "cargo".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(path).unwrap(),
            }]),
        ));
    }

    fn nobody_using() {
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
    }

    #[test]
    fn a_started_session_gets_a_scratch_directory_beside_its_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch created");
        let worktree = PathBuf::from(&session.worktree_path);
        let pool = std::fs::canonicalize(worktree.parent().unwrap()).unwrap();
        assert!(path.is_dir());
        assert!(
            path.starts_with(pool.join(crate::session_scratch::SCRATCH_CONTAINER)),
            "{path:?} is in the checkout's pool"
        );
        assert!(!path.starts_with(&worktree), "outside the checkout");
        std::fs::write(path.join("out.o"), b"build output").unwrap();
        assert_eq!(
            git(&worktree, &["status", "--porcelain"]),
            "",
            "never in git status"
        );
        // Discovery: the read-only lookup and the command environment agree.
        assert_eq!(broker.session_scratch_dir(session.id), Some(path.clone()));
        assert_eq!(
            crate::session_scratch::remembered_for_commands(session.id),
            Some(path.clone())
        );
        // Asking again returns the same directory.
        assert_eq!(broker.ensure_session_scratch(session.id).path, Some(path));
    }

    #[test]
    fn finishing_retires_the_scratch_directory_through_the_trash() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch finished");
        std::fs::create_dir_all(path.join("target/debug")).unwrap();
        std::fs::write(path.join("target/debug/app"), b"binary").unwrap();
        let finished = broker.finish(session.id).unwrap();
        assert!(finished.closed, "{finished:#?}");
        assert!(path.is_dir(), "kept until the retirement lane runs");

        let report = run(&mut broker);

        assert_eq!(report.scratch.retired, 1, "{report:#?}");
        assert!(!path.exists(), "moved out of the container");
        assert_eq!(broker.session_scratch_dir(session.id), None);
        assert!(broker.session_scratch_records().is_empty());
        assert_eq!(report.trash.scratch_entries_removed, 1, "{report:#?}");
        assert!(
            broker
                .checkout_trash_rows()
                .values()
                .flat_map(|entries| entries.values())
                .all(|row| !row.scratch),
            "the deleter removed the entry"
        );
    }

    #[test]
    fn a_scratch_directory_in_use_is_kept_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch in use");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        using(&path);

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept.get("process_using_scratch"),
            Some(&1),
            "{report:#?}"
        );
        assert!(path.is_dir(), "a directory in use is never moved");
        let record = broker
            .session_scratch_records()
            .into_values()
            .next()
            .unwrap();
        assert_eq!(record.keep.as_deref(), Some("process_using_scratch"));
        assert_eq!(record.attempts, 1);

        nobody_using();
        let report = run(&mut broker);
        assert_eq!(
            report.scratch.retired, 1,
            "retried on the next run: {report:#?}"
        );
        assert!(!path.exists());
    }

    #[test]
    fn an_open_session_keeps_its_scratch_directory() {
        let (_owner, _storage, mut broker) = fixture();
        let (_session, path) = started(&mut broker, "scratch open");
        let report = run(&mut broker);
        assert_eq!(report.scratch.examined, 0, "{report:#?}");
        assert!(path.is_dir());
    }

    #[test]
    fn a_path_that_is_not_the_recorded_scratch_is_never_moved() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch swapped");
        // The recorded directory is replaced by another one of the same name.
        // (made before the original goes, so it never reuses its inode).
        let replacement = path.with_file_name("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        std::fs::write(path.join("keep.txt"), b"not the broker's").unwrap();
        // And a forged record names the session's checkout.
        let worktree = std::fs::canonicalize(&session.worktree_path).unwrap();
        let forged = crate::session_scratch::ScratchRecord {
            session_id: session.id,
            pool: worktree.parent().unwrap().to_path_buf(),
            entry: format!("../{}", worktree.file_name().unwrap().to_string_lossy()),
            device: 0,
            inode: 0,
            created_ms: 1,
            superseded: true,
            keep: None,
            attempts: 0,
        };
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut crate::session_scratch::ScratchRecords| {
                    records.insert(forged.key(), forged.clone());
                },
            )
            .unwrap();
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept.get("identity_changed"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(
            report.scratch.kept.get("not_a_recorded_scratch"),
            Some(&1),
            "{report:#?}"
        );
        assert!(
            path.join("keep.txt").is_file(),
            "the swapped directory stays"
        );
        assert_eq!(report.scratch.retired, 0);
        assert!(
            broker
                .checkout_trash_rows()
                .values()
                .flat_map(|entries| entries.values())
                .all(|row| !row.scratch),
            "nothing recorded in the trash"
        );
    }

    /// Rewrite every recorded device number, as a remount of the pool's
    /// volume does.
    fn remount_records(broker: &crate::Broker) {
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut crate::session_scratch::ScratchRecords| {
                    for record in records.values_mut() {
                        record.device = record.device.wrapping_add(1000);
                    }
                },
            )
            .unwrap();
    }

    #[test]
    fn a_remounted_volume_is_reidentified_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let (open, open_path) = started(&mut broker, "scratch remount open");
        let (closed, closed_path) = started(&mut broker, "scratch remount closed");
        broker
            .close_abandoning_unpushed(closed.id, "scratch fixture")
            .unwrap();
        remount_records(&broker);

        // A command for the open session re-identifies its directory.
        assert_eq!(
            broker.ensure_session_scratch(open.id).path,
            Some(open_path.clone())
        );
        let report = run(&mut broker);

        assert_eq!(report.scratch.retired, 1, "{report:#?}");
        assert!(!closed_path.exists());
        assert!(open_path.is_dir());
        let devices = broker
            .session_scratch_records()
            .into_values()
            .map(|record| record.device)
            .collect::<Vec<_>>();
        let actual = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&open_path).unwrap().dev()
        };
        assert_eq!(devices, vec![actual], "the record carries the new device");
    }

    #[test]
    fn a_remounted_directory_that_was_swapped_is_kept_and_counted() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch remount swapped");
        let replacement = path.with_file_name("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        remount_records(&broker);

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept.get("identity_changed"),
            Some(&1),
            "{report:#?}"
        );
        assert!(path.is_dir(), "never moved");
        let record = broker
            .session_scratch_records()
            .into_values()
            .next()
            .unwrap();
        assert!(super::is_refusal_keep(record.keep.as_deref().unwrap()));
    }

    #[test]
    fn a_replaced_directory_gets_a_fresh_observation_before_its_rename() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, first) = started(&mut broker, "scratch reused in use");
        broker.fresh_session_scratch(session.id);
        // The run's observation sees nobody; the fresh one right before the
        // rename sees the agent inside the replaced directory.
        crate::session_holder::queue_process_use_snapshots_for_tests(
            vec![
                crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
                crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "shell".into(),
                    descriptor: "cwd".into(),
                    file_type: Some("DIR".into()),
                    path: std::fs::canonicalize(&first).unwrap(),
                }]),
            ],
            false,
        );

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept.get("process_using_scratch"),
            Some(&1),
            "{report:#?}"
        );
        assert!(first.is_dir());
    }

    #[test]
    fn reuse_gets_a_fresh_scratch_directory_and_retires_the_old_one() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, first) = started(&mut broker, "scratch reused");
        let fresh = broker.fresh_session_scratch(session.id);
        let second = fresh.path.unwrap_or_else(|| panic!("{}", fresh.note));
        assert_ne!(first, second);
        assert_eq!(broker.session_scratch_dir(session.id), Some(second.clone()));

        let report = run(&mut broker);

        assert_eq!(report.scratch.retired, 1, "{report:#?}");
        assert!(!first.exists(), "the replaced one is retired");
        assert!(second.is_dir(), "the open session keeps its fresh one");
    }

    /// A command's run directory is inside the session scratch; what the
    /// command left there stays in place with a marker beside it, then
    /// reaches the trash through the retirement run and the paced deleter
    /// while the session stays open, its alias and marker going with it.
    #[test]
    fn a_finished_command_run_is_retired_through_the_trash() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, scratch) = started(&mut broker, "command temp retired");
        let temp = broker
            .command_temp_for_session(session.id)
            .expect("a command temp directory");
        let run = temp.run_dir();
        let tmpdir = temp.tmpdir();
        assert!(run.starts_with(&scratch));
        std::fs::write(run.join("leftover.tmp"), b"left by the hook").unwrap();
        let name = run.file_name().unwrap().to_owned();
        broker.finish_command_temp(session.id, Some(temp));
        assert!(run.join("leftover.tmp").is_file(), "kept in place");
        let marker = scratch
            .join(crate::command_temp::DONE_CONTAINER)
            .join(&name);
        assert!(marker.is_file());

        let report = run_retirement(&mut broker);

        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!run.exists(), "moved into the trash");
        assert!(!marker.exists(), "marker dropped");
        if tmpdir != run {
            assert!(std::fs::symlink_metadata(&tmpdir).is_err(), "alias removed");
        }
        assert!(scratch.is_dir(), "the open session keeps its scratch");
        assert!(
            broker
                .checkout_trash_rows()
                .values()
                .flat_map(|entries| entries.values())
                .all(|row| !row.scratch),
            "the deleter removed the entry"
        );
    }

    #[test]
    fn a_finished_command_run_in_use_is_kept() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, _scratch) = started(&mut broker, "command temp in use");
        let temp = broker.command_temp_for_session(session.id).unwrap();
        let run = temp.run_dir();
        std::fs::write(run.join("out"), b"x").unwrap();
        drop(temp);
        using(&run);
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_kept, 1, "{report:#?}");
        assert!(run.join("out").is_file());
    }

    /// Past the count threshold, finishing a run requests the retirement
    /// run; below it, nothing is requested.
    #[test]
    fn many_finished_runs_request_the_retirement_run() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, _scratch) = started(&mut broker, "command temp backlog");
        run_retirement(&mut broker);
        let due_later =
            |broker: &crate::Broker| !broker.checkout_retirement_due(crate::clock::epoch_ms());
        assert!(due_later(&broker), "the run scheduled its next pass");
        let _ = crate::command_temp::take_retirement_wanted();
        for index in 0..crate::command_temp::RETIREMENT_REQUEST_RUNS {
            let temp = broker.command_temp_for_session(session.id).unwrap();
            std::fs::write(temp.run_dir().join("out"), b"x").unwrap();
            broker.finish_command_temp(session.id, Some(temp));
            if index + 1 < crate::command_temp::RETIREMENT_REQUEST_RUNS {
                assert!(due_later(&broker), "below the threshold at {index}");
            }
        }
        assert!(
            broker.checkout_retirement_due(crate::clock::epoch_ms()),
            "requested at the threshold"
        );
    }

    #[test]
    fn a_repository_can_opt_out_of_command_temp_directories() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, _scratch) = started(&mut broker, "command temp opt out");
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[command_temp]\nenabled = false\n",
        )
        .unwrap();
        assert!(broker.command_temp_for_session(session.id).is_none());
        assert!(!crate::command_temp::remembered_enabled(session.id));
    }

    #[test]
    fn a_closed_session_gets_no_new_scratch_directory() {
        let (_owner, _storage, mut broker) = fixture();
        let session = broker.start_worktree("scratch closed", None).unwrap();
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        let scratch = broker.ensure_session_scratch(session.id);
        assert_eq!(scratch.path, None);
        assert!(broker.session_scratch_records().is_empty());
    }
}
