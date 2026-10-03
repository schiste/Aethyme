//! Live phase, position and progress of in-flight `broker submit` runs.
//!
//! A submit can legitimately wait for minutes: behind another submission's
//! verification slot, behind a gate's owner lock, or on a long gate. From the
//! outside that looked exactly like a hung process. Each running submit now
//! keeps one small record under `.aethyme/run/submits/`, so the submitting
//! terminal can say where it is in line and another terminal can tell a
//! waiting submit from a stuck one with `aethyme broker status`.
//!
//! Records are files, not database rows, on purpose: they describe a process
//! that is running right now, they must be readable while the broker database
//! is busy, and a crashed submit leaves a record whose PID is dead -- reported
//! as `alive: false` -- rather than a row some migration has to understand.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clock::epoch_ms;

/// No progress for this long marks a running submit as possibly stalled.
///
/// Every wait in the submit path reports at least every heartbeat interval
/// (30 s by default) and a running gate prints `running... Ns` on the same
/// cadence, so a submit that is merely waiting never gets near this. Silence
/// for five minutes means one step -- a git command, a lease refresh -- has
/// been running that long without a sign of life.
pub const SUBMIT_STALL_AFTER: Duration = Duration::from_secs(5 * 60);

/// The phase a submit holds the verification slot in.
pub(crate) const PHASE_VERIFYING: &str = "verifying the merged tree";
/// The phase a submit waits for the verification slot in.
pub(crate) const PHASE_WAITING_FOR_SLOT: &str = "waiting for a verification slot";

fn submits_dir(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/submits")
}

/// What one running submit last said about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitProgressRecord {
    pub session_id: i64,
    pub pid: i64,
    pub started_at_ms: i64,
    pub phase: String,
    pub phase_started_at_ms: i64,
    /// Last time the submit did something observable (a gate line, a wait
    /// tick). The silence heartbeat does not move this.
    pub last_progress_at_ms: i64,
    pub last_progress: String,
}

/// Where a waiting submit stands among the submits wanting the same slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitPosition {
    /// 1-based place among the live submits waiting for a verification slot,
    /// oldest wait first.
    pub position: usize,
    pub waiting: usize,
    /// Live submits currently holding a verification slot.
    pub holders: Vec<SlotHolder>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotHolder {
    pub session_id: i64,
    pub held_for_ms: i64,
    pub last_progress: String,
}

/// One in-flight submit as `broker status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InFlightSubmit {
    pub session_id: i64,
    pub pid: i64,
    pub phase: String,
    pub elapsed_ms: i64,
    pub phase_elapsed_ms: i64,
    pub last_progress: String,
    pub last_progress_age_ms: i64,
    /// False when the submitting process is gone: a crashed submit's record.
    pub alive: bool,
    /// Dead, or alive without progress for [`SUBMIT_STALL_AFTER`].
    pub possibly_stalled: bool,
    pub stall_after_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<WaitPosition>,
}

/// Every submit record in this repository, oldest first, with liveness,
/// stall and queue position derived at `now_ms`.
pub fn in_flight_submits(main_root: &Path, now_ms: i64) -> Vec<InFlightSubmit> {
    in_flight_with(main_root, now_ms, crate::broker::pid_alive)
}

