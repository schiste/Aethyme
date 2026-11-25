//! The checkout retirement lane process (`broker gc __retire-lane <root>`).
//!
//! The worker starts one per repository while closed checkouts wait. It
//! works through the closed backlog continuously instead of a lap that
//! shares the worker's 180 s window. It runs at normal disk priority with
//! reduced CPU priority, while the worker is throttled.
//!
//! It uses the same judgement and the same keeps as the worker's laps:
//! [`Broker::retirement_lap`] over closed sessions, then over cleaned
//! sessions' leftover folders. Each plan has the 60 s inspection allowance.
//!
//! Bounds:
//! - One lane process per repository (a lane lock in the host state
//!   directory). The worker takes the same lock for the length of its own
//!   closed and cleaned laps and skips them when it cannot, so the kernel
//!   lock makes the two exclusive; a starting lane waits for the worker's
//!   laps to let go.
//! - One judgement at a time per volume, host-wide (a per-volume turn lock
//!   held for each judgement), so a volume never runs two of the lane's Git
//!   inspections at once.
//! - At most one judgement per run is given longer than its slice (a
//!   checkout an earlier attempt ran out of time on).
//! - Paced by host pressure (a gap between slices while loaded or at
//!   memory warning). It ends at critical memory pressure, once both laps
//!   reach their end (everything judgeable was judged), or after 60
//!   minutes.

use super::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One slice: the open sessions' paths and the run's open-file observation
/// are taken once per slice. A judgement that runs past its slice may hold
/// them longer; its move into the trash takes a fresh observation.
pub(super) const RETIRE_LANE_SLICE: Duration = Duration::from_secs(5 * 60);
/// The longest one lane process runs; the next worker starts another while
/// closed checkouts still wait.
const RETIRE_LANE_MAX: Duration = Duration::from_secs(60 * 60);
/// The gap between slices while the host is loaded or memory is at warning.
const RETIRE_LANE_LOADED_GAP: Duration = Duration::from_secs(5);
/// The budget of the one process-listing probe a worker (before starting
/// the lane) and the lane (at its start) make.
const RETIRE_LANE_LISTING_PROBE_BUDGET: Duration = Duration::from_secs(10);
/// Meta key: when this repository's worker or lane was last refused the
/// process listing (epoch ms): retirement waits for an unsandboxed runner.
pub(crate) const CHECKOUT_RETIREMENT_LISTING_REFUSED_KEY: &str =
    "checkout_retirement.listing_refused_at_ms";
/// How often a judgement asks again for its volume's turn.
const RETIRE_LANE_TURN_POLL: Duration = Duration::from_millis(250);

/// How long the judgement of a checkout earlier attempts ran out of time on
/// may take from its plan: four slices, a third of [`RETIRE_LANE_MAX`].
pub(super) const RETIRE_LONG_JUDGEMENT: Duration = Duration::from_secs(20 * 60);

/// What one lane process did, recorded as `checkout_retirement_lane` on
/// its metric row.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RetirementLaneReport {
    pub examined: usize,
    pub retired: usize,
    pub kept: std::collections::BTreeMap<String, usize>,
    pub inspections_started: usize,
    pub inspections_succeeded: usize,
    pub inspections_failed: usize,
    pub inspections_cut_by_deadline: usize,
    pub median_inspection_ms: Option<u64>,
    pub slices: usize,
    pub paced_slices: usize,
    /// `backlog_done`, `cap`, `pressure`, `process_use_unknown`,
    /// `process_listing_refused`, `open_paths_unresolved`, `lane_present`,
    /// `no_pool` or `error`.
    pub end_reason: &'static str,
    pub elapsed_ms: u64,
}

/// How long a waiter's note beside a volume turn lock counts as fresh: a
/// waiter rewrites it at every poll.
const RETIRE_LANE_WAITER_FRESH_MS: i64 = 1_000;
/// How long a lane process waits for its repository's lane lock while the
/// worker's own laps hold it (they last a few minutes at most).
#[cfg(not(test))]
const RETIRE_LANE_LOCK_WAIT: Duration = Duration::from_secs(5 * 60);
#[cfg(test)]
const RETIRE_LANE_LOCK_WAIT: Duration = Duration::from_secs(2);
/// How often a starting lane asks again for its repository's lane lock.
const RETIRE_LANE_LOCK_POLL: Duration = Duration::from_secs(1);

