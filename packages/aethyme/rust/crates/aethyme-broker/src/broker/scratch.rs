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
use crate::session_scratch::{MoveFailure, Refusal, ScratchRecord, ScratchRecords};

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
/// Broker metadata key holding where each phase of the last scratch pass
/// stopped ([`ScratchRetireCursor`]).
pub(crate) const SESSION_SCRATCH_CURSOR_KEY: &str = "session_scratch.retire_cursor";
/// Failed moves in a row after which a phase stops for this run (a systemic
/// failure, [`MoveFailure::systemic`], stops it at once): a failing move is
/// retried on the next run, never in a loop.
const MOVE_FAILURES_BEFORE_STOP: usize = 3;
/// Moves attempted per phase per run, at most.
const MOVES_PER_PHASE: usize = 256;
/// The attachment counting failed moves by `<subject>.<step>.<cause>`.
const MOVE_FAILURES_ATTACHMENT: &str = "scratch_move_failures";
/// The longest one pass spends classifying records (store lookups and
/// done-container listings); the rest is classified by the next pass.
const SCRATCH_CLASSIFY_BUDGET: Duration = Duration::from_secs(2);
/// How far past the lane's own deadline the scratch pass may start new
/// work. With an item already in flight (at most one fresh open-file
/// observation of a replaced directory, 10 s, and one rename), a retirement
/// run ends at most this plus 10 s plus one move past its deadline.
pub(crate) const SCRATCH_OVERRUN_MAX: Duration = Duration::from_secs(20);

#[cfg(test)]
thread_local! {
    static MOVES_PER_PHASE_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

fn moves_per_phase() -> usize {
    #[cfg(test)]
    if let Some(limit) = MOVES_PER_PHASE_FOR_TEST.with(std::cell::Cell::get) {
        return limit;
    }
    MOVES_PER_PHASE
}

/// Where each phase of the last scratch retirement pass stopped: the key of
/// the last record it judged, or nothing once a phase went round.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ScratchRetireCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    classify: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_temps: Option<String>,
}

/// `items` (in key order) rotated to start after `cursor`: the records the
/// previous pass did not reach come first, then the ones it did.
fn rotate_after<'a, T>(mut items: Vec<(&'a str, T)>, cursor: Option<&str>) -> Vec<(&'a str, T)> {
    if let Some(cursor) = cursor {
        let split = items.partition_point(|(key, _)| *key <= cursor);
        items.rotate_left(split);
        if split > 0 {
            crate::phase_telemetry::add("session_scratch_retire_cursor_resumed", Duration::ZERO);
        }
    }
    items
}

/// The finished-runs phase's deadline: the pass's budget from `started`,
/// but never less than half of it from `now` (a closed directory's fresh
/// observation can use the closed share and more), and never past
/// `latest`.
fn pending_phase_deadline(
    started: Instant,
    budget: Duration,
    now: Instant,
    latest: Instant,
) -> Instant {
    (started + budget).max(now + budget / 2).min(latest)
}

/// One phase of a scratch pass: its deadline, its move cap, and its stop
/// rule for failed moves.
struct Phase {
    deadline: Instant,
    attempted: usize,
    failures_in_a_row: usize,
    stopped: bool,
    last_done: Option<String>,
}