fn in_flight_with(
    main_root: &Path,
    now_ms: i64,
    alive: impl Fn(i64) -> bool,
) -> Vec<InFlightSubmit> {
    let mut records = read_records(main_root)
        .into_iter()
        .map(|record| {
            let live = alive(record.pid);
            (record, live)
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|(record, _)| (record.started_at_ms, record.session_id));
    let positions = slot_positions(&records, now_ms);
    let stall_after_ms = SUBMIT_STALL_AFTER.as_millis() as i64;
    records
        .iter()
        .map(|(record, live)| {
            let last_progress_age_ms = now_ms.saturating_sub(record.last_progress_at_ms);
            InFlightSubmit {
                session_id: record.session_id,
                pid: record.pid,
                phase: record.phase.clone(),
                elapsed_ms: now_ms.saturating_sub(record.started_at_ms),
                phase_elapsed_ms: now_ms.saturating_sub(record.phase_started_at_ms),
                last_progress: record.last_progress.clone(),
                last_progress_age_ms,
                alive: *live,
                possibly_stalled: !*live || last_progress_age_ms >= stall_after_ms,
                stall_after_ms,
                position: positions
                    .iter()
                    .find(|(session, _)| *session == record.session_id)
                    .map(|(_, position)| position.clone()),
            }
        })
        .collect()
}

/// Positions of the live submits waiting for a slot, oldest wait first.
fn slot_positions(
    records: &[(SubmitProgressRecord, bool)],
    now_ms: i64,
) -> Vec<(i64, WaitPosition)> {
    let live = records.iter().filter(|(_, live)| *live).map(|(r, _)| r);
    let holders = live
        .clone()
        .filter(|record| record.phase == PHASE_VERIFYING)
        .map(|record| SlotHolder {
            session_id: record.session_id,
            held_for_ms: now_ms.saturating_sub(record.phase_started_at_ms),
            last_progress: record.last_progress.clone(),
        })
        .collect::<Vec<_>>();
    let mut waiting = live
        .filter(|record| record.phase == PHASE_WAITING_FOR_SLOT)
        .collect::<Vec<_>>();
    waiting.sort_by_key(|record| (record.phase_started_at_ms, record.session_id));
    let count = waiting.len();
    waiting
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            (
                record.session_id,
                WaitPosition {
                    position: index + 1,
                    waiting: count,
                    holders: holders.clone(),
                },
            )
        })
        .collect()
}

fn read_records(main_root: &Path) -> Vec<SubmitProgressRecord> {
    let Ok(entries) = std::fs::read_dir(submits_dir(main_root)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
        .collect()
}

/// `2m05s`, `45s`.
pub(crate) fn duration_label(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// One line saying where a waiting submit stands, for stderr and status.
pub(crate) fn describe_position(position: &WaitPosition) -> String {
    let mut line = format!(
        "position {} of {} waiting",
        position.position, position.waiting
    );
    if !position.holders.is_empty() {
        let holders = position
            .holders
            .iter()
            .map(|holder| {
                let mut text = format!(
                    "session {} (verifying for {}",
                    holder.session_id,
                    duration_label(holder.held_for_ms)
                );
                if !holder.last_progress.is_empty() {
                    text.push_str(", last: ");
                    text.push_str(&holder.last_progress);
                }
                text.push(')');
                text
            })
            .collect::<Vec<_>>()
            .join("; ");
        line.push_str(", slots held by ");
        line.push_str(&holders);
    }
    line
}

struct Shared {
    path: PathBuf,
    record: SubmitProgressRecord,
    /// Last time anything was printed for this submit, heartbeat included.
    last_printed_ms: i64,
}

/// The record of the submit running in this process. Dropping it removes the
/// record, so only a crashed submit leaves one behind.
pub(crate) struct SubmitProgress {
    shared: Arc<Mutex<Shared>>,
    stop: Option<mpsc::Sender<()>>,
    heartbeat: Option<JoinHandle<()>>,
}

impl SubmitProgress {
    /// Start recording, with a stderr heartbeat every `interval` of silence.
    pub(crate) fn start(main_root: &Path, session_id: i64, interval: Duration) -> Self {
        let now = epoch_ms();
        let shared = Arc::new(Mutex::new(Shared {
            path: submits_dir(main_root).join(format!("session-{session_id}.json")),
            record: SubmitProgressRecord {
                session_id,
                pid: i64::from(std::process::id()),
                started_at_ms: now,
                phase: "starting".into(),
                phase_started_at_ms: now,
                last_progress_at_ms: now,
                last_progress: String::new(),
            },
            last_printed_ms: now,
        }));
        persist(&shared);
        let (stop, stopped) = mpsc::channel::<()>();
        let heartbeat_shared = Arc::clone(&shared);
        let heartbeat = std::thread::Builder::new()
            .name("submit-heartbeat".into())
            .spawn(move || heartbeat_loop(&heartbeat_shared, &stopped, interval))
            .ok();
        Self {
            shared,
            stop: Some(stop),
            heartbeat,
        }
    }

    /// Enter a new phase. Counts as progress.
    pub(crate) fn phase(&self, phase: &str) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        let now = epoch_ms();
        shared.record.phase = phase.to_string();
        shared.record.phase_started_at_ms = now;
        shared.record.last_progress_at_ms = now;
        drop(shared);
        persist(&self.shared);
    }

    /// Something observable happened: a gate line, a wait tick.
    pub(crate) fn progress(&self, line: &str, printed: bool) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        let now = epoch_ms();
        shared.record.last_progress_at_ms = now;
        shared.record.last_progress = truncate(line, 160);
        if printed {
            shared.last_printed_ms = now;
        }
        drop(shared);
        persist(&self.shared);
    }

    /// Report a wait for the verification slot: where this submit stands and
    /// who holds the slots. Prints and records one line.
    pub(crate) fn report_slot_wait(&self, main_root: &Path, waited: Duration) {
        let session_id = self
            .shared
            .lock()
            .map(|shared| shared.record.session_id)
            .unwrap_or_default();
        let position = in_flight_submits(main_root, epoch_ms())
            .into_iter()
            .find(|submit| submit.session_id == session_id)
            .and_then(|submit| submit.position);
        let mut line = format!(
            "submit waiting for a verification slot ({})",
            duration_label(waited.as_millis() as i64)
        );
        if let Some(position) = position {
            line.push_str(": ");
            line.push_str(&describe_position(&position));
        }
        eprintln!("{line}");
        self.progress(&line, true);
    }
}