/// The note a waiter for the turn lock at `path` keeps beside it.
fn waiter_note_path(path: &Path) -> PathBuf {
    path.with_extension("waiting")
}

/// The process named by a fresh waiter note at `note`, if any.
pub(super) fn fresh_waiter(note: &Path, now_ms: i64) -> Option<u32> {
    let text = std::fs::read_to_string(note).ok()?;
    let mut words = text.split_whitespace();
    let pid = match (words.next(), words.next()) {
        (Some("pid"), Some(pid)) => pid.parse().ok()?,
        _ => return None,
    };
    let at_ms: i64 = match (words.next(), words.next()) {
        (Some("at"), Some(at)) => at.parse().ok()?,
        _ => return None,
    };
    (now_ms.saturating_sub(at_ms) < RETIRE_LANE_WAITER_FRESH_MS).then_some(pid)
}

/// Meta key: the retirement lane's run record ([`RetirementLaneRecord`]).
pub(crate) const CHECKOUT_RETIREMENT_LANE_RECORD_KEY: &str = "checkout_retirement.lane";
/// Lane runs in a row that retired nothing, with closed checkouts waiting,
/// before `broker status` says so.
pub(crate) const RETIRE_LANE_IDLE_ADVICE_AT: u32 = 3;

/// What the last lane runs did, for `broker status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetirementLaneRecord {
    /// Lane runs in a row (that examined anything or ran a slice) that
    /// retired nothing; reset by a run that retired one.
    #[serde(default)]
    pub runs_without_retirement: u32,
    #[serde(default)]
    pub last_examined: usize,
    #[serde(default)]
    pub last_inspections_started: usize,
    #[serde(default)]
    pub last_inspections_succeeded: usize,
    #[serde(default)]
    pub last_inspections_failed: usize,
    #[serde(default)]
    pub last_inspections_cut_by_deadline: usize,
    #[serde(default)]
    pub last_end_reason: String,
    /// The last run that ran a slice.
    #[serde(default)]
    pub last_at_ms: i64,
}

/// The record after a lane run: a run that retired one resets the count;
/// one that ran no slice (another lane, no pool, refused listing) leaves it.
pub(crate) fn next_lane_record(
    previous: &RetirementLaneRecord,
    lane: &RetirementLaneReport,
    now_ms: i64,
) -> RetirementLaneRecord {
    let ran = lane.slices > 0;
    let runs_without_retirement = if lane.retired > 0 {
        0
    } else if ran {
        previous.runs_without_retirement.saturating_add(1)
    } else {
        previous.runs_without_retirement
    };
    RetirementLaneRecord {
        runs_without_retirement,
        last_examined: lane.examined,
        last_inspections_started: lane.inspections_started,
        last_inspections_succeeded: lane.inspections_succeeded,
        last_inspections_failed: lane.inspections_failed,
        last_inspections_cut_by_deadline: lane.inspections_cut_by_deadline,
        last_end_reason: lane.end_reason.to_string(),
        last_at_ms: if ran { now_ms } else { previous.last_at_ms },
    }
}