impl Phase {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            attempted: 0,
            failures_in_a_row: 0,
            stopped: false,
            last_done: None,
        }
    }

    /// Whether the phase stopped after failed moves.
    fn stopped_for_failures(&self) -> bool {
        self.failures_in_a_row >= MOVE_FAILURES_BEFORE_STOP
    }

    /// Whether the phase may go on: no stop for failures, time left, and
    /// moves left under the cap. Marks it stopped otherwise.
    fn may_start(&mut self, report: &mut ScratchRetireReport) -> bool {
        if self.failures_in_a_row >= MOVE_FAILURES_BEFORE_STOP {
            report.stopped_after_failures = true;
            self.stopped = true;
            return false;
        }
        if Instant::now() >= self.deadline || self.attempted >= moves_per_phase() {
            report.more_remaining = true;
            self.stopped = true;
            return false;
        }
        true
    }

    /// [`Self::may_start`], counting one more move attempted when so.
    fn may_continue(&mut self, report: &mut ScratchRetireReport) -> bool {
        let go_on = self.may_start(report);
        if go_on {
            self.attempted += 1;
        }
        go_on
    }

    /// The outcome of one item: a failed move counts toward the stop rule
    /// (a systemic one stops the phase at once); anything else resets it.
    fn note(&mut self, failure: Option<MoveFailure>, stop_counter: &'static str) {
        let before = self.failures_in_a_row;
        self.failures_in_a_row = match failure {
            Some(failure) if failure.systemic() => MOVE_FAILURES_BEFORE_STOP,
            Some(_) => self.failures_in_a_row + 1,
            None => 0,
        };
        if before < MOVE_FAILURES_BEFORE_STOP && self.failures_in_a_row >= MOVE_FAILURES_BEFORE_STOP
        {
            crate::phase_telemetry::add(stop_counter, Duration::ZERO);
        }
    }

    /// The cursor to store: where it stopped (`previous` when it judged
    /// nothing), or nothing once it went round.
    fn cursor(&self, previous: Option<String>) -> Option<String> {
        if self.stopped {
            self.last_done.clone().or(previous)
        } else {
            None
        }
    }
}