impl Drop for SubmitProgress {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.heartbeat.take()
            && thread.join().is_err()
        {
            eprintln!("Warning: submit progress heartbeat thread panicked");
        }
        if let Ok(shared) = self.shared.lock()
            && let Err(error) = std::fs::remove_file(&shared.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Warning: cannot remove submit progress record: {error}");
        }
    }
}

/// Print one line when the submit has been silent for `interval`, so a long
/// step never looks like a hang. It does not count as progress: a stalled
/// step keeps aging toward [`SUBMIT_STALL_AFTER`] while the heartbeat speaks.
fn heartbeat_loop(shared: &Arc<Mutex<Shared>>, stopped: &mpsc::Receiver<()>, interval: Duration) {
    let tick = interval
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(10));
    loop {
        match stopped.recv_timeout(tick) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => return,
        }
        let Ok(mut state) = shared.lock() else {
            return;
        };
        let now = epoch_ms();
        if now.saturating_sub(state.last_printed_ms) < interval.as_millis() as i64 {
            continue;
        }
        state.last_printed_ms = now;
        let record = &state.record;
        let mut line = format!(
            "submit still running: {} for {} (total {}",
            record.phase,
            duration_label(now - record.phase_started_at_ms),
            duration_label(now - record.started_at_ms)
        );
        line.push_str(&format!(
            "; last progress {} ago)",
            duration_label(now - record.last_progress_at_ms)
        ));
        eprintln!("{line}");
    }
}