/// Take the host-wide turn lock at `path`, asking every
/// [`RETIRE_LANE_TURN_POLL`] until `deadline`. `None` when it stayed held
/// until then; an error when it could not be opened or asked for.
///
/// Turns are shared fairly between repositories' lanes: a waiter keeps a
/// note beside the lock, and a process about to take the turn while
/// another process's note is fresh first lets one poll go by, so a lane
/// that judges back to back cannot keep the turn from a waiting one.
pub(super) fn take_volume_turn(
    path: &Path,
    deadline: Instant,
) -> std::io::Result<Option<crate::file_lock::ExclusiveFileLock>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let note = waiter_note_path(path);
    let own_pid = std::process::id();
    let forget_note = || {
        if fresh_waiter(&note, crate::clock::epoch_ms()) == Some(own_pid) {
            crate::warn_unrecorded(
                "clear a volume turn waiter note",
                std::fs::remove_file(&note),
            );
        }
    };
    let mut yielded = false;
    loop {
        if !yielded
            && fresh_waiter(&note, crate::clock::epoch_ms()).is_some_and(|waiter| waiter != own_pid)
        {
            yielded = true;
            crate::phase_telemetry::add("checkout_retirement_volume_turn_yielded", Duration::ZERO);
            std::thread::sleep(RETIRE_LANE_TURN_POLL * 2);
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match crate::file_lock::ExclusiveFileLock::try_acquire(file) {
            Ok(Some(lock)) => {
                forget_note();
                return Ok(Some(lock));
            }
            Ok(None) => {}
            Err(error) => {
                forget_note();
                return Err(error);
            }
        }
        if Instant::now() + RETIRE_LANE_TURN_POLL >= deadline {
            forget_note();
            return Ok(None);
        }
        crate::warn_unrecorded(
            "note a volume turn waiter",
            std::fs::write(
                &note,
                format!("pid {own_pid} at {}\n", crate::clock::epoch_ms()),
            ),
        );
        std::thread::sleep(RETIRE_LANE_TURN_POLL);
    }
}

/// Take this repository's lane lock at `path`, waiting up to
/// [`RETIRE_LANE_LOCK_WAIT`] while it is held (the worker's own laps take it
/// for their length). `None` when it stayed held: another lane process.
fn take_lane_lock(
    path: &Path,
    repository: &Path,
) -> std::io::Result<Option<crate::checkout_trash::CatchupLaneLock>> {
    let deadline = Instant::now() + RETIRE_LANE_LOCK_WAIT;
    loop {
        if let Some(lock) = crate::checkout_trash::try_catchup_lane_lock(path, repository)? {
            return Ok(Some(lock));
        }
        if Instant::now() + RETIRE_LANE_LOCK_POLL >= deadline {
            return Ok(None);
        }
        std::thread::sleep(RETIRE_LANE_LOCK_POLL);
    }
}

