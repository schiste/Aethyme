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
use crate::session_scratch::{FailureScope, MoveFailure, Refusal, ScratchRecord, ScratchRecords};

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
/// failure, [`MoveFailure::systemic`], stops it at once when it affects the
/// whole process, and closes only its pool or volume otherwise, see
/// [`Blocked`]): a failing move is
/// retried on the next run, never in a loop. In the finished-runs phase it
/// counts records whose visit ended on a failed move, and also caps the
/// failed moves in a row within one record, after which the phase passes
/// to the next record.
const MOVE_FAILURES_BEFORE_STOP: usize = 3;
/// Moves attempted per phase per run, at most.
const MOVES_PER_PHASE: usize = 256;
/// The attachment counting failed moves by `<subject>.<step>.<cause>`.
const MOVE_FAILURES_ATTACHMENT: &str = "scratch_move_failures";
/// The longest one pass spends probing the filesystem while it classifies
/// records (listing the done containers of the records due for it, and
/// proving a closed record's directory gone); what is left stays due for
/// the next pass. Reading every session's state is one store query and is
/// not cut short.
const SCRATCH_CLASSIFY_BUDGET: Duration = Duration::from_secs(2);
/// How often every open session's done container is listed whatever the
/// finished-run signals say: the backstop for a marker whose signal could
/// not be written, or that a broker without signals wrote.
const SCRATCH_FULL_LISTING_INTERVAL_MS: i64 = 60 * 60 * 1000;
/// How far past the lane's own deadline the scratch pass may start new
/// work. With an item already in flight (at most one fresh open-file
/// observation of a replaced directory, 10 s, and one rename), a retirement
/// run ends at most this plus 10 s plus one move past its deadline.
pub(crate) const SCRATCH_OVERRUN_MAX: Duration = Duration::from_secs(20);

#[cfg(test)]
thread_local! {
    static MOVES_PER_PHASE_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static CLASSIFY_BUDGET_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

fn moves_per_phase() -> usize {
    #[cfg(test)]
    if let Some(limit) = MOVES_PER_PHASE_FOR_TEST.with(std::cell::Cell::get) {
        return limit;
    }
    MOVES_PER_PHASE
}

fn classify_budget() -> Duration {
    #[cfg(test)]
    if let Some(budget) = CLASSIFY_BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        return budget;
    }
    SCRATCH_CLASSIFY_BUDGET
}

/// Where each phase of the last scratch retirement pass stopped: the key of
/// the last record it judged, or nothing once a phase went round; and which
/// open records' done containers are due for a listing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ScratchRetireCursor {
    /// The last record whose done container was listed: the next pass lists
    /// the due records after it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    classify: Option<String>,
    /// Open records whose done container the next pass lists: signaled
    /// ones, every open one during a full listing lap, and the ones whose
    /// last listing showed a finished run. A record leaves only after a
    /// listing showed none.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    listing_due: std::collections::BTreeSet<String>,
    /// When the last full listing lap started (epoch ms); none yet means
    /// one is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    full_listing_started_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_temps: Option<String>,
    /// Per record key, the name of the last finished run judged when the
    /// record's last visit ended before it went through all its runs: the
    /// next visit starts after it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    command_temp_runs: std::collections::BTreeMap<String, String>,
}

/// How one record's visit in the finished-runs phase ended.
#[derive(Debug, Default)]
struct RunsVisit {
    /// The record counts as judged: the phase may go on to the next one.
    judged: bool,
    /// The run cursor to store for the record (nothing once it went round).
    run_cursor: Option<String>,
    /// The visit ended on a failed move.
    ended_failing: bool,
    /// The verified listing found no finished run: the record leaves the
    /// listing-due set.
    found_none: bool,
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

/// What the scratch pass does with a record, from its session's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordClass {
    /// Replaced, or its session closed, cleaned or gone: retire it.
    Retire,
    /// Its session is open: only its finished runs are retired.
    Open,
    /// The session could not be read: kept as `session_unknown`.
    Unknown,
}

/// The class of `record` given its session's status (`Ok(None)`: no such
/// session; `Err`: the store could not answer).
fn record_class(
    record: &ScratchRecord,
    status: Result<Option<crate::SessionStatus>, ()>,
) -> RecordClass {
    if record.superseded {
        return RecordClass::Retire;
    }
    match status {
        Ok(Some(status)) if !status.is_closed() => RecordClass::Open,
        Ok(_) => RecordClass::Retire,
        Err(()) => RecordClass::Unknown,
    }
}