fn persist(shared: &Arc<Mutex<Shared>>) {
    let Ok(shared) = shared.lock() else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&shared.record) else {
        return;
    };
    let target = shared.path.clone();
    crate::warn_unrecorded(
        "record submit progress",
        crate::atomic_file::with_synced_temporary(&target, &bytes, |temporary| {
            std::fs::rename(temporary, &target)
        }),
    );
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = text.chars().take(max).collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(session_id: i64, phase: &str, phase_started_at_ms: i64) -> SubmitProgressRecord {
        SubmitProgressRecord {
            session_id,
            pid: 100 + session_id,
            started_at_ms: phase_started_at_ms - 1_000,
            phase: phase.into(),
            phase_started_at_ms,
            last_progress_at_ms: phase_started_at_ms,
            last_progress: format!("gate cargo-test running... (s{session_id})"),
        }
    }

    fn write(root: &Path, record: &SubmitProgressRecord) {
        let dir = submits_dir(root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("session-{}.json", record.session_id)),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn waiting_submits_get_their_place_in_line_and_the_slot_holders() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), &record(812, PHASE_VERIFYING, 10_000));
        write(root.path(), &record(813, PHASE_WAITING_FOR_SLOT, 20_000));
        write(root.path(), &record(814, PHASE_WAITING_FOR_SLOT, 15_000));
        let submits = in_flight_with(root.path(), 200_000, |_| true);

        let position = |session: i64| {
            submits
                .iter()
                .find(|s| s.session_id == session)
                .and_then(|s| s.position.clone())
        };
        assert_eq!(position(812), None, "the holder is not waiting");
        let first = position(814).expect("814 waits");
        let second = position(813).expect("813 waits");
        assert_eq!((first.position, first.waiting), (1, 2), "oldest wait first");
        assert_eq!((second.position, second.waiting), (2, 2));
        assert_eq!(second.holders.len(), 1);
        assert_eq!(second.holders[0].session_id, 812);
        assert_eq!(second.holders[0].held_for_ms, 190_000);
        let line = describe_position(&second);
        assert!(
            line.starts_with(
                "position 2 of 2 waiting, slots held by session 812 (verifying for 3m10s"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_dead_submitter_or_a_silent_one_is_flagged_as_possibly_stalled() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), &record(1, "simulating the merge", 0));
        write(root.path(), &record(2, "simulating the merge", 0));
        let stall = SUBMIT_STALL_AFTER.as_millis() as i64;

        let quiet = in_flight_with(root.path(), stall - 1, |pid| pid != 102);
        let one = quiet.iter().find(|s| s.session_id == 1).unwrap();
        let two = quiet.iter().find(|s| s.session_id == 2).unwrap();
        assert!(one.alive && !one.possibly_stalled, "alive and recent");
        assert!(!two.alive && two.possibly_stalled, "dead submitter");

        let late = in_flight_with(root.path(), stall, |_| true);
        assert!(
            late.iter().all(|s| s.possibly_stalled),
            "silent past the threshold"
        );
    }

    #[test]
    fn a_dead_holder_is_not_counted_as_holding_or_waiting() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), &record(5, PHASE_VERIFYING, 0));
        write(root.path(), &record(6, PHASE_WAITING_FOR_SLOT, 0));
        write(root.path(), &record(7, PHASE_WAITING_FOR_SLOT, 1));
        let submits = in_flight_with(root.path(), 10, |pid| pid != 105 && pid != 106);
        let seven = submits.iter().find(|s| s.session_id == 7).unwrap();
        let position = seven.position.clone().unwrap();
        assert_eq!((position.position, position.waiting), (1, 1));
        assert!(position.holders.is_empty());
    }

    #[test]
    fn the_record_follows_the_submit_and_disappears_when_it_ends() {
        let root = tempfile::tempdir().unwrap();
        let progress = SubmitProgress::start(root.path(), 42, Duration::from_secs(3600));
        progress.phase(PHASE_WAITING_FOR_SLOT);
        progress.progress("gate unit pass in 3s", false);
        let submits = in_flight_submits(root.path(), epoch_ms());
        assert_eq!(submits.len(), 1);
        assert_eq!(submits[0].session_id, 42);
        assert_eq!(submits[0].phase, PHASE_WAITING_FOR_SLOT);
        assert_eq!(submits[0].last_progress, "gate unit pass in 3s");
        assert!(submits[0].alive);
        drop(progress);
        assert!(in_flight_submits(root.path(), epoch_ms()).is_empty());
    }

    #[test]
    fn the_heartbeat_does_not_count_as_progress() {
        let root = tempfile::tempdir().unwrap();
        let progress = SubmitProgress::start(root.path(), 9, Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(120));
        let shared = progress.shared.lock().unwrap();
        assert!(
            shared.last_printed_ms > shared.record.started_at_ms,
            "the heartbeat printed"
        );
        assert_eq!(
            shared.record.last_progress_at_ms, shared.record.started_at_ms,
            "a heartbeat must not move last progress, or a hung step never looks stalled"
        );
    }
}