/// The 16 hex digits naming `root` in host-wide lock file names.
fn lock_key(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    Sha256::digest(canonical.as_os_str().as_encoded_bytes())[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The volume `path` is on, as a lock name component (the device number;
/// `0` where it cannot be read).
#[cfg(unix)]
pub(super) fn volume_key(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map_or(0, |metadata| metadata.dev())
}

/// No device number on this platform: one turn for every volume.
#[cfg(not(unix))]
pub(super) fn volume_key(_path: &Path) -> u64 {
    0
}

impl Broker {
    /// This repository's retirement lane lock in `host_dir`.
    pub(crate) fn checkout_retirement_lane_lock_path(&self, host_dir: &Path) -> PathBuf {
        host_dir.join(format!(
            "checkout-retirement-lane-{}.lock",
            lock_key(self.main_root())
        ))
    }

    /// Whether this repository's retirement lane process is alive: its lock
    /// is held (probed by a momentary take that writes nothing).
    pub(crate) fn checkout_retirement_lane_present(&self, host_dir: &Path) -> bool {
        matches!(
            crate::checkout_trash::probe_catchup_lane_lock(
                &self.checkout_retirement_lane_lock_path(host_dir)
            ),
            crate::checkout_trash::CatchupLaneProbe::Held(_)
        )
    }

    /// Whether this process is refused the host-wide process listing the
    /// retirement judgement needs (one bounded `lsof`, judged by how long it
    /// took: an observation that ran out of time is not a refusal). Records
    /// when, as meta `checkout_retirement.listing_refused_at_ms`, and counts
    /// `checkout_retirement_lane_listing_refused`.
    pub(crate) fn checkout_retirement_listing_refused(&self) -> bool {
        let _span = crate::phase_telemetry::span("checkout_retirement_listing_probe");
        let started = Instant::now();
        let observed = crate::session_holder::retirement_run_process_use_snapshot(
            RETIRE_LANE_LISTING_PROBE_BUDGET,
        );
        if matches!(observed, crate::reclaim::OpenFileSnapshot::Complete(_))
            || process_use_failure(started.elapsed(), RETIRE_LANE_LISTING_PROBE_BUDGET)
                == ProcessUseFailure::TimedOut
        {
            return false;
        }
        crate::phase_telemetry::add("checkout_retirement_lane_listing_refused", Duration::ZERO);
        crate::warn_unrecorded(
            "record that the process listing was refused",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_LISTING_REFUSED_KEY,
                &crate::clock::epoch_ms().to_string(),
            ),
        );
        true
    }

    /// The retirement lane's run record.
    pub(crate) fn checkout_retirement_lane_record(&self) -> RetirementLaneRecord {
        self.meta_json(CHECKOUT_RETIREMENT_LANE_RECORD_KEY)
    }

    /// Record `lane`'s run (see [`next_lane_record`]).
    fn record_checkout_retirement_lane_run(&self, lane: &RetirementLaneReport) {
        let record = next_lane_record(
            &self.checkout_retirement_lane_record(),
            lane,
            crate::clock::epoch_ms(),
        );
        if record.runs_without_retirement >= RETIRE_LANE_IDLE_ADVICE_AT {
            crate::phase_telemetry::add("checkout_retirement_lane_idle_runs", Duration::ZERO);
        }
        crate::warn_unrecorded(
            "record the checkout retirement lane run",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_LANE_RECORD_KEY,
                &serde_json::to_string(&record).unwrap_or_default(),
            ),
        );
    }

    /// Whether the worker should start the retirement lane process: the
    /// last strip pass counted closed checkouts on disk, and no lane
    /// process of this repository is alive.
    pub(crate) fn checkout_retirement_lane_needed(&self, host_dir: &Path) -> bool {
        self.checkout_retirement_progress()
            .strip
            .backlog
            .closed_folders
            > 0
            && !self.checkout_retirement_lane_present(host_dir)
    }

    /// Run the retirement lane for this repository until the closed backlog
    /// is judged, the cap, or critical memory pressure (see the module
    /// documentation). `sample` reads host pressure.
    pub(crate) fn run_checkout_retirement_lane_process(
        &mut self,
        host_dir: &Path,
        sample: fn() -> crate::checkout_trash::HostPressure,
    ) -> RetirementLaneReport {
        let started = Instant::now();
        let mut lane = RetirementLaneReport::default();
        let end_reason = self.run_checkout_retirement_lane_slices(host_dir, sample, &mut lane);
        lane.end_reason = end_reason;
        lane.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        crate::phase_telemetry::add(
            match lane.end_reason {
                "backlog_done" => "checkout_retirement_lane_end_backlog_done",
                "cap" => "checkout_retirement_lane_end_cap",
                "pressure" => "checkout_retirement_lane_end_pressure",
                "process_use_unknown" => "checkout_retirement_lane_end_process_use_unknown",
                "open_paths_unresolved" => "checkout_retirement_lane_end_open_paths_unresolved",
                "lane_present" => "checkout_retirement_lane_end_lane_present",
                "no_pool" => "checkout_retirement_lane_end_no_pool",
                "process_listing_refused" => "checkout_retirement_lane_end_process_listing_refused",
                _ => "checkout_retirement_lane_end_error",
            },
            Duration::ZERO,
        );
        self.record_checkout_retirement_lane_run(&lane);
        crate::phase_telemetry::attach(
            "checkout_retirement_lane",
            serde_json::to_value(&lane).unwrap_or_default(),
        );
        lane
    }

    fn run_checkout_retirement_lane_slices(
        &mut self,
        host_dir: &Path,
        sample: fn() -> crate::checkout_trash::HostPressure,
        lane: &mut RetirementLaneReport,
    ) -> &'static str {
        let _span = crate::phase_telemetry::span("checkout_retirement_lane");
        // One lane process per repository, exclusive with the worker's own
        // laps: it waits while the worker's laps hold the lock, and a
        // second lane process ends once the wait runs out.
        let _repository_lock = match take_lane_lock(
            &self.checkout_retirement_lane_lock_path(host_dir),
            self.main_root(),
        ) {
            Ok(Some(lock)) => lock,
            Ok(None) => return "lane_present",
            Err(_) => return "error",
        };
        let Ok(pool) = self.require_bound_repository_worktree_root("checkout retirement lane")
        else {
            return "no_pool";
        };
        // A lane that cannot list processes judges nothing: it ends before
        // spending a slice (a sandboxed worker should not have started it).
        if self.checkout_retirement_listing_refused() {
            return "process_listing_refused";
        }
        let volume_turn = host_dir.join(format!(
            "checkout-retirement-volume-{}.lock",
            volume_key(&pool)
        ));
        let cap = Instant::now() + RETIRE_LANE_MAX;
        let mut long_slot = true;
        let mut short_cuts = std::collections::BTreeSet::new();
        let mut succeeded_ms = Vec::new();
        loop {
            // Too little left for an inspection: slices would only defer.
            if Instant::now() + RETIRE_INSPECTION_MIN_LEFT >= cap {
                return "cap";
            }
            let pressure = {
                let _span = crate::phase_telemetry::span("checkout_retirement_lane_pressure");
                sample()
            };
            if pressure.memory_critical {
                return "pressure";
            }
            if pressure.loaded || pressure.memory_warning {
                lane.paced_slices += 1;
                let _span = crate::phase_telemetry::span("checkout_retirement_lane_paced");
                std::thread::sleep(RETIRE_LANE_LOADED_GAP);
            }
            let slice_deadline = (Instant::now() + RETIRE_LANE_SLICE).min(cap);
            lane.slices += 1;
            // The same scopes as a worker run: one worktree listing and one
            // live-path cache per slice.
            let _inventory = crate::git::WorktreeInventoryRunScope::enter();
            let _lane_scope = RetirementLaneScope::enter();
            let begun = match self.begin_checkout_retirement_run(slice_deadline) {
                Ok(begun) => begun,
                Err(_) => return "error",
            };
            let Some(mut run) = begun else {
                return "open_paths_unresolved";
            };
            run.archive_deadline = slice_deadline;
            run.volume_turn = Some(volume_turn.clone());
            run.lane_end = Some(cap);
            run.long_slot = long_slot;
            run.short_cuts = std::mem::take(&mut short_cuts);
            let mut report = CheckoutRetirementReport::default();
            let closed_complete =
                match self.retirement_lap(&mut run, &mut report, false, slice_deadline) {
                    Ok(complete) => complete,
                    Err(_) => return "error",
                };
            let cleaned_complete = closed_complete
                && !report.process_use_unavailable
                && match self.retirement_lap(&mut run, &mut report, true, slice_deadline) {
                    Ok(complete) => complete,
                    Err(_) => return "error",
                };
            long_slot = run.long_slot;
            short_cuts = std::mem::take(&mut run.short_cuts);
            lane.examined += report.examined;
            lane.retired +=
                report.retired_clean + report.retired_archived + report.retired_operation_archived;
            for (code, count) in &report.kept {
                *lane.kept.entry(code.clone()).or_default() += count;
            }
            let inspections = std::mem::take(&mut run.inspections);
            lane.inspections_started += inspections.started;
            lane.inspections_succeeded += inspections.succeeded;
            lane.inspections_failed += inspections.failed;
            lane.inspections_cut_by_deadline += inspections.cut_by_deadline;
            succeeded_ms.extend(inspections.succeeded_ms);
            lane.median_inspection_ms = InspectionStats {
                succeeded_ms: succeeded_ms.clone(),
                ..InspectionStats::default()
            }
            .median_succeeded_ms();
            if report.process_use_unavailable {
                return "process_use_unknown";
            }
            // The turn lock cannot be opened: no slice could judge anything.
            if report.kept.contains_key("volume_turn_error") {
                return "error";
            }
            if closed_complete && cleaned_complete {
                return "backlog_done";
            }
        }
    }
}