/// Whether a full listing lap is due at `now_ms`, the last one having
/// started at `started_ms` (a clock that went back starts one too).
fn full_listing_due(started_ms: Option<i64>, now_ms: i64) -> bool {
    started_ms.is_none_or(|started| {
        now_ms < started || now_ms - started >= SCRATCH_FULL_LISTING_INTERVAL_MS
    })
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

    /// The end of one record's visit in the finished-runs phase: a visit
    /// that ended on a failed move counts toward the stop rule, any other
    /// resets it.
    fn note_record(&mut self, ended_failing: bool, stop_counter: &'static str) {
        let before = self.failures_in_a_row;
        self.failures_in_a_row = if ended_failing {
            self.failures_in_a_row + 1
        } else {
            0
        };
        if before < MOVE_FAILURES_BEFORE_STOP && self.failures_in_a_row >= MOVE_FAILURES_BEFORE_STOP
        {
            crate::phase_telemetry::add(stop_counter, Duration::ZERO);
        }
    }

    /// A systemic failure: the phase stops at once.
    fn stop_systemic(&mut self, report: &mut ScratchRetireReport, stop_counter: &'static str) {
        if self.failures_in_a_row < MOVE_FAILURES_BEFORE_STOP {
            crate::phase_telemetry::add(stop_counter, Duration::ZERO);
        }
        self.failures_in_a_row = MOVE_FAILURES_BEFORE_STOP;
        report.stopped_after_failures = true;
        self.stopped = true;
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
    /// (one whose cause affects the whole process, `stop_now`, stops the
    /// phase at once); anything else resets it.
    fn note(&mut self, failure: Option<MoveFailure>, stop_now: bool, stop_counter: &'static str) {
        let before = self.failures_in_a_row;
        self.failures_in_a_row = match failure {
            Some(_) if stop_now => MOVE_FAILURES_BEFORE_STOP,
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

/// The pools and volumes a systemic failure closed for the rest of a pass:
/// a failure of one pool's trash or its store row
/// ([`crate::session_scratch::FailureScope::Pool`]) skips that pool's
/// records, a full, read-only or unsupported volume
/// ([`crate::session_scratch::FailureScope::Volume`]) skips the records on
/// that device. Other pools and volumes go on.
#[derive(Debug, Default)]
struct Blocked {
    pools: std::collections::BTreeSet<PathBuf>,
    devices: std::collections::BTreeSet<u64>,
}

impl Blocked {
    /// Close `record`'s pool or volume after the systemic `failure`; false
    /// when the failure affects the whole process (nothing is closed and
    /// the caller stops the phase).
    fn close(&mut self, failure: MoveFailure, record: &ScratchRecord) -> bool {
        match failure.scope() {
            FailureScope::Process => false,
            FailureScope::Pool => {
                crate::phase_telemetry::add("session_scratch_retire_pool_closed", Duration::ZERO);
                self.pools.insert(record.pool.clone());
                true
            }
            FailureScope::Volume => {
                crate::phase_telemetry::add("session_scratch_retire_volume_closed", Duration::ZERO);
                self.devices.insert(record.device);
                true
            }
        }
    }

    /// Whether `record` is in a pool or on a volume closed for this pass:
    /// it is passed (counted, and retried on a later lap).
    fn skips(&self, record: &ScratchRecord, report: &mut ScratchRetireReport) -> bool {
        let skip = self.pools.contains(&record.pool) || self.devices.contains(&record.device);
        if skip {
            report.skipped_closed_scope += 1;
            crate::phase_telemetry::add("session_scratch_retire_skipped_closed", Duration::ZERO);
        }
        skip
    }
}

/// The run cursor to keep for a record: the last run judged in this visit,
/// else the cursor the visit started from.
fn resume_after(last_judged: Option<&str>, after: Option<&str>) -> Option<String> {
    last_judged.or(after).map(str::to_owned)
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
    /// The classification stopped at its budget with records still due for
    /// a listing; the next pass goes on.
    pub classify_incomplete: bool,
    /// Done containers listed (records due for a listing reached).
    pub listed: usize,
    /// Closed records whose directory was probed for being gone.
    pub probed: usize,
    /// Finished-run signals read (and cleared) this pass.
    pub signals: usize,
    /// Records still due for a listing after the pass.
    pub listing_due: usize,
    /// This pass started a full listing lap (every open record due).
    pub full_listing: bool,
    /// The run's open-file observation failed: nothing was judged.
    pub process_use_unknown: bool,
    /// Records passed because a systemic failure closed their pool or
    /// volume for this pass ([`Blocked`]); retried on a later lap.
    pub skipped_closed_scope: usize,
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
                Err(Refusal::Missing) if crate::session_scratch::pool_unavailable(&record) => {
                    // The volume is not mounted: the directory is out of
                    // reach, not gone. Keep the record (the retirement lane
                    // applies the same rule) and make nothing new.
                    crate::phase_telemetry::add(
                        "session_scratch_unavailable_pool_unavailable",
                        Duration::ZERO,
                    );
                    return SessionScratch::unavailable(format!(
                        "the volume holding the recorded scratch directory {} is not available; the record is kept",
                        record.path().display()
                    ));
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

    /// The class of every record, by key: the sessions' states in one
    /// store read (`BrokerStore::session_statuses`), or one
    /// lookup per record when that read fails (counted), which gives the
    /// same answers.
    fn scratch_record_classes<'a>(
        &self,
        records: &'a crate::session_scratch::ScratchRecords,
    ) -> std::collections::BTreeMap<&'a str, RecordClass> {
        let _span = crate::phase_telemetry::span("session_scratch_retire_session_states");
        let ids = records
            .values()
            .map(|record| record.session_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let statuses = self.store_ref().session_statuses(&ids).ok();
        if statuses.is_none() {
            crate::phase_telemetry::add(
                "session_scratch_retire_session_states_fallback",
                Duration::ZERO,
            );
        }
        records
            .iter()
            .map(|(key, record)| {
                let status = match &statuses {
                    Some(statuses) => Ok(statuses.get(&record.session_id).copied()),
                    None => self.scratch_session_status(record.session_id),
                };
                (key.as_str(), record_class(record, status))
            })
            .collect()
    }

    /// One session's status by its own store lookup (`Ok(None)`: no such
    /// session).
    fn scratch_session_status(&self, session_id: i64) -> Result<Option<crate::SessionStatus>, ()> {
        match self.store_ref().session(session_id) {
            Ok(session) => Ok(Some(session.status)),
            Err(crate::BrokerError::SessionNotFound(_)) => Ok(None),
            Err(_) => Err(()),
        }
    }

    /// Retire scratch directories whose session is closed (or gone), and
    /// those replaced by a fresh one, then the finished command runs of open
    /// sessions, within `budget`. See the module documentation for the rules.
    ///
    /// On the busiest repository a pass judged 650 records in key order
    /// from the first one, with a descriptor walk from `/` per open session,
    /// and its 5 s ran out before the newest sessions (whose ids sort last)
    /// were reached: six passes in eight examined nothing. A cheap
    /// classification then came first, but it still listed the done
    /// container of every open session (about 600, nearly all without a
    /// finished run): 2 to 3.4 s, never a whole lap in one pass. Now the
    /// sessions' states are one store read, and a done container is listed
    /// only when its record is due: a finished-run signal names it
    /// ([`crate::session_scratch::signal_finished_run`]), its last listing
    /// showed a finished run, or the hourly full listing lap reached it. The
    /// run's open-file observation is taken outside the budget, the closed
    /// directories and the command runs each get a share of it, and each
    /// phase resumes after the record where the previous pass stopped.
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
        let mut next = ScratchRetireCursor {
            command_temp_runs: cursor
                .command_temp_runs
                .iter()
                .filter(|(key, _)| records.contains_key(*key))
                .map(|(key, run)| (key.clone(), run.clone()))
                .collect(),
            ..ScratchRetireCursor::default()
        };
        // Classify every record from its session's state (one store read,
        // no filesystem access), then probe the filesystem only where there
        // can be work, within the classification budget: closed records
        // whose directory may be gone, and the done containers of the open
        // records due for a listing (always at least one listing).
        let (mut closed, mut pending, mut due, containers) = {
            let _classify = crate::phase_telemetry::span("session_scratch_retire_classify");
            let classes = self.scratch_record_classes(&records);
            let mut closed = Vec::new();
            let mut open = std::collections::BTreeSet::<&str>::new();
            for (key, record) in &records {
                match classes.get(key.as_str()) {
                    Some(RecordClass::Retire) => closed.push((key.as_str(), record)),
                    Some(RecordClass::Open) => {
                        open.insert(key.as_str());
                    }
                    Some(RecordClass::Unknown) | None => {
                        self.remember_scratch_keep(record, "session_unknown", &mut report);
                    }
                }
            }

            // What is due for a listing: what the last pass left due, every
            // open record when a full lap starts, and every record a
            // finished-run signal names (one listing of each pool's
            // container, which also yields the unrecorded entries).
            let mut due = cursor
                .listing_due
                .iter()
                .filter(|key| open.contains(key.as_str()))
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            next.full_listing_started_ms = cursor.full_listing_started_ms;
            let now_ms = crate::clock::epoch_ms();
            if full_listing_due(cursor.full_listing_started_ms, now_ms) {
                crate::phase_telemetry::add("session_scratch_retire_full_listing", Duration::ZERO);
                report.full_listing = true;
                due.extend(open.iter().map(|key| (*key).to_owned()));
                next.full_listing_started_ms = Some(now_ms);
            }
            let containers = {
                let _signals = crate::phase_telemetry::span("session_scratch_retire_signals");
                pools
                    .keys()
                    .map(|pool| (pool.clone(), crate::session_scratch::list_container(pool)))
                    .collect::<std::collections::BTreeMap<_, _>>()
            };
            for (pool, listing) in &containers {
                for entry in &listing.signals {
                    report.signals += 1;
                    let key = pool
                        .join(crate::session_scratch::SCRATCH_CONTAINER)
                        .join(entry)
                        .to_string_lossy()
                        .into_owned();
                    if open.contains(key.as_str()) {
                        due.insert(key);
                    }
                }
            }
            // A signal is cleared only once what it made due is stored, and
            // before any listing: a marker written after its container was
            // listed signals again (its writer writes the signal after the
            // marker), and one written before is seen by the listing.
            if report.signals > 0 {
                let stored = self.update_meta_json(
                    SESSION_SCRATCH_CURSOR_KEY,
                    |stored: &mut ScratchRetireCursor| {
                        stored.listing_due.extend(due.iter().cloned());
                    },
                );
                if stored.is_ok() {
                    for (pool, listing) in &containers {
                        if !listing.signals.is_empty() {
                            crate::session_scratch::clear_finished_signals(pool, &listing.signals);
                        }
                    }
                } else {
                    crate::phase_telemetry::add(
                        "session_scratch_retire_signals_kept",
                        Duration::ZERO,
                    );
                }
            }

            let deadline = (Instant::now() + classify_budget()).min(latest);
            // A closed record whose directory is proven gone (the same proof
            // as a move's: the walk without links finds no entry, and the
            // pool is on a mounted volume) has nothing to move: forget it now,
            // with no open-file observation and no lifecycle claim. On a
            // volume that is away it stays, and is judged as before.
            closed.retain(|(_, record)| {
                if Instant::now() >= deadline {
                    return true;
                }
                report.probed += 1;
                match crate::session_scratch::verify(record) {
                    Err(Refusal::Missing) if !crate::session_scratch::pool_unavailable(record) => {
                        self.forget_scratch_record(record);
                        report.forgotten_missing += 1;
                        crate::phase_telemetry::add(
                            "session_scratch_missing_forgotten_early",
                            Duration::ZERO,
                        );
                        false
                    }
                    _ => true,
                }
            });

            // List the due records, after the last one listed: a record with
            // a finished run stays due (the finished-runs phase takes it), one
            // without leaves.
            let mut pending = Vec::new();
            let mut without_runs = Vec::new();
            let mut last_listed = None;
            let ordered = rotate_after(
                due.iter().map(|key| (key.as_str(), ())).collect(),
                cursor.classify.as_deref(),
            );
            for (key, ()) in ordered {
                if report.listed > 0 && Instant::now() >= deadline {
                    report.classify_incomplete = true;
                    crate::phase_telemetry::add(
                        "session_scratch_retire_classify_stopped",
                        Duration::ZERO,
                    );
                    break;
                }
                let Some((key, record)) = records.get_key_value(key) else {
                    continue;
                };
                report.listed += 1;
                last_listed = Some(key.clone());
                if crate::session_scratch::may_have_finished_runs(record) {
                    pending.push((key.as_str(), record));
                } else {
                    without_runs.push(key.as_str());
                }
            }
            for key in without_runs {
                due.remove(key);
            }
            next.classify = last_listed.or_else(|| cursor.classify.clone());
            (closed, pending, due, containers)
        };
        // Both phases take their records in key order, each from its own
        // cursor.
        //
        // Invariant: every closed (or replaced) record is classified on
        // every pass, so the closed phase reaches each one within as many
        // passes as there are closed records. A finished run's record is due
        // from the pass after its marker was written (its signal), or within
        // the hour (the full listing lap) when the signal could not be
        // written; a due record is listed within as many passes as there are
        // due records (at least one listing per pass, resuming after the last
        // one), stays due while its listing shows a finished run, and is
        // then judged by the finished-runs phase within as many passes as
        // there are records with finished runs, plus one per finished run.
        // All of this given passes whose open-file observation succeeds and
        // whose moves do not fail with a cause that affects the whole
        // process (out of descriptors). A failure confined to one pool's
        // trash or store row, or to one volume, closes only that pool or
        // volume for the pass; its records are retried on a later lap. A
        // record cut short by the deadline or a move cap keeps a run cursor
        // past the runs it judged.
        closed.sort_by(|left, right| left.0.cmp(right.0));
        pending.sort_by(|left, right| left.0.cmp(right.0));
        closed = rotate_after(closed, cursor.closed.as_deref());
        pending = rotate_after(pending, cursor.command_temps.as_deref());
        report.sessions_with_finished_runs = pending.len();
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
            // A systemic failure confined to one pool or volume closes it
            // for the rest of the pass; only one that affects the whole
            // process stops a phase.
            let mut blocked = Blocked::default();

            let mut phase = Phase::new(closed_deadline);
            for (key, record) in &closed {
                if blocked.skips(record, &mut report) {
                    phase.last_done = Some((*key).to_owned());
                    continue;
                }
                if !phase.may_continue(&mut report) {
                    break;
                }
                let failure = self.retire_closed_scratch(record, run, &mut report);
                let stop_now = failure
                    .is_some_and(|failure| failure.systemic() && !blocked.close(failure, record));
                phase.note(
                    failure,
                    stop_now,
                    "session_scratch_retire_stopped_after_failures",
                );
                phase.last_done = Some((*key).to_owned());
            }
            let closed_cursor = phase.cursor(cursor.closed.clone());

            let mut phase = Phase::new(pending_phase_deadline(
                started,
                budget,
                Instant::now(),
                latest,
            ));
            for (key, record) in &pending {
                if blocked.skips(record, &mut report) {
                    phase.last_done = Some((*key).to_owned());
                    continue;
                }
                if !phase.may_start(&mut report) {
                    break;
                }
                let after = next.command_temp_runs.get(*key).cloned();
                let visit = self.retire_finished_command_temps(
                    record,
                    after.as_deref(),
                    run,
                    &mut phase,
                    &mut blocked,
                    &mut report,
                );
                match visit.run_cursor {
                    Some(run_name) => {
                        next.command_temp_runs.insert((*key).to_owned(), run_name);
                    }
                    None => {
                        next.command_temp_runs.remove(*key);
                    }
                }
                // A record left before all its runs were judged (the
                // deadline, the move cap, or a failure that affects the
                // whole process) is not judged: the next pass starts with
                // it, at its run cursor.
                if !visit.judged {
                    break;
                }
                // A verified listing that found no finished run (the hint
                // listing saw a name the verified one refuses) leaves the
                // due set: a new marker signals again.
                if visit.found_none {
                    due.remove(*key);
                }
                phase.note_record(
                    visit.ended_failing,
                    "command_temp_retire_stopped_after_failures",
                );
                phase.last_done = Some((*key).to_owned());
            }
            let pending_cursor = phase.cursor(cursor.command_temps.clone());

            next.closed = closed_cursor;
            next.command_temps = pending_cursor;
        }
        // Records still due for a listing are more to do only when this pass
        // retired or forgot something: a volume where listing is always slow
        // but nothing is retired stays on the normal cadence.
        if report.classify_incomplete
            && report.retired + report.forgotten_missing + report.command_temps_retired > 0
        {
            report.more_remaining = true;
        }
        report.listing_due = due.len();
        next.listing_due = due;
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
                "skipped_closed_scope": report.skipped_closed_scope,
                "sessions_with_finished_runs": report.sessions_with_finished_runs,
                "command_temps_retired": report.command_temps_retired,
                "command_temps_kept": report.command_temps_kept,
                "command_temps_kept_by_reason": report.command_temps_kept_by_reason,
                "listed": report.listed,
                "probed": report.probed,
                "signals": report.signals,
                "listing_due": report.listing_due,
                "full_listing": report.full_listing,
            }),
        );
        for (pool, recorded) in &pools {
            let Some(listing) = containers.get(pool) else {
                continue;
            };
            for name in
                crate::session_scratch::unrecorded_in(listing, recorded, UNRECORDED_NAMES_MAX)
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
    /// keep is counted by reason.
    ///
    /// Runs are taken in name order after `after` (the record's run cursor),
    /// so a run that fails every time cannot hide the runs behind it: a
    /// failed move that is not systemic passes to the next run, and only
    /// [`MOVE_FAILURES_BEFORE_STOP`] failed moves in a row pass to the next
    /// record (the record is judged, its run cursor kept past them). A
    /// systemic failure, the deadline or the move cap leaves the record
    /// unjudged, its run cursor past the runs it judged. A systemic failure
    /// confined to the record's pool or volume closes it for the pass
    /// (`blocked`) and judges the record, its run cursor before the failing
    /// run; only one that affects the whole process stops the phase.
    fn retire_finished_command_temps(
        &self,
        record: &ScratchRecord,
        after: Option<&str>,
        run: &mut RetireRun,
        phase: &mut Phase,
        blocked: &mut Blocked,
        report: &mut ScratchRetireReport,
    ) -> RunsVisit {
        if after.is_some() {
            crate::phase_telemetry::add("command_temp_run_cursor_resumed", Duration::ZERO);
        }
        let listing =
            crate::session_scratch::finished_runs_after(record, after, FINISHED_RUNS_PER_PASS);
        let finished = listing.runs;
        if finished.is_empty() {
            return RunsVisit {
                judged: true,
                found_none: true,
                ..RunsVisit::default()
            };
        }
        let _span = crate::phase_telemetry::span("command_temp_retire");
        let Ok(snapshot) = run.process_use() else {
            for _ in &finished {
                keep_command_temp(report, "process_use_unknown");
            }
            crate::phase_telemetry::add("command_temp_kept_process_use_unknown", Duration::ZERO);
            return RunsVisit {
                judged: true,
                run_cursor: after.map(str::to_owned),
                ended_failing: false,
                found_none: false,
            };
        };
        let mut failures_in_a_row = 0_usize;
        let mut last_judged: Option<&str> = None;
        for finished_run in &finished {
            if !phase.may_continue(report) {
                return RunsVisit {
                    judged: false,
                    run_cursor: resume_after(last_judged, after),
                    ended_failing: false,
                    found_none: false,
                };
            }
            let path = finished_run.path(record);
            if crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, &path).is_some() {
                keep_command_temp(report, "in_use");
                crate::phase_telemetry::add("command_temp_kept_in_use", Duration::ZERO);
                failures_in_a_row = 0;
                last_judged = Some(finished_run.name.as_str());
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
            match failure {
                Some(failure) if failure.systemic() && blocked.close(failure, record) => {
                    // Confined to this record's pool or volume: the record
                    // is judged (its run cursor before the failing run, so
                    // a later lap retries it first) and the phase goes on
                    // with the other pools.
                    return RunsVisit {
                        judged: true,
                        run_cursor: resume_after(last_judged, after),
                        ended_failing: true,
                        found_none: false,
                    };
                }
                Some(failure) if failure.systemic() => {
                    phase.stop_systemic(report, "command_temp_retire_stopped_after_failures");
                    return RunsVisit {
                        judged: false,
                        run_cursor: resume_after(last_judged, after),
                        ended_failing: true,
                        found_none: false,
                    };
                }
                Some(_) => failures_in_a_row += 1,
                None => failures_in_a_row = 0,
            }
            last_judged = Some(finished_run.name.as_str());
            if failures_in_a_row >= MOVE_FAILURES_BEFORE_STOP {
                crate::phase_telemetry::add(
                    "command_temp_record_passed_after_failures",
                    Duration::ZERO,
                );
                return RunsVisit {
                    judged: true,
                    run_cursor: resume_after(last_judged, after),
                    ended_failing: true,
                    found_none: false,
                };
            }
        }
        RunsVisit {
            judged: true,
            run_cursor: if listing.complete {
                None
            } else {
                resume_after(last_judged, after)
            },
            ended_failing: failures_in_a_row > 0,
            found_none: false,
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
        use crate::session_scratch::FailureScope;
        assert_eq!(
            failure(MoveStep::Open, "too_many_open_files").scope(),
            FailureScope::Process
        );
        assert_eq!(
            failure(MoveStep::TrashOpen, "too_many_open_files").scope(),
            FailureScope::Process
        );
        assert_eq!(
            failure(MoveStep::TrashOpen, "not_private").scope(),
            FailureScope::Pool
        );
        assert_eq!(
            failure(MoveStep::TrashRecord, "store").scope(),
            FailureScope::Pool
        );
        assert_eq!(
            failure(MoveStep::Rename, "no_space").scope(),
            FailureScope::Volume
        );
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
    /// the run, counts it under its step and cause, and closes its volume
    /// for the pass instead of failing every run on it in turn; the next
    /// run retires them once renames work.
    #[test]
    fn an_unsupported_rename_is_counted_and_closes_its_volume() {
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
        assert_eq!(report.scratch.skipped_closed_scope, 1, "{report:#?}");
        assert!(!report.scratch.stopped_after_failures, "{report:#?}");
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

    /// The run names of `runs`, in the order the finished-runs phase takes
    /// them.
    fn run_names(runs: &[PathBuf]) -> Vec<String> {
        runs.iter()
            .map(|run| run.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// Runs failing every time cannot hide the runs behind them: three
    /// failed moves in a row in one record pass to the next record (a later
    /// session is reached in the same pass, and the phase does not stop),
    /// and the next pass starts that record after the failing runs, so its
    /// fourth run is retired.
    #[test]
    fn runs_failing_every_time_do_not_hide_the_runs_behind_them() {
        let (_owner, _storage, mut broker) = fixture();
        let sessions = sessions_in_record_order(&mut broker, 2, "command temp stuck");
        let mut stuck = (0..4)
            .map(|_| finished_run(&broker, &sessions[0]))
            .collect::<Vec<_>>();
        stuck.sort_by_key(|run| run.file_name().unwrap().to_owned());
        let fine = stuck.pop().unwrap();
        let behind = finished_run(&broker, &sessions[1]);
        crate::session_scratch::fail_renames_of_for_test(libc::EACCES, run_names(&stuck));
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
        assert!(!report.scratch.stopped_after_failures, "{report:#?}");
        assert!(
            !report.scratch.more_remaining,
            "failures are not busy: {report:#?}"
        );
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!behind.exists(), "the later session is reached");
        assert!(fine.is_dir(), "behind the failing runs in this pass");

        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!fine.exists(), "the run behind the failing ones is retired");
        assert!(
            stuck.iter().all(|run| run.is_dir()),
            "the failing runs stay"
        );
    }

    /// A failure that affects the whole process (out of descriptors) at a
    /// record's first run stops the phase without judging the record: the
    /// phase cursor stays before it.
    #[test]
    fn a_process_wide_stop_does_not_judge_the_record() {
        let (_owner, _storage, mut broker) = fixture();
        let sessions = sessions_in_record_order(&mut broker, 2, "command temp systemic");
        let first = finished_run(&broker, &sessions[0]);
        let second = finished_run(&broker, &sessions[1]);
        let failing = FailingRenames::with(libc::EMFILE);

        let report = run_retirement(&mut broker);
        assert!(report.scratch.stopped_after_failures, "{report:#?}");
        assert_eq!(report.scratch.command_temps_kept, 1, "{report:#?}");
        let cursor: super::ScratchRetireCursor =
            broker.meta_json(super::SESSION_SCRATCH_CURSOR_KEY);
        assert_eq!(cursor.command_temps, None, "not judged: {cursor:?}");
        assert!(cursor.command_temp_runs.is_empty(), "{cursor:?}");

        drop(failing);
        let _cap = MovesPerPhase::set(1);
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!first.exists(), "the record that stopped comes first");
        assert!(second.is_dir());
    }

    /// A systemic failure confined to one pool (its trash refused) closes
    /// that pool for the pass: its other records are passed and counted,
    /// another pool's finished runs are retired in the same pass, and the
    /// failing pool's records are retried on a later lap.
    #[test]
    fn a_failing_pool_does_not_hold_back_another_pool() {
        let (owner, _storage, mut broker) = fixture();
        let pools = std::fs::canonicalize(owner.path()).unwrap().join("pools");
        let (failing_pool, other_pool) = (pools.join("a-pool"), pools.join("b-pool"));
        std::fs::create_dir_all(&failing_pool).unwrap();
        std::fs::create_dir_all(&other_pool).unwrap();
        let sessions = (0..3)
            .map(|index| {
                broker
                    .start_worktree(&format!("command temp pools {index}"), None)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        // Two sessions' scratch directories in the failing pool (sorting
        // first), one in the other pool.
        let records = [&failing_pool, &failing_pool, &other_pool]
            .iter()
            .zip(&sessions)
            .map(|(pool, session)| crate::session_scratch::create(pool, session.id).unwrap())
            .collect::<Vec<_>>();
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |stored: &mut crate::session_scratch::ScratchRecords| {
                    stored.clear();
                    for record in &records {
                        stored.insert(record.key(), record.clone());
                    }
                },
            )
            .unwrap();
        let runs = sessions
            .iter()
            .map(|session| finished_run(&broker, session))
            .collect::<Vec<_>>();
        assert!(runs[0].starts_with(&failing_pool) && runs[2].starts_with(&other_pool));
        let trash = failing_pool.join(crate::checkout_trash::TRASH_DIR);
        std::fs::create_dir(&trash).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o755)).unwrap();

        let report = run_retirement(&mut broker);

        assert_eq!(
            report
                .scratch
                .command_temps_kept_by_reason
                .get("move_failed.trash_open.not_private"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(report.scratch.skipped_closed_scope, 1, "{report:#?}");
        assert!(!report.scratch.stopped_after_failures, "{report:#?}");
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(
            !runs[2].exists(),
            "the other pool is reached in the same pass"
        );
        assert!(runs[0].is_dir() && runs[1].is_dir());

        std::fs::remove_dir(&trash).unwrap();
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.command_temps_retired, 2, "{report:#?}");
        assert!(
            runs.iter().all(|run| !run.exists()),
            "retried on a later lap"
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

    /// Whether the volume of an [`absent_record`] is mounted.
    enum Volume {
        Away,
        Mounted,
    }

    /// A record of `session_id` whose pool, under `owner`, does not exist:
    /// on `owner`'s device when the volume is mounted (the pool was
    /// removed), on another device when it is away (the nearest existing
    /// ancestor then stands on another volume, as a mount point does).
    fn absent_record(
        owner: &Path,
        session_id: i64,
        volume: Volume,
    ) -> crate::session_scratch::ScratchRecord {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::canonicalize(owner).unwrap();
        let device = std::fs::metadata(&owner).unwrap().dev();
        crate::session_scratch::ScratchRecord {
            session_id,
            pool: owner.join("removed-volume").join("pool"),
            entry: format!("s{session_id}-abc"),
            device: match volume {
                Volume::Away => device.wrapping_add(1000),
                Volume::Mounted => device,
            },
            inode: 1,
            created_ms: 1,
            superseded: false,
            keep: None,
            attempts: 0,
        }
    }

    /// Make `record` the only scratch record.
    fn only_record(broker: &crate::Broker, record: &crate::session_scratch::ScratchRecord) {
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut crate::session_scratch::ScratchRecords| {
                    records.clear();
                    records.insert(record.key(), record.clone());
                },
            )
            .unwrap();
    }

    /// A closed session's directory whose pool was removed from a volume
    /// that is mounted (its nearest existing ancestor is on the recorded
    /// device) is really gone: the record is forgotten, not kept forever.
    #[test]
    fn a_removed_pool_on_a_mounted_volume_is_forgotten() {
        let (owner, _storage, mut broker) = fixture();
        let (session, _path) = started(&mut broker, "scratch pool removed");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        let removed = absent_record(owner.path(), session.id, Volume::Mounted);
        only_record(&broker, &removed);

        let report = run(&mut broker);

        assert_eq!(report.scratch.forgotten_missing, 1, "{report:#?}");
        assert_eq!(report.scratch.kept.get("pool_unavailable"), None);
        assert!(broker.session_scratch_records().is_empty());
    }

    /// An open session's record of a directory on a volume that is away is
    /// kept, and no fresh directory is made in its place; once the volume
    /// is mounted and the directory is really gone, the record is forgotten
    /// and a fresh directory made.
    #[test]
    fn an_open_session_keeps_its_record_while_its_volume_is_away() {
        let (owner, _storage, mut broker) = fixture();
        let (session, _path) = started(&mut broker, "scratch volume away");
        let away = absent_record(owner.path(), session.id, Volume::Away);
        only_record(&broker, &away);

        let scratch = broker.ensure_session_scratch(session.id);

        assert_eq!(scratch.path, None, "{}", scratch.note);
        let records = broker.session_scratch_records();
        assert_eq!(records.len(), 1, "{records:#?}");
        assert!(records.contains_key(&away.key()), "the record is kept");

        let removed = absent_record(owner.path(), session.id, Volume::Mounted);
        only_record(&broker, &removed);
        let scratch = broker.ensure_session_scratch(session.id);
        let fresh = scratch.path.unwrap_or_else(|| panic!("{}", scratch.note));
        assert!(fresh.is_dir());
        let records = broker.session_scratch_records();
        assert_eq!(records.len(), 1, "{records:#?}");
        assert!(!records.contains_key(&removed.key()), "forgotten");
    }

    /// Restores the classification budget when dropped.
    struct ClassifyBudget;

    impl ClassifyBudget {
        fn set(budget: Duration) -> Self {
            super::CLASSIFY_BUDGET_FOR_TEST.with(|cell| cell.set(Some(budget)));
            Self
        }
    }

    impl Drop for ClassifyBudget {
        fn drop(&mut self) {
            super::CLASSIFY_BUDGET_FOR_TEST.with(|cell| cell.set(None));
        }
    }

    /// With no classification budget, each pass lists one due record and
    /// the next pass resumes after it: every finished run is reached within
    /// as many passes as there are due records. The busy cadence holds while
    /// a pass retires something, not merely because listing did not finish;
    /// a record whose listing shows nothing leaves the due set, and once
    /// none is left a pass lists nothing.
    #[test]
    fn a_listing_cut_at_its_budget_resumes_and_reaches_every_finished_run() {
        let (_owner, _storage, mut broker) = fixture();
        let sessions = sessions_in_record_order(&mut broker, 3, "scratch listing cut");
        let runs = sessions
            .iter()
            .map(|session| finished_run(&broker, session))
            .collect::<Vec<_>>();
        let _budget = ClassifyBudget::set(Duration::ZERO);

        for (pass, run) in runs.iter().enumerate() {
            let report = run_retirement(&mut broker);
            assert!(report.scratch.classify_incomplete, "{report:#?}");
            assert_eq!(report.scratch.listed, 1, "{report:#?}");
            assert_eq!(
                report.scratch.command_temps_retired, 1,
                "pass {pass}: {report:#?}"
            );
            assert!(report.scratch.more_remaining, "{report:#?}");
            assert!(!run.exists(), "pass {pass} reaches the next record");
        }
        // Each record is listed once more, shows nothing and leaves.
        for _ in 0..runs.len() {
            let report = run_retirement(&mut broker);
            assert_eq!(report.scratch.listed, 1, "{report:#?}");
            assert_eq!(report.scratch.command_temps_retired, 0, "{report:#?}");
            assert!(!report.scratch.more_remaining, "{report:#?}");
        }
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.listed, 0, "{report:#?}");
        assert_eq!(report.scratch.listing_due, 0, "{report:#?}");
        assert!(!report.scratch.classify_incomplete, "{report:#?}");
    }

    /// A pass whose open-file observation failed judges nothing, and the
    /// finished run its consumed signal made due stays due: the next pass
    /// retires it with no new signal.
    #[test]
    fn a_failed_observation_keeps_a_finished_run_due() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, scratch) = started(&mut broker, "scratch due unobserved");
        let run = finished_run(&broker, &session);
        let pool = scratch.parent().unwrap().parent().unwrap().to_path_buf();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "lsof refused".into(),
                observed: Vec::new(),
            },
        ));

        let report = run_retirement(&mut broker);
        assert!(report.scratch.process_use_unknown, "{report:#?}");
        assert_eq!(report.scratch.command_temps_retired, 0, "{report:#?}");
        assert_eq!(report.scratch.listing_due, 1, "{report:#?}");
        assert!(run.is_dir());
        assert!(
            crate::session_scratch::list_container(&pool)
                .signals
                .is_empty(),
            "the signal was consumed"
        );

        nobody_using();
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.signals, 0, "{report:#?}");
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!run.exists());
    }

    /// A closed session's record whose directory is proven gone on a
    /// mounted volume is forgotten in classification, with no open-file
    /// observation at all (here it would fail); a record on a volume that
    /// is away is never forgotten that way, and still waits for an
    /// observation.
    #[test]
    fn a_gone_directory_is_forgotten_without_an_observation_unless_its_volume_is_away() {
        let (owner, _storage, mut broker) = fixture();
        let (session, path) = started(&mut broker, "scratch gone early");
        broker
            .close_abandoning_unpushed(session.id, "scratch fixture")
            .unwrap();
        std::fs::remove_dir(&path).unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "lsof refused".into(),
                observed: Vec::new(),
            },
        ));

        let report = run(&mut broker);
        assert_eq!(report.scratch.forgotten_missing, 1, "{report:#?}");
        assert_eq!(report.scratch.probed, 1, "{report:#?}");
        assert!(!report.scratch.process_use_unknown, "{report:#?}");
        assert!(broker.session_scratch_records().is_empty());

        let away = absent_record(owner.path(), session.id, Volume::Away);
        only_record(&broker, &away);
        let report = run(&mut broker);
        assert_eq!(report.scratch.forgotten_missing, 0, "{report:#?}");
        assert!(report.scratch.process_use_unknown, "{report:#?}");
        assert!(broker.session_scratch_records().contains_key(&away.key()));

        nobody_using();
        let report = run(&mut broker);
        assert_eq!(
            report.scratch.kept.get("pool_unavailable"),
            Some(&1),
            "{report:#?}"
        );
        assert!(broker.session_scratch_records().contains_key(&away.key()));
    }

    /// The sessions' states read in one store query classify every record
    /// exactly as one lookup per record does: open, closed, replaced, and a
    /// session the store does not know; the query is chunked past 500 ids.
    #[test]
    fn batched_classification_equals_the_per_record_lookups() {
        let (owner, _storage, mut broker) = fixture();
        let (open, _) = started(&mut broker, "classes open");
        let (closed, _) = started(&mut broker, "classes closed");
        let (replaced, _) = started(&mut broker, "classes replaced");
        broker
            .close_abandoning_unpushed(closed.id, "scratch fixture")
            .unwrap();
        broker.fresh_session_scratch(replaced.id);
        let unknown = absent_record(owner.path(), 999_999, Volume::Mounted);
        broker
            .update_meta_json(
                super::SESSION_SCRATCH_RECORDS_KEY,
                |records: &mut crate::session_scratch::ScratchRecords| {
                    records.insert(unknown.key(), unknown.clone());
                },
            )
            .unwrap();
        let records = broker.session_scratch_records();

        let batched = broker.scratch_record_classes(&records);
        let per_record = records
            .iter()
            .map(|(key, record)| {
                (
                    key.as_str(),
                    super::record_class(record, broker.scratch_session_status(record.session_id)),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(batched, per_record);
        let classes = batched.values().copied().collect::<Vec<_>>();
        assert_eq!(
            classes
                .iter()
                .filter(|class| **class == super::RecordClass::Open)
                .count(),
            2,
            "the open session and the replaced session's fresh directory: {classes:?}"
        );
        assert_eq!(
            classes
                .iter()
                .filter(|class| **class == super::RecordClass::Retire)
                .count(),
            3,
            "closed, replaced and unknown: {classes:?}"
        );

        let ids = (1..=1200).chain([999_999]).collect::<Vec<i64>>();
        let statuses = broker.store_ref().session_statuses(&ids).unwrap();
        for id in [open.id, closed.id, replaced.id] {
            assert_eq!(
                statuses.get(&id).copied(),
                Some(broker.store_ref().session(id).unwrap().status),
                "session {id}"
            );
        }
        assert!(!statuses.contains_key(&999_999));
    }

    /// Once every record was listed, a pass with nothing new lists nothing
    /// and probes nothing; a finished run's signal makes only its record
    /// due, the next pass retires the run and clears the signal.
    #[test]
    fn an_idle_pass_lists_nothing_and_a_new_marker_is_picked_up() {
        let (_owner, _storage, mut broker) = fixture();
        let (first, scratch) = started(&mut broker, "scratch idle first");
        let (_second, _) = started(&mut broker, "scratch idle second");
        let pool = scratch.parent().unwrap().parent().unwrap().to_path_buf();

        let report = run_retirement(&mut broker);
        assert!(report.scratch.full_listing, "{report:#?}");
        assert_eq!(report.scratch.listed, 2, "{report:#?}");
        let report = run_retirement(&mut broker);
        assert!(!report.scratch.full_listing, "{report:#?}");
        assert_eq!(
            (
                report.scratch.listed,
                report.scratch.probed,
                report.scratch.signals
            ),
            (0, 0, 0),
            "{report:#?}"
        );
        assert!(!report.scratch.classify_incomplete, "{report:#?}");

        let run = finished_run(&broker, &first);
        assert_eq!(
            crate::session_scratch::list_container(&pool).signals,
            vec![scratch.file_name().unwrap().to_string_lossy().into_owned()],
            "finishing the run signals its record"
        );
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.signals, 1, "{report:#?}");
        assert_eq!(report.scratch.listed, 1, "only the signaled record");
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!run.exists());
        assert!(
            crate::session_scratch::list_container(&pool)
                .signals
                .is_empty(),
            "the signal is cleared"
        );
        assert!(report.scratch.unrecorded.is_empty(), "{report:#?}");

        // Listed once more (its last listing showed a run), then idle.
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.listed, 1, "{report:#?}");
        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.listed, 0, "{report:#?}");
    }

    /// A marker whose signal is missing (a broker without signals wrote it,
    /// or writing the signal failed) is found by the hourly full listing.
    #[test]
    fn a_marker_without_its_signal_is_found_by_the_full_listing() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, scratch) = started(&mut broker, "scratch backstop");
        run_retirement(&mut broker);
        let run = finished_run(&broker, &session);
        let pool = scratch.parent().unwrap().parent().unwrap().to_path_buf();
        let entry = scratch.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            crate::session_scratch::clear_finished_signals(&pool, &[entry]),
            1
        );

        let report = run_retirement(&mut broker);
        assert_eq!(report.scratch.listed, 0, "{report:#?}");
        assert!(run.is_dir(), "no signal, no listing before the lap");

        broker
            .update_meta_json(
                super::SESSION_SCRATCH_CURSOR_KEY,
                |cursor: &mut super::ScratchRetireCursor| {
                    cursor.full_listing_started_ms = Some(
                        crate::clock::epoch_ms() - super::SCRATCH_FULL_LISTING_INTERVAL_MS - 1,
                    );
                },
            )
            .unwrap();
        let report = run_retirement(&mut broker);
        assert!(report.scratch.full_listing, "{report:#?}");
        assert_eq!(report.scratch.command_temps_retired, 1, "{report:#?}");
        assert!(!run.exists());
    }

    #[test]
    fn a_full_listing_is_due_hourly_or_when_the_clock_went_back() {
        let hour = super::SCRATCH_FULL_LISTING_INTERVAL_MS;
        assert!(super::full_listing_due(None, 0));
        assert!(!super::full_listing_due(Some(1_000), 1_000 + hour - 1));
        assert!(super::full_listing_due(Some(1_000), 1_000 + hour));
        assert!(super::full_listing_due(Some(1_000), 999));
    }

    /// Every directory kept in a pass has its keep recorded exactly once
    /// for that pass: one attempt per pass, whatever else the pass did.
    #[test]
    fn keeps_are_recorded_once_per_pass() {
        let (_owner, _storage, mut broker) = fixture();
        let paths = (0..2)
            .map(|index| {
                let (session, path) = started(&mut broker, &format!("scratch kept {index}"));
                broker
                    .close_abandoning_unpushed(session.id, "scratch fixture")
                    .unwrap();
                path
            })
            .collect::<Vec<_>>();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(
                paths
                    .iter()
                    .map(|path| crate::reclaim::OpenFileUse {
                        pid: std::process::id(),
                        command: "cargo".into(),
                        descriptor: "cwd".into(),
                        file_type: Some("DIR".into()),
                        path: std::fs::canonicalize(path).unwrap(),
                    })
                    .collect(),
            ),
        ));

        for pass in 1..=2_u32 {
            let report = run(&mut broker);
            assert_eq!(
                report.scratch.kept.get("process_using_scratch"),
                Some(&2),
                "{report:#?}"
            );
            let records = broker.session_scratch_records();
            assert_eq!(records.len(), 2);
            for record in records.values() {
                assert_eq!(record.keep.as_deref(), Some("process_using_scratch"));
                assert_eq!(record.attempts, pass, "{record:?}");
            }
        }
        nobody_using();
        assert!(paths.iter().all(|path| path.is_dir()));
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
        let absent = absent_record(owner.path(), session.id, Volume::Away);
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