/// Count one finished command run kept, by reason.
fn keep_command_temp(report: &mut ScratchRetireReport, reason: &str) {
    report.command_temps_kept += 1;
    *report
        .command_temps_kept_by_reason
        .entry(reason.to_owned())
        .or_default() += 1;
}

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
    /// Those keeps by reason: `in_use`, `process_use_unknown`, a refusal
    /// code, or `move_failed.<step>.<cause>`.
    pub command_temps_kept_by_reason: std::collections::BTreeMap<String, usize>,
    /// Directories kept because their move failed, by `<step>.<cause>`.
    pub kept_causes: std::collections::BTreeMap<String, usize>,
    /// A phase stopped after failed moves ([`MOVE_FAILURES_BEFORE_STOP`]).
    pub stopped_after_failures: bool,
    /// Recorded scratch directories.
    pub records: usize,
    /// Open sessions whose done container lists a finished run.
    pub sessions_with_finished_runs: usize,
    /// The classification stopped at its budget; the next pass goes on.
    pub classify_incomplete: bool,
    /// The run's open-file observation failed: nothing was judged.
    pub process_use_unknown: bool,
    /// Keeps to write into the records, once per pass: key and reason.
    keeps_to_record: Vec<(String, String)>,
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
        "pool_unavailable" => "session_scratch_kept_pool_unavailable",
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
        report
            .keeps_to_record
            .push((record.key(), code.to_string()));
    }

    /// Write the pass's keeps into the records: one rewrite of the records,
    /// not one per kept directory.
    fn record_scratch_keeps(&self, report: &mut ScratchRetireReport) {
        let keeps = std::mem::take(&mut report.keeps_to_record);
        if keeps.is_empty() {
            return;
        }
        crate::warn_unrecorded(
            "record kept scratch directories",
            self.update_meta_json(
                SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut ScratchRecords| {
                    for (key, code) in &keeps {
                        if let Some(stored) = records.get_mut(key) {
                            stored.keep = Some(code.clone());
                            stored.attempts = stored.attempts.saturating_add(1);
                        }
                    }
                },
            ),
        );
    }

    /// Retire scratch directories whose session is closed (or gone), and
    /// those replaced by a fresh one, then the finished command runs of open
    /// sessions, within `budget`. See the module documentation for the rules.
    ///
    /// On the busiest repository a pass judged 650 records in key order
    /// from the first one, with a descriptor walk from `/` per open session,
    /// and its 5 s ran out before the newest sessions (whose ids sort last)
    /// were reached: six passes in eight examined nothing. Now a cheap
    /// classification comes first (one store lookup per record, and one
    /// listing of an open session's done container), the run's open-file
    /// observation is taken outside the budget, the closed directories and
    /// the command runs each get a share of it, and each phase resumes after
    /// the record where the previous pass stopped. Every record is reached
    /// within a bounded number of passes.
    pub(crate) fn retire_session_scratch(
        &self,
        run: &mut RetireRun,
        budget: Duration,
        latest: Instant,
    ) -> ScratchRetireReport {
        let _span = crate::phase_telemetry::span("session_scratch_retire_pass");
        let records = self.session_scratch_records();
        let mut report = ScratchRetireReport {
            records: records.len(),
            ..ScratchRetireReport::default()
        };
        let mut pools =
            std::collections::BTreeMap::<PathBuf, std::collections::BTreeSet<String>>::new();
        for record in records.values() {
            pools
                .entry(record.pool.clone())
                .or_default()
                .insert(record.entry.clone());
        }
        let cursor: ScratchRetireCursor = self.meta_json(SESSION_SCRATCH_CURSOR_KEY);
        let mut next = ScratchRetireCursor::default();
        // Classify, from where the last pass's classification stopped, until
        // its own budget: one store lookup per record, one listing per open
        // session.
        let (closed, pending) = {
            let _classify = crate::phase_telemetry::span("session_scratch_retire_classify");
            let classify_deadline = (Instant::now() + SCRATCH_CLASSIFY_BUDGET).min(latest);
            let ordered = rotate_after(
                records
                    .iter()
                    .map(|(key, record)| (key.as_str(), record))
                    .collect(),
                cursor.classify.as_deref(),
            );
            let mut closed = Vec::new();
            let mut pending = Vec::new();
            let mut last_classified = None;
            for (key, record) in ordered {
                if Instant::now() >= classify_deadline {
                    report.classify_incomplete = true;
                    crate::phase_telemetry::add(
                        "session_scratch_retire_classify_stopped",
                        Duration::ZERO,
                    );
                    break;
                }
                last_classified = Some(key.to_owned());
                let eligible = record.superseded
                    || match self.store_ref().session(record.session_id) {
                        Ok(session) => session.status.is_closed(),
                        Err(crate::BrokerError::SessionNotFound(_)) => true,
                        Err(_) => {
                            self.remember_scratch_keep(record, "session_unknown", &mut report);
                            continue;
                        }
                    };
                if eligible {
                    closed.push((key, record));
                } else if crate::session_scratch::may_have_finished_runs(record) {
                    pending.push((key, record));
                }
            }
            if report.classify_incomplete {
                next.classify = last_classified.or_else(|| cursor.classify.clone());
                report.more_remaining = true;
            }
            // Back to key order, so the phases' own cursors apply.
            closed.sort_by(|left, right| left.0.cmp(right.0));
            pending.sort_by(|left, right| left.0.cmp(right.0));
            (closed, pending)
        };
        report.sessions_with_finished_runs = pending.len();
        let closed = rotate_after(closed, cursor.closed.as_deref());
        let pending = rotate_after(pending, cursor.command_temps.as_deref());
        // The run's open-file observation, when this pass is the first to
        // need it, before the budget starts: `lsof` alone can take 10 s on
        // the busiest machine. The run keeps a failed observation, so when
        // it failed nothing is judged and the cursors stay where they are.
        let observed = (closed.is_empty() && pending.is_empty()) || run.process_use().is_ok();
        if !observed {
            report.process_use_unknown = true;
            crate::phase_telemetry::add(
                "session_scratch_retire_process_use_unknown",
                Duration::ZERO,
            );
            next.closed = cursor.closed.clone();
            next.command_temps = cursor.command_temps.clone();
        } else {
            let started = Instant::now();
            let deadline = (started + budget).min(latest);
            let closed_deadline = if pending.is_empty() {
                deadline
            } else {
                (started + budget / 2).min(latest)
            };

            let mut phase = Phase::new(closed_deadline);
            for (key, record) in &closed {
                if !phase.may_continue(&mut report) {
                    break;
                }
                let failure = self.retire_closed_scratch(record, run, &mut report);
                phase.note(failure, "session_scratch_retire_stopped_after_failures");
                phase.last_done = Some((*key).to_owned());
            }
            next.closed = phase.cursor(cursor.closed.clone());

            let mut phase = Phase::new(pending_phase_deadline(
                started,
                budget,
                Instant::now(),
                latest,
            ));
            for (key, record) in &pending {
                if !phase.may_start(&mut report) {
                    break;
                }
                let finished =
                    self.retire_finished_command_temps(record, run, &mut phase, &mut report);
                // A record whose runs stopped the phase by failing is judged:
                // the next pass starts after it, so a failure that never
                // clears cannot hold back the sessions behind it.
                if finished || phase.stopped_for_failures() {
                    phase.last_done = Some((*key).to_owned());
                }
                if !finished {
                    break;
                }
            }
            next.command_temps = phase.cursor(cursor.command_temps.clone());
        }
        self.record_scratch_keeps(&mut report);

        if next != cursor {
            crate::warn_unrecorded(
                "record where the scratch retirement pass stopped",
                self.update_meta_json(
                    SESSION_SCRATCH_CURSOR_KEY,
                    |stored: &mut ScratchRetireCursor| {
                        *stored = next.clone();
                    },
                ),
            );
        }
        // A stop after failed moves is not "more to do": the next periodic
        // run retries, and a failure that never clears must not keep the
        // lane on its busy cadence.
        crate::phase_telemetry::attach(
            "session_scratch_retire",
            serde_json::json!({
                "records": report.records,
                "examined": report.examined,
                "retired": report.retired,
                "forgotten_missing": report.forgotten_missing,
                "kept": report.kept,
                "kept_causes": report.kept_causes,
                "more_remaining": report.more_remaining,
                "stopped_after_failures": report.stopped_after_failures,
                "classify_incomplete": report.classify_incomplete,
                "process_use_unknown": report.process_use_unknown,
                "sessions_with_finished_runs": report.sessions_with_finished_runs,
                "command_temps_retired": report.command_temps_retired,
                "command_temps_kept": report.command_temps_kept,
                "command_temps_kept_by_reason": report.command_temps_kept_by_reason,
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

    /// Judge one scratch directory of a closed (or replaced) session and move
    /// it into the trash when nothing uses it. The failure of a move that
    /// failed, for the pass's stop rule.
    fn retire_closed_scratch(
        &self,
        record: &ScratchRecord,
        run: &mut RetireRun,
        report: &mut ScratchRetireReport,
    ) -> Option<MoveFailure> {
        report.examined += 1;
        let path = record.path();
        let snapshot = match run.process_use() {
            Ok(snapshot) => snapshot,
            Err(_) => {
                self.remember_scratch_keep(record, "process_use_unknown", report);
                return None;
            }
        };
        if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &path).is_some() {
            self.remember_scratch_keep(record, "process_using_scratch", report);
            return None;
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
                self.remember_scratch_keep(record, "process_use_unknown", report);
                return None;
            }
            if crate::reclaim::open_file_snapshot_blocker_for_worktree(&fresh, &path).is_some() {
                self.remember_scratch_keep(record, "process_using_scratch", report);
                return None;
            }
        }
        // The rename, and the record it rests on, under the lifecycle
        // claim taken without waiting: a foreground command never queues
        // behind this pass.
        let _claim = match self.try_claim_worktree_lifecycle("retire") {
            Ok(Some(claim)) => claim,
            Ok(None) | Err(_) => {
                self.remember_scratch_keep(record, "lifecycle_busy", report);
                return None;
            }
        };
        let unchanged = self
            .session_scratch_records()
            .get(&record.key())
            .is_some_and(|stored| stored.inode == record.inode && stored.device == record.device);
        if !unchanged {
            self.remember_scratch_keep(record, "changed", report);
            return None;
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
                None
            }
            Err(Refusal::Missing) if crate::session_scratch::pool_unavailable(record) => {
                // The volume is not mounted (or an empty mount point stands
                // in for it): the directory is not gone, only out of reach.
                self.remember_scratch_keep(record, "pool_unavailable", report);
                None
            }
            Err(Refusal::Missing) => {
                self.forget_scratch_record(record);
                report.forgotten_missing += 1;
                crate::phase_telemetry::add("session_scratch_missing_forgotten", Duration::ZERO);
                None
            }
            Err(refusal) => {
                self.remember_scratch_keep(record, refusal.code(), report);
                let Refusal::Failed(failure) = refusal else {
                    return None;
                };
                let key = failure.key();
                *report.kept_causes.entry(key.clone()).or_default() += 1;
                crate::phase_telemetry::add_keyed(
                    MOVE_FAILURES_ATTACHMENT,
                    &format!("session_scratch.{key}"),
                    Duration::ZERO,
                );
                Some(failure)
            }
        }
    }

    /// Move the finished command runs of an open session's scratch
    /// directory into the pool's trash: none may be in use (the run's
    /// open-file observation), and each must verify
    /// ([`crate::session_scratch::move_finished_run_to_trash`]). The paced
    /// host deleter unlinks them later, after its own observation. Every
    /// keep is counted by reason. False when `phase` stopped before every
    /// finished run of this record was judged.
    fn retire_finished_command_temps(
        &self,
        record: &ScratchRecord,
        run: &mut RetireRun,
        phase: &mut Phase,
        report: &mut ScratchRetireReport,
    ) -> bool {
        let finished = crate::session_scratch::finished_runs(record, FINISHED_RUNS_PER_PASS);
        if finished.is_empty() {
            return true;
        }
        let _span = crate::phase_telemetry::span("command_temp_retire");
        let Ok(snapshot) = run.process_use() else {
            for _ in &finished {
                keep_command_temp(report, "process_use_unknown");
            }
            crate::phase_telemetry::add("command_temp_kept_process_use_unknown", Duration::ZERO);
            return true;
        };
        for finished_run in &finished {
            if !phase.may_continue(report) {
                return false;
            }
            let path = finished_run.path(record);
            if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &path).is_some() {
                keep_command_temp(report, "in_use");
                crate::phase_telemetry::add("command_temp_kept_in_use", Duration::ZERO);
                phase.note(None, "command_temp_retire_stopped_after_failures");
                continue;
            }
            let moved = crate::session_scratch::move_finished_run_to_trash(
                record,
                finished_run,
                &mut |dir, entry, row| self.record_checkout_trash_row(dir, entry, row),
            );
            let failure = match moved {
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
                    None
                }
                Err(Refusal::Missing) => {
                    // The run is gone (removed by hand): drop its marker and
                    // its alias, which no longer resolves.
                    if let Some(alias) = &finished_run.marker.alias {
                        crate::command_temp::remove_alias(alias, &path);
                    }
                    crate::session_scratch::forget_finished_run(record, finished_run);
                    crate::phase_telemetry::add("command_temp_missing_forgotten", Duration::ZERO);
                    None
                }
                Err(Refusal::Failed(failure)) => {
                    let key = failure.key();
                    keep_command_temp(report, &format!("move_failed.{key}"));
                    crate::phase_telemetry::add("command_temp_kept_move_failed", Duration::ZERO);
                    crate::phase_telemetry::add_keyed(
                        MOVE_FAILURES_ATTACHMENT,
                        &format!("command_temp.{key}"),
                        Duration::ZERO,
                    );
                    Some(failure)
                }
                Err(refusal) => {
                    keep_command_temp(report, refusal.code());
                    crate::phase_telemetry::add("command_temp_kept_refused", Duration::ZERO);
                    None
                }
            };
            phase.note(failure, "command_temp_retire_stopped_after_failures");
        }
        true
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

    /// A finished, non-empty command run of `session`: its run directory.
    fn finished_run(broker: &crate::Broker, session: &crate::Session) -> PathBuf {
        let temp = broker
            .command_temp_for_session(session.id)
            .expect("a command temp directory");
        let run = temp.run_dir();
        std::fs::write(run.join("leftover"), b"left by the command").unwrap();
        broker.finish_command_temp(session.id, Some(temp));
        assert!(run.is_dir());
        run
    }

    /// Restores the per-phase move cap when dropped.
    struct MovesPerPhase;

    impl MovesPerPhase {
        fn set(limit: usize) -> Self {
            super::MOVES_PER_PHASE_FOR_TEST.with(|cell| cell.set(Some(limit)));
            Self
        }
    }

    impl Drop for MovesPerPhase {
        fn drop(&mut self) {
            super::MOVES_PER_PHASE_FOR_TEST.with(|cell| cell.set(None));
        }
    }

    /// Clears the injected rename failure when dropped.
    struct FailingRenames;

    impl FailingRenames {
        fn with(errno: i32) -> Self {
            crate::session_scratch::fail_renames_for_test(Some(errno));
            Self
        }
    }

    impl Drop for FailingRenames {
        fn drop(&mut self) {
            crate::session_scratch::fail_renames_for_test(None);
        }
    }

    #[test]
    fn rotation_starts_after_the_cursor() {
        let items = vec![("a", 1), ("b", 2), ("c", 3)];
        let keys = |items: Vec<(&str, i32)>| items.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        assert_eq!(keys(super::rotate_after(items.clone(), None)), [1, 2, 3]);
        assert_eq!(
            keys(super::rotate_after(items.clone(), Some("a"))),
            [2, 3, 1]
        );
        assert_eq!(
            keys(super::rotate_after(items.clone(), Some("bb"))),
            [3, 1, 2]
        );
        assert_eq!(keys(super::rotate_after(items, Some("z"))), [1, 2, 3]);
    }

    #[test]
    fn errno_causes_are_named_by_kind() {
        let cause =
            |errno| crate::checkout_trash::errno_cause(&std::io::Error::from_raw_os_error(errno));
        assert_eq!(cause(libc::EMFILE), "too_many_open_files");
        assert_eq!(cause(libc::ENFILE), "too_many_open_files");
        assert_eq!(cause(libc::ENOSPC), "no_space");
        assert_eq!(cause(libc::ENOTSUP), "unsupported");
        assert_eq!(cause(libc::EOPNOTSUPP), "unsupported");
        assert_eq!(cause(libc::EINVAL), "unsupported");
        assert_eq!(cause(libc::EACCES), "denied");
        assert_eq!(cause(libc::EPERM), "denied");
        assert_eq!(cause(libc::ENOENT), "not_found");
        assert_eq!(cause(libc::EXDEV), "crosses_device");
        assert_eq!(cause(libc::EHOSTDOWN), "other");
        let failure = |step, cause| crate::session_scratch::MoveFailure { step, cause };
        use crate::session_scratch::MoveStep;
        assert!(failure(MoveStep::Rename, "unsupported").systemic());
        assert!(failure(MoveStep::Open, "too_many_open_files").systemic());
        assert!(failure(MoveStep::TrashRecord, "store").systemic());
        assert!(!failure(MoveStep::Inspect, "io").systemic());
        assert_eq!(failure(MoveStep::Rename, "denied").key(), "rename.denied");
    }

    /// The pass resumes after the record where the last one stopped: a
    /// finished run that is kept (in use) at the front of the order does
    /// not starve the sessions after it, as the measured passes that walked
    /// from the first record every time did.
    #[test]
    fn each_pass_resumes_after_the_last_record_judged() {
        let (_owner, _storage, mut broker) = fixture();
        let mut sessions = (0..3)
            .map(|index| started(&mut broker, &format!("command temp order {index}")).0)
            .collect::<Vec<_>>();
        // The run kept in use belongs to the record judged first.
        let first_id = broker
            .session_scratch_records()
            .into_values()
            .next()
            .unwrap()
            .session_id;
        sessions.sort_by_key(|session| session.id != first_id);
        let kept = finished_run(&broker, &sessions[0]);
        let runs = [
            finished_run(&broker, &sessions[1]),
            finished_run(&broker, &sessions[2]),
        ];
        let _cap = MovesPerPhase::set(1);
        using(&kept);

        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.sessions_with_finished_runs, 3, "{report:#?}");
        assert_eq!(
            report.scratch.command_temps_kept_by_reason.get("in_use"),
            Some(&1),
            "{report:#?}"
        );
        assert!(report.scratch.more_remaining, "{report:#?}");
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");

        assert!(runs.iter().all(|run| !run.exists()), "both reached");
        assert!(kept.is_dir(), "the run in use stays");
    }

    /// A rename the filesystem does not support (no exclusive rename) keeps
    /// the run, counts it under its step and cause, and stops the phase at
    /// once instead of failing every run in turn; the next run retires them
    /// once renames work.
    #[test]
    fn an_unsupported_rename_is_counted_and_stops_the_phase() {
        let (_owner, _storage, mut broker) = fixture();
        let (first, _) = started(&mut broker, "command temp rename first");
        let (second, _) = started(&mut broker, "command temp rename second");
        let runs = [
            finished_run(&broker, &first),
            finished_run(&broker, &second),
        ];
        let failing = FailingRenames::with(libc::ENOTSUP);

        let report = run_retirement(&mut broker);

        assert_eq!(
            report
                .scratch
                .command_temps_kept_by_reason
                .get("move_failed.rename.unsupported"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(report.scratch.command_temps_kept, 1, "one attempt only");
        assert!(report.scratch.stopped_after_failures, "{report:#?}");
        assert!(runs.iter().all(|run| run.is_dir()), "nothing moved");

        drop(failing);
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 2, "{report:#?}");
        assert!(runs.iter().all(|run| !run.exists()));
    }

    /// A rename failing for one entry (not systemic) is retried at most once
    /// per run, and three in a row stop the phase.
    #[test]
    fn failing_moves_in_a_row_stop_the_phase() {
        let (_owner, _storage, mut broker) = fixture();
        let sessions = (0..4)
            .map(|index| started(&mut broker, &format!("command temp denied {index}")).0)
            .collect::<Vec<_>>();
        for session in &sessions {
            finished_run(&broker, session);
        }
        let _failing = FailingRenames::with(libc::EACCES);

        let report = run_retirement(&mut broker);

        assert_eq!(
            report
                .scratch
                .command_temps_kept_by_reason
                .get("move_failed.rename.denied"),
            Some(&3),
            "{report:#?}"
        );
        assert!(report.scratch.stopped_after_failures, "{report:#?}");
    }

    /// A closed session's scratch directory whose trash cannot be opened (a
    /// trash directory open to others) is kept, counted as
    /// `trash_open.not_private`, and retired once the trash is usable.
    #[test]
    fn a_refused_trash_is_counted_by_cause() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch trash refused");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        let pool =
            std::fs::canonicalize(PathBuf::from(&session.worktree_path).parent().unwrap()).unwrap();
        let trash = pool.join(crate::checkout_trash::TRASH_DIR);
        std::fs::create_dir(&trash).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o755)).unwrap();

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept_causes.get("trash_open.not_private"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(report.scratch.kept.get("move_failed"), Some(&1));
        assert!(path.is_dir(), "never moved");

        std::fs::remove_dir(&trash).unwrap();
        let report = run(&mut broker);
        assert_eq!(report.scratch.retired, 1, "{report:#?}");
        assert!(!path.exists());
    }

    /// The session whose scratch record is judged first, then the others.
    fn sessions_in_record_order(
        broker: &mut crate::Broker,
        count: usize,
        label: &str,
    ) -> Vec<crate::Session> {
        let mut sessions = (0..count)
            .map(|index| started(broker, &format!("{label} {index}")).0)
            .collect::<Vec<_>>();
        let order = broker
            .session_scratch_records()
            .into_values()
            .map(|record| record.session_id)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| order.iter().position(|id| *id == session.id));
        sessions
    }

    /// Three runs of one session failing in a row stop the phase, and that
    /// session counts as judged: the next run starts after it and reaches
    /// the session behind it, instead of hitting the same three failures
    /// forever. The stop is not "more to do", so the lane is not kept on
    /// its busy cadence by it.
    #[test]
    fn a_record_that_stops_the_phase_by_failing_is_passed_next_time() {
        let (_owner, _storage, mut broker) = fixture();
        let sessions = sessions_in_record_order(&mut broker, 2, "command temp stuck");
        let stuck = (0..4)
            .map(|_| finished_run(&broker, &sessions[0]))
            .collect::<Vec<_>>();
        let behind = finished_run(&broker, &sessions[1]);
        let names = stuck
            .iter()
            .map(|run| run.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        crate::session_scratch::fail_renames_of_for_test(libc::EACCES, names);
        let _clear = FailingRenames;

        let report = run_retirement(&mut broker);
        assert_eq!(
            report
                .scratch
                .command_temps_kept_by_reason
                .get("move_failed.rename.denied"),
            Some(&3),
            "{report:#?}"
        );
        assert!(report.scratch.stopped_after_failures, "{report:#?}");
        assert!(
            !report.scratch.more_remaining,
            "a stop after failures is not busy: {report:#?}"
        );
        assert!(behind.is_dir(), "not reached in this run");

        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!behind.exists(), "the session behind is reached");
        assert!(
            stuck.iter().all(|run| run.is_dir()),
            "the failing runs stay"
        );
    }

    /// The finished-runs phase keeps at least half the budget after a
    /// closed phase that overran, and never runs past the latest start.
    #[test]
    fn the_finished_runs_phase_has_a_floor() {
        let started = Instant::now();
        let budget = Duration::from_secs(5);
        let latest = started + Duration::from_secs(60);
        assert_eq!(
            super::pending_phase_deadline(started, budget, started, latest),
            started + budget
        );
        let overran = started + Duration::from_secs(12);
        assert_eq!(
            super::pending_phase_deadline(started, budget, overran, latest),
            overran + budget / 2
        );
        let near = started + Duration::from_secs(4);
        assert_eq!(
            super::pending_phase_deadline(started, budget, overran, near),
            near
        );
    }

    /// A scratch directory whose pool root is not there (a volume that is
    /// not mounted) is kept and counted, never forgotten as gone.
    #[test]
    fn an_unmounted_pool_keeps_its_records() {
        let (owner, _storage, mut broker) = fixture();
        let (session, _path) = started(&mut broker, "scratch unmounted");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        let absent = crate::session_scratch::ScratchRecord {
            session_id: session.id,
            pool: std::fs::canonicalize(owner.path())
                .unwrap()
                .join("unmounted-volume")
                .join("pool"),
            entry: format!("s{}-abc", session.id),
            device: 1,
            inode: 1,
            created_ms: 1,
            superseded: false,
            keep: None,
            attempts: 0,
        };
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut crate::session_scratch::ScratchRecords| {
                    records.insert(absent.key(), absent.clone());
                },
            )
            .unwrap();

        let report = run(&mut broker);

        assert_eq!(
            report.scratch.kept.get("pool_unavailable"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(report.scratch.forgotten_missing, 0, "{report:#?}");
        let records = broker.session_scratch_records();
        let kept = records.get(&absent.key()).expect("the record stays");
        assert_eq!(kept.keep.as_deref(), Some("pool_unavailable"));
        assert_eq!(kept.attempts, 1);
    }

    /// Opening a directory on a recorded path: only `ENOENT`, `ELOOP` and
    /// `ENOTDIR` judge the path; anything else (out of descriptors) is a
    /// failed call, counted by cause and systemic.
    #[test]
    fn open_errors_other_than_missing_or_not_a_directory_are_failures() {
        use crate::session_scratch::{MoveStep, Refusal, open_refusal};
        let refusal = |errno| open_refusal(&std::io::Error::from_raw_os_error(errno));
        assert_eq!(refusal(libc::ENOENT), Refusal::Missing);
        assert_eq!(refusal(libc::ELOOP), Refusal::NotARealDirectory);
        assert_eq!(refusal(libc::ENOTDIR), Refusal::NotARealDirectory);
        let Refusal::Failed(failure) = refusal(libc::EMFILE) else {
            panic!("EMFILE is a failure, not a refusal");
        };
        assert_eq!(failure.step, MoveStep::Open);
        assert_eq!(failure.key(), "open.too_many_open_files");
        assert!(failure.systemic());
        assert!(matches!(refusal(libc::EACCES), Refusal::Failed(_)));
    }

    /// When the run's open-file observation failed, nothing is judged: no
    /// sweep of every record as `process_use_unknown`, no record rewritten,
    /// the cursors left alone.
    #[test]
    fn a_failed_observation_judges_nothing() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch no observation");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "lsof refused".into(),
                observed: Vec::new(),
            },
        ));

        let report = run(&mut broker);

        assert!(report.scratch.process_use_unknown, "{report:#?}");
        assert_eq!(report.scratch.examined, 0, "{report:#?}");
        assert!(report.scratch.kept.is_empty(), "{report:#?}");
        assert!(path.is_dir());
        let record = broker
            .session_scratch_records()
            .into_values()
            .next()
            .unwrap();
        assert_eq!(record.keep, None, "no record rewritten");
        nobody_using();
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
