//! Pacing for deletion nobody is waiting on.
//!
//! The detached cleanup worker removes finished sessions' build output file by
//! file. At full speed that is tens of thousands of `unlink` calls a second,
//! and every one of them is followed by the system's file-event and indexing
//! daemons; on a 16 GB machine three such workers dirtied gigabytes of
//! file-backed memory and contributed to memory-pressure kills. Reclaiming the
//! space is wanted; doing it at a rate that starves the machine is not.
//!
//! The worker installs an [`UnattendedPacer`] on its main thread before it
//! opens the broker. Descriptor-relative tree removal consults it before every
//! unlink; without one installed (every foreground command an agent waits on:
//! `finish`, `finish cleanup`, `gc apply`, the bounded inline sweep) the hook
//! is a no-op and removal runs at full speed exactly as before. Once per batch
//! of [`PACING_BATCH_ENTRIES`] unlinks the pacer
//!
//! * sleeps off the batch's share of the token bucket
//!   (`[cleanup] unattended_unlinks_per_second`), unless the sweep is in
//!   disk-pressure mode, where freeing space is the priority; and
//! * reads cheap system signals. Memory pressure above normal is a hard
//!   stop: the pacer pauses, bounded by the caller's deadline and a per-run
//!   cap, and when that budget is spent it asks removal to stop resumably;
//!   the reclaim request stays durable for the next run. Load far above the
//!   core count (or Linux I/O pressure) is a throttle, never a stop: the
//!   pacer waits a bounded time for it to clear ([`LOAD_BATCH_WAIT`] per
//!   batch, [`LOAD_GATE_WAIT`] before a removal starts) and then proceeds,
//!   so a machine whose normal state is a high load average still makes
//!   progress, at a reduced rate.
//!
//! The pacer exists because unthrottled unattended deletion once filled
//! file-backed memory and contributed to memory-pressure kills. Memory
//! pressure is that hazard; a high load average on its own is not (many
//! short-lived processes and I/O waits on a busy machine keep it far above
//! the core count for days while memory is fine), and background CPU and
//! I/O priority plus the unlink rate limit already bound the impact.
//!
//! The pacer also holds the host-wide single-deleter slot, and only while
//! deleting: at most one process on the machine deletes unattended at a
//! time. It takes the slot (waiting at most [`HOST_DELETER_ACQUIRE_WAIT`])
//! right before a deletion, after the pressure check, and keeps it through
//! that deletion and the limiter sleep that pays for it:
//!
//! * a tree removal takes it at its first batch and keeps it batch after
//!   batch, handing it over after [`HOST_DELETER_MAX_HOLD`], and gives it
//!   back when the removal returns;
//! * a whole-checkout removal takes it right before Git deletes (the
//!   pressure is sampled again once it is held) and gives it back after the
//!   removal's charge;
//! * lanes that delete through paths with no per-deletion hook pin it for
//!   their length.
//!
//! A pressure wait never holds it (an unpinned slot is given back before
//! the pacer sleeps for memory or load), nor does judging, counting or a
//! lock gap, which run with nothing taken. When another repository's worker
//! keeps it through the wait, the deletion is put off resumably and the
//! worker goes back to judging. Judging itself is bounded by a separate,
//! small host-wide semaphore ([`HOST_JUDGING_PERMITS`]).
//!
//! Everything here is observational: pacing never changes what is deleted.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::background_priority::BackgroundPriority;

/// `[cleanup]` key in `.aethyme/config.toml`.
pub(crate) const UNLINK_RATE_CONFIG_KEY: &str = "unattended_unlinks_per_second";
/// Shipped unlink rate for unattended removal. A full-speed sweep on an
/// SSD runs one to two orders of magnitude faster; this still clears a
/// 300,000-entry dependency tree within one five-minute worker run.
pub(crate) const DEFAULT_UNATTENDED_UNLINKS_PER_SECOND: u32 = 1_000;
const MAX_UNATTENDED_UNLINKS_PER_SECOND: i64 = 1_000_000;
/// Unlinks between two limiter and pressure checks: the "short sleep every N
/// entries". Small enough that a pause starts within a fraction of a second
/// at the default rate, large enough that the checks cost nothing measurable.
pub(crate) const PACING_BATCH_ENTRIES: u32 = 256;
/// How long one pressure pause sleeps before sampling again.
const PRESSURE_POLL: Duration = Duration::from_secs(1);
/// Total time one worker run may spend paused for memory pressure before it
/// yields with its request retained. Load waits do not count against it.
const MAX_PRESSURE_PAUSE_PER_RUN: Duration = Duration::from_secs(120);
/// How long each pacing batch waits while load stays elevated before it
/// proceeds anyway: the throttle. At the default rate a 256-entry batch
/// takes 256 ms, so sustained load cuts tree removal to about a fifth of
/// the configured rate (about 200 entries a second), never to zero.
pub(crate) const LOAD_BATCH_WAIT: Duration = PRESSURE_POLL;
/// How long the check before an unattended removal starts waits for
/// elevated load to clear before the removal proceeds throttled.
pub(crate) const LOAD_GATE_WAIT: Duration = Duration::from_secs(5);
/// One-minute load average per core above which the machine is "far above"
/// its capacity. Only a throttle signal: load never stops removal.
const LOAD_PER_CORE_LIMIT: f64 = 2.0;
/// Linux PSI thresholds (percent of the last ten seconds some task stalled).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PSI_MEMORY_SOME_AVG10_LIMIT: f64 = 10.0;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PSI_IO_SOME_AVG10_LIMIT: f64 = 40.0;
/// Longest a worker waits for the host-wide single-deleter slot right
/// before one deletion. Past it the deletion is put off resumably and the
/// worker goes back to judging; it never sits idle for minutes. A
/// whole-checkout removal waits with its repository's GC lock held, so the
/// wait is short.
pub(crate) const HOST_DELETER_ACQUIRE_WAIT: Duration = Duration::from_secs(2);
/// How often that wait tries the slot again: well under
/// [`HOST_DELETER_HANDOFF`], so a waiting worker sees a hand-over.
const HOST_DELETER_ACQUIRE_POLL: Duration = Duration::from_millis(100);
/// How long a lane that pins the slot for its whole length waits for it.
/// No GC lock is held then, so it may wait longer than one deletion does.
const HOST_DELETER_PIN_WAIT: Duration = Duration::from_secs(10);
/// Longest one tree removal keeps the slot batch after batch before it
/// hands it over to a waiting worker.
const HOST_DELETER_MAX_HOLD: Duration = Duration::from_secs(20);
/// After a hand-over, how long the slot stays free before this worker tries
/// again: more than two polls of a waiting worker.
const HOST_DELETER_HANDOFF: Duration = Duration::from_millis(250);
/// Workers on one machine that may judge closed checkouts (Git status walks,
/// ignored listings, ancestry checks) at the same time. One is always
/// allowed to delete; two judging keeps a repository that just finished a
/// removal from waiting on the busiest repository's lap, while four or five
/// repositories' walks no longer all run at once on a loaded machine.
pub(crate) const HOST_JUDGING_PERMITS: usize = 2;
/// How long a slice waits for a judging permit before it ends with a short
/// retry.
const HOST_JUDGING_PERMIT_WAIT: Duration = Duration::from_secs(5);
const HOST_JUDGING_PERMIT_POLL: Duration = Duration::from_millis(250);
/// Lock files of the host-wide judging semaphore, in the host deleter lock's
/// directory: `gc-unattended-judge-<n>.lock` for `n` below
/// [`HOST_JUDGING_PERMITS`].
const HOST_JUDGING_LOCK_PREFIX: &str = "gc-unattended-judge-";

/// Configured unattended unlink rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnlinkRate {
    Limited(u32),
    /// `0` or `"unlimited"`: today's full-speed behaviour.
    Unlimited,
}

impl UnlinkRate {
    fn limit(self) -> Option<u32> {
        match self {
            Self::Limited(rate) => Some(rate),
            Self::Unlimited => None,
        }
    }
}

/// Parse `[cleanup] unattended_unlinks_per_second`. Absent means the shipped
/// default; `0` and `"unlimited"` turn the limiter off.
pub(crate) fn parse_unlink_rate(value: Option<&toml::Value>) -> Result<UnlinkRate, String> {
    match value {
        None => Ok(UnlinkRate::Limited(DEFAULT_UNATTENDED_UNLINKS_PER_SECOND)),
        Some(toml::Value::Integer(0)) => Ok(UnlinkRate::Unlimited),
        Some(toml::Value::Integer(rate))
            if (1..=MAX_UNATTENDED_UNLINKS_PER_SECOND).contains(rate) =>
        {
            Ok(UnlinkRate::Limited(*rate as u32))
        }
        Some(toml::Value::String(text)) if text == "unlimited" => Ok(UnlinkRate::Unlimited),
        Some(_) => Err(format!(
            "[cleanup] {UNLINK_RATE_CONFIG_KEY} must be an integer between 0 (unlimited) and {MAX_UNATTENDED_UNLINKS_PER_SECOND}, or \"unlimited\""
        )),
    }
}

/// Read the rate from the primary checkout's `.aethyme/config.toml`. An
/// unreadable file keeps the conservative default rather than lifting the
/// limit, and the error is reported in the run summary.
pub(crate) fn load_unlink_rate(main_root: &Path) -> (UnlinkRate, Option<String>) {
    let loaded = (|| -> Result<UnlinkRate, String> {
        let Some((path, text)) =
            aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
                .map_err(|error| error.to_string())?
        else {
            return parse_unlink_rate(None);
        };
        let root: toml::Value = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        parse_unlink_rate(
            root.get("cleanup")
                .and_then(|cleanup| cleanup.get(UNLINK_RATE_CONFIG_KEY)),
        )
    })();
    match loaded {
        Ok(rate) => (rate, None),
        Err(error) => (
            UnlinkRate::Limited(DEFAULT_UNATTENDED_UNLINKS_PER_SECOND),
            Some(error.chars().take(160).collect()),
        ),
    }
}

/// Time source for the limiter; tests inject a fake one.
pub(crate) trait PacingClock {
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
}

pub(crate) struct SystemClock;

impl PacingClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// One reading of the system signals. `None` means the signal is unavailable
/// on this platform or could not be read; the pacer then proceeds at the rate
/// limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PressureSample {
    pub(crate) memory_elevated: Option<bool>,
    pub(crate) load_elevated: Option<bool>,
}

/// Source of pressure samples; tests inject a scripted one.
pub(crate) trait PressureSignal {
    fn sample(&mut self) -> PressureSample;
}

/// Reads the platform's cheap pressure signals: on macOS
/// `kern.memorystatus_vm_pressure_level` and the one-minute load average, on
/// Linux `/proc/pressure/{memory,io}` and `/proc/loadavg`.
pub(crate) struct SystemPressure {
    cores: usize,
}

impl SystemPressure {
    pub(crate) fn new() -> Self {
        Self {
            cores: std::thread::available_parallelism().map_or(1, |cores| cores.get()),
        }
    }
}

impl PressureSignal for SystemPressure {
    fn sample(&mut self) -> PressureSample {
        platform::sample(self.cores)
    }
}

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn memory_pressure_level_elevated(level: i32) -> bool {
    level > 1
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn load_far_above_cores(load_one_minute: f64, cores: usize) -> bool {
    load_one_minute > LOAD_PER_CORE_LIMIT * cores.max(1) as f64
}

/// The `avg10` value of the `some` line of a Linux PSI file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_psi_some_avg10(text: &str) -> Option<f64> {
    text.lines()
        .find(|line| line.starts_with("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// The first field of `/proc/loadavg`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{PressureSample, load_far_above_cores, memory_pressure_level_elevated};

    pub(super) fn sample(cores: usize) -> PressureSample {
        let mut level: libc::c_int = 0;
        let mut length = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is a NUL-terminated literal, `level` and `length`
        // are valid for writes of the sizes passed, and no new value is set.
        let read = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&raw mut level).cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        } == 0
            && length == std::mem::size_of::<libc::c_int>();
        let mut load = [0f64; 1];
        // SAFETY: `load` has room for the one element requested.
        let loaded = unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } == 1;
        PressureSample {
            memory_elevated: read.then(|| memory_pressure_level_elevated(level)),
            load_elevated: loaded.then(|| load_far_above_cores(load[0], cores)),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{
        PSI_IO_SOME_AVG10_LIMIT, PSI_MEMORY_SOME_AVG10_LIMIT, PressureSample, load_far_above_cores,
        parse_loadavg, parse_psi_some_avg10,
    };

    fn read_value(path: &str, parse: fn(&str) -> Option<f64>) -> Option<f64> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| parse(&text))
    }

    pub(super) fn sample(cores: usize) -> PressureSample {
        let memory = read_value("/proc/pressure/memory", parse_psi_some_avg10);
        let io = read_value("/proc/pressure/io", parse_psi_some_avg10);
        let load = read_value("/proc/loadavg", parse_loadavg);
        let io_elevated = io.map(|avg10| avg10 >= PSI_IO_SOME_AVG10_LIMIT);
        let load_elevated = load.map(|load| load_far_above_cores(load, cores));
        PressureSample {
            memory_elevated: memory.map(|avg10| avg10 >= PSI_MEMORY_SOME_AVG10_LIMIT),
            load_elevated: match (io_elevated, load_elevated) {
                (None, None) => None,
                (io, load) => Some(io.unwrap_or(false) || load.unwrap_or(false)),
            },
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::PressureSample;

    /// No cheap signal is qualified here; proceed at the rate limit.
    pub(super) fn sample(_cores: usize) -> PressureSample {
        PressureSample::default()
    }
}

/// Telemetry for one detached worker run, surfaced as
/// `last_artifact_sweep.unattended_pacing` in `broker status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnattendedPacingSummary {
    /// Wall-clock milliseconds when this summary was recorded.
    pub at_ms: i64,
    /// Configured limit; `None` when the limiter is off.
    pub unlinks_per_second_limit: Option<u32>,
    /// The configuration could not be read; the default limit applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    /// Which priority reductions the platform accepted.
    pub priority: BackgroundPriority,
    /// Entries (files, symlinks and directories) removed under pacing.
    pub entries_removed: u64,
    /// Logical regular-file bytes observed for those entries.
    pub logical_bytes_removed: u64,
    /// Milliseconds from installing the pacer to this summary.
    pub elapsed_ms: u64,
    /// `entries_removed` per second of `elapsed_ms`.
    pub removal_rate_per_second: u64,
    /// Milliseconds the token bucket slept.
    pub limiter_sleep_ms: u64,
    /// Milliseconds paused because memory pressure was above normal.
    pub memory_pause_ms: u64,
    /// Milliseconds waited because load (or Linux I/O pressure) was far
    /// above the core count, each wait bounded before removal proceeded.
    pub load_pause_ms: u64,
    /// Times removal stopped resumably because the memory pressure pause
    /// budget or the deadline ran out while memory pressure persisted. Load
    /// never stops removal.
    pub pressure_yields: u64,
    /// Times a pressure check proceeded at the throttled rate because load
    /// (or Linux I/O pressure) stayed elevated through its bounded wait.
    #[serde(default)]
    pub load_throttled: u64,
    /// Milliseconds spent waiting for the host-wide single-deleter lock,
    /// summed over every deletion of the run (each waits at most
    /// [`HOST_DELETER_ACQUIRE_WAIT`]).
    pub host_lock_wait_ms: u64,
    /// Milliseconds this run held the host-wide single-deleter slot.
    #[serde(default)]
    pub host_slot_held_ms: u64,
    /// Milliseconds of `host_slot_held_ms` not spent sleeping: deleting and
    /// the bookkeeping around it. The rest is the rate limiter paying for
    /// what was deleted.
    #[serde(default)]
    pub host_slot_deleting_ms: u64,
    /// Times this run took the slot.
    #[serde(default)]
    pub host_slot_acquisitions: u64,
    /// Times a deletion was put off because another repository's worker
    /// kept the slot through the bounded wait.
    #[serde(default)]
    pub host_slot_busy: u64,
    /// Milliseconds spent waiting for a host-wide judging permit.
    #[serde(default)]
    pub judge_permit_wait_ms: u64,
    /// Times every judging permit stayed taken through the wait and the
    /// slice ended with a short retry.
    #[serde(default)]
    pub judge_permit_busy: u64,
    /// Times the host directory stopped holding the slot (deletion went on
    /// unserialised) and times it held it again afterwards.
    #[serde(default)]
    pub host_slot_unavailable: u64,
    #[serde(default)]
    pub host_slot_recovered: u64,
    /// Deferred requests that found this repository's worker already alive
    /// and were served by this run instead of starting another.
    pub requests_coalesced: u64,
    /// Disk-pressure mode lifted the rate limit for at least one pass.
    pub rate_lifted_for_disk_pressure: bool,
}

/// Token bucket plus pressure back-off for one worker run.
pub(crate) struct UnattendedPacer {
    rate: UnlinkRate,
    disk_pressure: bool,
    clock: Box<dyn PacingClock>,
    pressure: Box<dyn PressureSignal>,
    batch_entries: u32,
    in_batch: u32,
    batch_started: Option<Instant>,
    installed_at: Instant,
    limiter_slept: Duration,
    memory_paused: Duration,
    load_paused: Duration,
    host_lock_waited: Duration,
    /// A whole-checkout charge is running: the disk-pressure lift does not
    /// apply to it, so checkout removal is always paced.
    bulk_charge_active: bool,
    /// Where the host-wide single-deleter and judging locks live; `None`
    /// (foreground, tests that do not opt in, no host state directory)
    /// deletes unserialised, as a missing directory always did.
    host_directory: Option<PathBuf>,
    /// The single-deleter slot while held, and since when.
    host_slot: Option<(crate::file_lock::ExclusiveFileLock, Instant)>,
    /// Lanes pinning the slot for their whole length; while any does, the
    /// slot is not given back for pressure waits or hand-overs.
    host_slot_pins: u32,
    host_slot_held: Duration,
    /// Sleeps (rate limiter, pinned pressure waits) while the slot was held.
    host_slot_slept: Duration,
    judge_permit_waited: Duration,
    /// Why the last whole-checkout deletion gate refused, until taken.
    deletion_refusal: Option<DeletionGate>,
    /// The host directory could not hold the slot at the last attempt:
    /// deletion runs unserialised, as a missing directory always did, until
    /// a later take succeeds.
    host_slot_unavailable: bool,
    /// Another worker kept the slot through a tree-removal batch's wait in
    /// this pass: later batches refuse at once instead of waiting again,
    /// until the pass ends ([`clear_host_deleter_busy`]).
    host_busy_this_pass: bool,
    summary: UnattendedPacingSummary,
}

/// What the gate right before an unattended deletion decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionGate {
    /// Delete: the slot is held (or no host directory serialises deletion)
    /// and memory pressure is not elevated.
    Proceed,
    /// Memory pressure is elevated: do not delete now.
    MemoryBackoff,
    /// Another repository's worker kept the slot through the bounded wait.
    HostDeleterBusy,
}

/// How a request for the slot ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotTake {
    AlreadyHeld,
    AtOnce,
    AfterWait,
    /// No host directory: deletion runs unserialised.
    Unserialised,
    Busy,
}

/// What [`charge_bulk_unattended_removal`] did for one whole-checkout removal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BulkCharge {
    /// Entries paid for by sleeping after the removal.
    pub(crate) charged: u64,
    /// Entries Git's own removal already paid for at the configured rate:
    /// the bucket measures from when Git began deleting, so that time is not
    /// slept a second time. Never more than the entries charged for.
    pub(crate) credited: u64,
    /// Memory pressure outlasted the pacer's pause budget; the charge
    /// stopped and the caller must back off. Load never sets it.
    pub(crate) memory_backoff: bool,
    /// What the deadline left unpaid at the configured rate; the caller must
    /// not remove again before this much more time has passed.
    pub(crate) unpaid: Duration,
}

/// What a pressure check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PressureVerdict {
    /// No pressure, or load cleared within the bounded wait.
    Clear,
    /// Load (or Linux I/O pressure) stayed elevated through the bounded
    /// wait: proceed, throttled.
    LoadThrottled,
    /// Memory pressure outlasted the run's pause budget or the deadline: do
    /// not start (or stop resumably).
    MemoryBackoff,
}

impl UnattendedPacer {
    pub(crate) fn new(
        rate: UnlinkRate,
        clock: Box<dyn PacingClock>,
        pressure: Box<dyn PressureSignal>,
    ) -> Self {
        let installed_at = clock.now();
        Self {
            rate,
            disk_pressure: false,
            clock,
            pressure,
            batch_entries: PACING_BATCH_ENTRIES,
            in_batch: 0,
            batch_started: None,
            installed_at,
            limiter_slept: Duration::ZERO,
            memory_paused: Duration::ZERO,
            load_paused: Duration::ZERO,
            host_lock_waited: Duration::ZERO,
            bulk_charge_active: false,
            host_directory: None,
            host_slot: None,
            host_slot_pins: 0,
            host_slot_held: Duration::ZERO,
            host_slot_slept: Duration::ZERO,
            judge_permit_waited: Duration::ZERO,
            deletion_refusal: None,
            host_slot_unavailable: false,
            host_busy_this_pass: false,
            summary: UnattendedPacingSummary {
                unlinks_per_second_limit: rate.limit(),
                ..UnattendedPacingSummary::default()
            },
        }
    }

    /// Serialise this pacer's deletions with every other worker on the
    /// machine through the locks in `directory`.
    pub(crate) fn with_host_directory(mut self, directory: Option<PathBuf>) -> Self {
        self.host_directory = directory;
        self
    }

    /// Take the single-deleter slot unless already held, waiting at most
    /// [`HOST_DELETER_ACQUIRE_WAIT`] (or `wait` when given), never past
    /// `deadline`.
    fn take_host_slot(&mut self, deadline: Option<Instant>, wait: Duration) -> SlotTake {
        if self.host_slot.is_some() {
            return SlotTake::AlreadyHeld;
        }
        let wait = cap_to_deadline(wait, self.clock.now(), deadline);
        let directory = self.host_directory.as_deref();
        let clock = &mut self.clock;
        let Some(directory) = directory else {
            return SlotTake::Unserialised;
        };
        let (slot, waited) = super::wait_for_host_deleter_slot(
            Some(directory),
            wait,
            HOST_DELETER_ACQUIRE_POLL,
            |step| clock.sleep(step),
        );
        self.host_lock_waited += waited;
        match slot {
            super::HostDeleterSlot::Held(lock) => {
                // A transient open error no longer serialises nothing for
                // the rest of the run: the next successful take ends it.
                if self.host_slot_unavailable {
                    self.host_slot_unavailable = false;
                    self.summary.host_slot_recovered =
                        self.summary.host_slot_recovered.saturating_add(1);
                }
                self.host_slot = Some((lock, self.clock.now()));
                self.summary.host_slot_acquisitions =
                    self.summary.host_slot_acquisitions.saturating_add(1);
                if waited.is_zero() {
                    SlotTake::AtOnce
                } else {
                    SlotTake::AfterWait
                }
            }
            super::HostDeleterSlot::Unavailable => {
                if !self.host_slot_unavailable {
                    self.host_slot_unavailable = true;
                    self.summary.host_slot_unavailable =
                        self.summary.host_slot_unavailable.saturating_add(1);
                }
                SlotTake::Unserialised
            }
            super::HostDeleterSlot::Busy => {
                self.summary.host_slot_busy = self.summary.host_slot_busy.saturating_add(1);
                SlotTake::Busy
            }
        }
    }

    /// Whether an unlink must hold the slot: a host directory serialises
    /// deletion here and has not proved unusable this run.
    fn slot_required(&self) -> bool {
        self.host_directory.is_some() && !self.host_slot_unavailable
    }

    /// End a deletion: pay the partial batch it was in (with the slot still
    /// held, so the limiter's sleep stays the machine's one deleter's), start
    /// the next deletion on a fresh batch, and give the slot back unless a
    /// lane pins it. Without a held slot nothing changes, as before.
    fn end_deletion(&mut self) {
        if self.host_slot.is_none() || self.host_slot_pins > 0 {
            return;
        }
        self.settle_partial_batch(None);
        self.release_host_slot();
    }

    /// Pay the current batch's share of the bucket for the entries it has
    /// so far, and start the next unlink on a fresh batch.
    fn settle_partial_batch(&mut self, deadline: Option<Instant>) {
        if let Some(started) = self.batch_started.take() {
            self.pay_entries(started, self.in_batch, deadline);
        }
        self.in_batch = 0;
    }

    /// Hand a slot held for [`HOST_DELETER_MAX_HOLD`] over: give it back and
    /// leave it free for [`HOST_DELETER_HANDOFF`]. `true` when it did.
    fn hand_over_if_held_too_long(&mut self, deadline: Option<Instant>) -> bool {
        let held_too_long = self.host_slot_pins == 0
            && self.host_slot.as_ref().is_some_and(|(_, since)| {
                self.clock.now().saturating_duration_since(*since) >= HOST_DELETER_MAX_HOLD
            });
        if !held_too_long {
            return false;
        }
        self.release_host_slot();
        let pause = cap_to_deadline(HOST_DELETER_HANDOFF, self.clock.now(), deadline);
        self.clock.sleep(pause);
        true
    }

    /// Give the slot back, unless a lane pins it.
    fn release_host_slot(&mut self) {
        if self.host_slot_pins > 0 {
            return;
        }
        if let Some((lock, since)) = self.host_slot.take() {
            self.host_slot_held += self.clock.now().saturating_duration_since(since);
            drop(lock);
        }
    }

    /// Sleep `duration`, counting it against the slot's hold when held.
    fn sleep_noting_slot(&mut self, duration: Duration) {
        self.clock.sleep(duration);
        if self.host_slot.is_some() {
            self.host_slot_slept += duration;
        }
    }

    /// The gate before one batch of tree-removal unlinks. The pressure check
    /// runs first, with the slot given back before any wait; then the slot
    /// is taken (or kept from the last batch). When taking it meant waiting,
    /// memory pressure is sampled again, so the check stays immediately
    /// before the deletion; elevated, the slot goes back and the pressure is
    /// waited out without it. A slot held for [`HOST_DELETER_MAX_HOLD`] is
    /// handed over first.
    fn enter_batch_deletion(&mut self, deadline: Option<Instant>) -> DeletionGate {
        self.hand_over_if_held_too_long(deadline);
        if self.host_busy_this_pass && self.host_slot.is_none() {
            return DeletionGate::HostDeleterBusy;
        }
        loop {
            if self.checked_wait(deadline, LOAD_BATCH_WAIT) == PressureVerdict::MemoryBackoff {
                return DeletionGate::MemoryBackoff;
            }
            match self.take_host_slot(deadline, HOST_DELETER_ACQUIRE_WAIT) {
                SlotTake::Busy => {
                    self.host_busy_this_pass = true;
                    return DeletionGate::HostDeleterBusy;
                }
                SlotTake::AfterWait if self.pressure.sample().memory_elevated == Some(true) => {
                    self.release_host_slot();
                }
                _ => return DeletionGate::Proceed,
            }
        }
    }

    /// The gate right before Git removes a whole checkout, with the
    /// repository's GC lock held: take the slot within the bounded wait and
    /// sample memory pressure once it is held. Nothing here waits out
    /// pressure (that would hold the GC lock); the slice's own check before
    /// its next pass does, with nothing held.
    fn enter_deletion_now(&mut self) -> DeletionGate {
        let gate = match self.take_host_slot(None, HOST_DELETER_ACQUIRE_WAIT) {
            SlotTake::Busy => DeletionGate::HostDeleterBusy,
            SlotTake::Unserialised
            | SlotTake::AlreadyHeld
            | SlotTake::AtOnce
            | SlotTake::AfterWait => {
                if self.pressure.sample().memory_elevated == Some(true) {
                    self.summary.pressure_yields = self.summary.pressure_yields.saturating_add(1);
                    self.release_host_slot();
                    DeletionGate::MemoryBackoff
                } else {
                    DeletionGate::Proceed
                }
            }
        };
        if gate != DeletionGate::Proceed {
            self.deletion_refusal = Some(gate);
        }
        gate
    }

    /// Take one of the [`HOST_JUDGING_PERMITS`] host-wide judging permits,
    /// waiting at most [`HOST_JUDGING_PERMIT_WAIT`]. `Ok(None)` means judging
    /// is not bounded here (no host directory, or its lock files cannot be
    /// opened); `Err(())` that every permit stayed taken.
    fn take_judging_permit(&mut self) -> Result<Option<crate::file_lock::ExclusiveFileLock>, ()> {
        let Some(directory) = self.host_directory.clone() else {
            return Ok(None);
        };
        if std::fs::create_dir_all(&directory).is_err() {
            return Ok(None);
        }
        let mut waited = Duration::ZERO;
        loop {
            let mut unavailable = 0;
            for index in 0..HOST_JUDGING_PERMITS {
                let path = directory.join(format!("{HOST_JUDGING_LOCK_PREFIX}{index}.lock"));
                match crate::file_lock::open_lock_file(&path)
                    .and_then(crate::file_lock::ExclusiveFileLock::try_acquire)
                {
                    Ok(Some(lock)) => {
                        self.judge_permit_waited += waited;
                        return Ok(Some(lock));
                    }
                    Ok(None) => {}
                    Err(_) => unavailable += 1,
                }
            }
            if unavailable == HOST_JUDGING_PERMITS {
                self.judge_permit_waited += waited;
                return Ok(None);
            }
            if waited >= HOST_JUDGING_PERMIT_WAIT {
                self.judge_permit_waited += waited;
                self.summary.judge_permit_busy = self.summary.judge_permit_busy.saturating_add(1);
                return Err(());
            }
            let step =
                HOST_JUDGING_PERMIT_POLL.min(HOST_JUDGING_PERMIT_WAIT.saturating_sub(waited));
            self.clock.sleep(step);
            waited += step;
        }
    }

    fn effective_limit(&self) -> Option<u32> {
        if self.disk_pressure && !self.bulk_charge_active {
            None
        } else {
            self.rate.limit()
        }
    }

    /// The check before an unattended removal starts: pause while memory
    /// pressure lasts, wait at most [`LOAD_GATE_WAIT`] for elevated load to
    /// clear.
    fn pressure_check(&mut self, deadline: Option<Instant>) -> PressureVerdict {
        self.checked_wait(deadline, LOAD_GATE_WAIT)
    }

    /// [`Self::wait_out_pressure`], counting a memory back-off as a yield.
    fn checked_wait(&mut self, deadline: Option<Instant>, load_wait: Duration) -> PressureVerdict {
        let verdict = self.wait_out_pressure(deadline, load_wait);
        if verdict == PressureVerdict::MemoryBackoff {
            self.summary.pressure_yields = self.summary.pressure_yields.saturating_add(1);
        }
        verdict
    }

    /// Pay for `entries` Git deleted in one removal that took
    /// `removal_spent`. That duration is credited at the configured rate (so
    /// the average over removal plus wait never exceeds it) and the rest is
    /// slept off batch by batch, never at a disk-pressure-lifted rate.
    /// Pressure is sampled first, so a removal under memory pressure always
    /// waits it out (or stops the charge) even when the credit covers every
    /// entry; under load each batch waits at most [`LOAD_BATCH_WAIT`] and
    /// the charge goes on. A deadline that cuts the sleep short reports what
    /// is still owed.
    fn charge_bulk(
        &mut self,
        entries: u64,
        removal_spent: Duration,
        deadline: Option<Instant>,
    ) -> BulkCharge {
        if entries == 0 {
            return BulkCharge::default();
        }
        if self.checked_wait(deadline, LOAD_BATCH_WAIT) == PressureVerdict::MemoryBackoff {
            return BulkCharge {
                memory_backoff: true,
                ..BulkCharge::default()
            };
        }
        self.bulk_charge_active = true;
        let limit = self.effective_limit();
        let credited = limit.map_or(entries, |limit| {
            let paid = removal_spent.as_millis().saturating_mul(u128::from(limit)) / 1_000;
            u64::try_from(paid).unwrap_or(u64::MAX).min(entries)
        });
        let to_charge = entries - credited;
        self.summary.entries_removed = self.summary.entries_removed.saturating_add(credited);
        let mut charged = 0;
        let mut memory_backoff = false;
        while charged < to_charge && deadline.is_none_or(|deadline| self.clock.now() < deadline) {
            // The charge pays for a deletion already made: it keeps the slot
            // that deletion took but never takes one.
            if !self.gate_unlink(deadline, false) {
                memory_backoff = deadline.is_none_or(|deadline| self.clock.now() < deadline);
                break;
            }
            self.after_unlink(0);
            charged += 1;
        }
        self.bulk_charge_active = false;
        let unpaid = match limit {
            Some(limit) if !memory_backoff && charged < to_charge => Duration::from_millis(
                (to_charge - charged).saturating_mul(1_000) / u64::from(limit.max(1)),
            ),
            _ => Duration::ZERO,
        };
        BulkCharge {
            charged,
            credited,
            memory_backoff,
            unpaid,
        }
    }

    /// Gate one unlink. `false` asks the caller to stop resumably, exactly as
    /// an expired deadline does; memory pressure does that, and so does the
    /// host deleter slot staying with another repository's worker through
    /// the bounded wait. Under load each batch waits at most
    /// [`LOAD_BATCH_WAIT`] and goes on.
    pub(crate) fn before_unlink(&mut self, deadline: Option<Instant>) -> bool {
        self.gate_unlink(deadline, true)
    }

    /// [`Self::before_unlink`]; `take_slot` is false for a charge, which
    /// pays for a deletion already made and takes no slot.
    fn gate_unlink(&mut self, deadline: Option<Instant>, take_slot: bool) -> bool {
        // No unattended unlink without the slot, whatever the batch counter
        // says: one about to happen with no slot held starts a fresh batch
        // through the full gate (pressure check, then the slot).
        if take_slot && self.slot_required() && self.host_slot.is_none() && self.in_batch != 0 {
            self.settle_partial_batch(deadline);
        }
        if self.in_batch == 0 {
            if let Some(started) = self.batch_started.take() {
                self.pay_batch(started, deadline);
            }
            if take_slot {
                if self.enter_batch_deletion(deadline) != DeletionGate::Proceed {
                    return false;
                }
            } else {
                // A charge that has held the slot for the maximum hands it
                // over like a tree removal, and pays the rest without it if
                // another worker takes it: the deletion it pays for is over.
                if self.hand_over_if_held_too_long(deadline) {
                    // Busy is counted and the charge goes on without it.
                    self.take_host_slot(deadline, HOST_DELETER_ACQUIRE_WAIT);
                }
                if self.checked_wait(deadline, LOAD_BATCH_WAIT) == PressureVerdict::MemoryBackoff {
                    return false;
                }
            }
            self.batch_started = Some(self.clock.now());
        }
        self.in_batch += 1;
        if self.in_batch >= self.batch_entries {
            self.in_batch = 0;
        }
        true
    }

    pub(crate) fn after_unlink(&mut self, logical_bytes: u64) {
        self.summary.entries_removed = self.summary.entries_removed.saturating_add(1);
        self.summary.logical_bytes_removed = self
            .summary
            .logical_bytes_removed
            .saturating_add(logical_bytes);
    }

    /// Sleep off whatever the last full batch finished ahead of its share of
    /// the bucket, never past the caller's deadline.
    fn pay_batch(&mut self, started: Instant, deadline: Option<Instant>) {
        self.pay_entries(started, self.batch_entries, deadline);
    }

    /// Sleep off whatever `entries` unlinks begun at `started` finished
    /// ahead of their share of the bucket, never past the caller's deadline.
    fn pay_entries(&mut self, started: Instant, entries: u32, deadline: Option<Instant>) {
        let Some(limit) = self.effective_limit() else {
            return;
        };
        let minimum = Duration::from_nanos(
            u64::from(entries).saturating_mul(1_000_000_000) / u64::from(limit.max(1)),
        );
        let now = self.clock.now();
        let Some(deficit) = minimum.checked_sub(now.saturating_duration_since(started)) else {
            return;
        };
        let sleep = cap_to_deadline(deficit, now, deadline);
        if !sleep.is_zero() {
            // The slot stays held: this sleep is what keeps the machine's one
            // deleter at the configured rate.
            self.sleep_noting_slot(sleep);
            self.limiter_slept += sleep;
        }
    }

    /// Wait while a signal reports pressure. Memory pressure pauses until it
    /// clears, or until the run's memory pause budget or the deadline is
    /// spent ([`PressureVerdict::MemoryBackoff`]). Load (or Linux I/O
    /// pressure) waits at most `load_wait` in this call, never past the
    /// deadline, and then proceeds ([`PressureVerdict::LoadThrottled`]);
    /// load waits never spend the memory pause budget. The host deleter
    /// slot is given back before any of these waits, unless a lane pins it.
    fn wait_out_pressure(
        &mut self,
        deadline: Option<Instant>,
        load_wait: Duration,
    ) -> PressureVerdict {
        let mut load_waited = Duration::ZERO;
        loop {
            let sample = self.pressure.sample();
            let now = self.clock.now();
            if sample.memory_elevated == Some(true) {
                let budget = MAX_PRESSURE_PAUSE_PER_RUN.saturating_sub(self.memory_paused);
                let step = cap_to_deadline(PRESSURE_POLL.min(budget), now, deadline);
                if step.is_zero() {
                    return PressureVerdict::MemoryBackoff;
                }
                self.release_host_slot();
                self.sleep_noting_slot(step);
                self.memory_paused += step;
                continue;
            }
            if sample.load_elevated == Some(true) {
                let left = load_wait.saturating_sub(load_waited);
                let step = cap_to_deadline(PRESSURE_POLL.min(left), now, deadline);
                if step.is_zero() {
                    self.summary.load_throttled = self.summary.load_throttled.saturating_add(1);
                    return PressureVerdict::LoadThrottled;
                }
                self.release_host_slot();
                self.sleep_noting_slot(step);
                self.load_paused += step;
                load_waited += step;
                continue;
            }
            return PressureVerdict::Clear;
        }
    }

    fn summary(&self, at_ms: i64) -> UnattendedPacingSummary {
        let elapsed = self
            .clock
            .now()
            .saturating_duration_since(self.installed_at);
        let elapsed_ms = duration_ms(elapsed);
        let removal_rate_per_second = self
            .summary
            .entries_removed
            .saturating_mul(1_000)
            .checked_div(elapsed_ms)
            .unwrap_or(0);
        // A hold still running counts up to now.
        let held = self.host_slot_held.saturating_add(
            self.host_slot
                .as_ref()
                .map_or(Duration::ZERO, |(_, since)| {
                    self.clock.now().saturating_duration_since(*since)
                }),
        );
        UnattendedPacingSummary {
            at_ms,
            elapsed_ms,
            removal_rate_per_second,
            limiter_sleep_ms: duration_ms(self.limiter_slept),
            memory_pause_ms: duration_ms(self.memory_paused),
            load_pause_ms: duration_ms(self.load_paused),
            host_lock_wait_ms: duration_ms(self.host_lock_waited),
            host_slot_held_ms: duration_ms(held),
            host_slot_deleting_ms: duration_ms(held.saturating_sub(self.host_slot_slept)),
            judge_permit_wait_ms: duration_ms(self.judge_permit_waited),
            ..self.summary.clone()
        }
    }
}

fn cap_to_deadline(duration: Duration, now: Instant, deadline: Option<Instant>) -> Duration {
    deadline.map_or(duration, |deadline| {
        duration.min(deadline.saturating_duration_since(now))
    })
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

thread_local! {
    // The detached worker is single-threaded: its sweep, checkout cleanup and
    // tree removal all run on the thread that installed the pacer. A worker
    // that later parallelises removal must install a pacer per thread (or
    // share one) or that removal runs unpaced.
    static ACTIVE: RefCell<Option<UnattendedPacer>> = const { RefCell::new(None) };
}

/// Uninstalls the pacer when dropped.
pub(crate) struct UnattendedPacingScope {
    _thread_bound: std::marker::PhantomData<*const ()>,
}

impl Drop for UnattendedPacingScope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.borrow_mut().take());
    }
}

/// Install `pacer` for unattended removal on the calling thread.
pub(crate) fn install(pacer: UnattendedPacer) -> UnattendedPacingScope {
    ACTIVE.with(|active| *active.borrow_mut() = Some(pacer));
    UnattendedPacingScope {
        _thread_bound: std::marker::PhantomData,
    }
}

fn with_active<T>(default: T, operation: impl FnOnce(&mut UnattendedPacer) -> T) -> T {
    ACTIVE.with(|active| match active.try_borrow_mut() {
        Ok(mut active) => active.as_mut().map_or(default, operation),
        Err(_) => default,
    })
}

/// [`with_active`] for a deletion gate: `no_pacer` when none is installed
/// (foreground), but `refused` when the installed pacer is already borrowed,
/// so a gate never lets a deletion through unserialised by accident.
fn with_active_gate<T>(
    no_pacer: T,
    refused: T,
    operation: impl FnOnce(&mut UnattendedPacer) -> T,
) -> T {
    ACTIVE.with(|active| match active.try_borrow_mut() {
        Ok(mut active) => active.as_mut().map_or(no_pacer, operation),
        Err(_) => refused,
    })
}

/// Tree-removal hook before one unlink. Without an installed pacer (every
/// foreground caller) this always permits the unlink immediately.
pub(crate) fn before_unattended_unlink(deadline: Option<Instant>) -> bool {
    with_active_gate(true, false, |pacer| pacer.before_unlink(deadline))
}

/// Tree-removal hook after one successful unlink.
pub(crate) fn after_unattended_unlink(logical_bytes: u64) {
    with_active((), |pacer| pacer.after_unlink(logical_bytes));
}

/// Charge `entries` unlinks made in one burst outside tree removal (Git
/// removing a whole checkout, which took `removal_spent`) to the installed
/// pacer: it credits Git's own removal time at the configured rate, sleeps
/// off the rest of their share of the bucket (never lifted for disk
/// pressure), waits out memory pressure and is throttled by load as it
/// would be for its own unlinks, never past `deadline`. Call it with no lock held: the sleep is
/// the throttle. Without a pacer (every foreground caller) it charges
/// nothing and returns at once.
pub(crate) fn charge_bulk_unattended_removal(
    entries: u64,
    removal_spent: Duration,
    deadline: Option<Instant>,
) -> BulkCharge {
    with_active(BulkCharge::default(), |pacer| {
        pacer.charge_bulk(entries, removal_spent, deadline)
    })
}

/// Before an unattended removal starts: on the installed pacer, wait out
/// memory pressure and wait at most [`LOAD_GATE_WAIT`] for elevated load to
/// clear, never past `deadline`. [`PressureVerdict::MemoryBackoff`] means
/// memory pressure outlasted the pause budget or the deadline and the
/// removal must not start; [`PressureVerdict::LoadThrottled`] means load
/// stayed elevated and the removal proceeds at the caller's throttled rate.
/// Without a pacer (every foreground caller) it is always `Clear`.
pub(crate) fn unattended_pressure_check(deadline: Option<Instant>) -> PressureVerdict {
    with_active(PressureVerdict::Clear, |pacer| {
        pacer.pressure_check(deadline)
    })
}

/// The gate right before Git removes a whole checkout (with the GC lock
/// held): on the installed pacer, take the host deleter slot within
/// [`HOST_DELETER_ACQUIRE_WAIT`] and check memory pressure once it is held.
/// On [`DeletionGate::Proceed`] the slot stays held until
/// [`release_host_deleter_slot`], after the removal's charge. Without a
/// pacer (every foreground caller) it always proceeds and holds nothing.
pub(crate) fn enter_unattended_deletion() -> DeletionGate {
    ACTIVE.with(|active| match active.try_borrow_mut() {
        Ok(mut active) => active
            .as_mut()
            .map_or(DeletionGate::Proceed, UnattendedPacer::enter_deletion_now),
        // Refused because the pacer is borrowed: the refusal cannot be
        // stored on it, so it is kept beside it and the slice stops on it
        // like any other.
        Err(_) => {
            BORROWED_DELETION_REFUSAL.with(|refusal| refusal.set(Some(BORROWED_REFUSAL)));
            BORROWED_REFUSAL
        }
    })
}

/// What a deletion gate answers when the installed pacer is already
/// borrowed: refused, as if another worker held the slot.
const BORROWED_REFUSAL: DeletionGate = DeletionGate::HostDeleterBusy;

thread_local! {
    static BORROWED_DELETION_REFUSAL: std::cell::Cell<Option<DeletionGate>> =
        const { std::cell::Cell::new(None) };
}

/// Why the last [`enter_unattended_deletion`] refused, once; `None` when it
/// did not (or no pacer is installed).
pub(crate) fn take_deletion_refusal() -> Option<DeletionGate> {
    let borrowed = BORROWED_DELETION_REFUSAL.with(std::cell::Cell::take);
    with_active(None, |pacer| pacer.deletion_refusal.take()).or(borrowed)
}

/// Give the host deleter slot back, unless a lane pins it. A no-op when it
/// is not held or no pacer is installed.
pub(crate) fn release_host_deleter_slot() {
    with_active((), UnattendedPacer::end_deletion);
}

/// Start a new pass for the sticky busy refusal of tree-removal batches.
pub(crate) fn clear_host_deleter_busy() {
    with_active((), |pacer| pacer.host_busy_this_pass = false);
}

/// Whether a tree removal found the slot with another worker in this pass.
pub(crate) fn host_deleter_busy_this_pass() -> bool {
    with_active(false, |pacer| pacer.host_busy_this_pass)
}

/// Releases the host deleter slot when dropped: a slice holds one so the
/// slot never outlives it, whatever exit it takes.
pub(crate) struct HostDeleterSlotRelease;

impl Drop for HostDeleterSlotRelease {
    fn drop(&mut self) {
        release_host_deleter_slot();
    }
}

/// Whether the installed pacer holds the host deleter slot.
pub(crate) fn host_deleter_slot_held() -> bool {
    with_active(false, |pacer| pacer.host_slot.is_some())
}

/// Before an unlink outside the batch gate (a tree's own root and its
/// witness): take the slot within the bounded wait unless already held.
/// `false` means another worker kept it and the unlink must wait for a
/// later pass. Without a pacer it is always `true`.
pub(crate) fn ensure_host_deleter_slot(deadline: Option<Instant>) -> bool {
    with_active_gate(true, false, |pacer| {
        if pacer.host_busy_this_pass && pacer.host_slot.is_none() {
            return false;
        }
        match pacer.take_host_slot(deadline, HOST_DELETER_ACQUIRE_WAIT) {
            SlotTake::Busy => {
                pacer.host_busy_this_pass = true;
                false
            }
            // Newly taken: memory pressure is read right before the unlink.
            // Elevated, the slot goes back and the tree waits for a later
            // pass (resumably), never holding the slot through the wait.
            SlotTake::AtOnce | SlotTake::AfterWait => {
                if pacer.pressure.sample().memory_elevated == Some(true) {
                    pacer.summary.pressure_yields = pacer.summary.pressure_yields.saturating_add(1);
                    pacer.end_deletion();
                    false
                } else {
                    true
                }
            }
            SlotTake::AlreadyHeld | SlotTake::Unserialised => true,
        }
    })
}

/// A tree removal's hold on the host deleter slot: whatever the removal
/// took is given back when it returns. A slot already held when it began
/// (a pinned lane's) stays.
pub(crate) struct TreeRemovalSlotScope {
    held_before: bool,
}

impl TreeRemovalSlotScope {
    pub(crate) fn enter() -> Self {
        // A removal that starts with no slot held starts on a fresh batch,
        // so its first unlink goes through the full gate; that is also where
        // a host directory that failed earlier in the run gets another try.
        let held_before = with_active(false, |pacer| {
            if pacer.host_slot.is_none() && pacer.host_directory.is_some() {
                pacer.settle_partial_batch(None);
            }
            pacer.host_slot.is_some()
        });
        Self { held_before }
    }
}

impl Drop for TreeRemovalSlotScope {
    fn drop(&mut self) {
        if !self.held_before {
            release_host_deleter_slot();
        }
    }
}

/// A lane's pin on the host deleter slot: held, and kept through pressure
/// waits and hand-overs, until dropped.
pub(crate) struct HostDeleterPin {
    pinned: bool,
}

impl Drop for HostDeleterPin {
    fn drop(&mut self) {
        if self.pinned {
            with_active((), |pacer| {
                pacer.host_slot_pins = pacer.host_slot_pins.saturating_sub(1);
                pacer.end_deletion();
            });
        }
    }
}

/// Pin the host deleter slot for a whole lane that deletes through paths
/// with no per-deletion gate (session retirement, checkout archives, the
/// requested checkout cleanup), waiting at most [`HOST_DELETER_PIN_WAIT`].
/// `None` when another repository's worker kept it through the wait.
/// Without a pacer it pins nothing and always succeeds.
pub(crate) fn pin_host_deleter_slot() -> Option<HostDeleterPin> {
    pin_host_deleter_slot_within(HOST_DELETER_PIN_WAIT)
}

/// [`pin_host_deleter_slot`] with the short wait one deletion gets
/// ([`HOST_DELETER_ACQUIRE_WAIT`]), for a step taken with its
/// repository's GC lock held.
pub(crate) fn pin_host_deleter_slot_briefly() -> Option<HostDeleterPin> {
    pin_host_deleter_slot_within(HOST_DELETER_ACQUIRE_WAIT)
}

fn pin_host_deleter_slot_within(wait: Duration) -> Option<HostDeleterPin> {
    with_active_gate(Some(HostDeleterPin { pinned: false }), None, |pacer| {
        if pacer.take_host_slot(None, wait) == SlotTake::Busy {
            return None;
        }
        pacer.host_slot_pins = pacer.host_slot_pins.saturating_add(1);
        Some(HostDeleterPin { pinned: true })
    })
}

/// One of the host-wide judging permits, held until dropped.
pub(crate) struct JudgingPermit {
    _lock: Option<crate::file_lock::ExclusiveFileLock>,
}

/// Take a host-wide judging permit on the installed pacer, waiting at most
/// [`HOST_JUDGING_PERMIT_WAIT`]. `None` when every permit stayed taken: the
/// caller ends its slice with a short retry. Without a pacer, or with no
/// host directory, judging is not bounded and a permit is always granted.
pub(crate) fn take_judging_permit() -> Option<JudgingPermit> {
    with_active_gate(Some(JudgingPermit { _lock: None }), None, |pacer| {
        pacer
            .take_judging_permit()
            .ok()
            .map(|lock| JudgingPermit { _lock: lock })
    })
}

/// Disk-pressure mode lifts the rate limit (freeing space is then the
/// priority) but keeps the priority lowering and the pressure back-off.
pub(crate) fn set_disk_pressure(pressured: bool) {
    with_active((), |pacer| {
        pacer.disk_pressure = pressured;
        if pressured && pacer.rate.limit().is_some() {
            pacer.summary.rate_lifted_for_disk_pressure = true;
        }
    });
}

pub(crate) fn record_priority(priority: BackgroundPriority) {
    with_active((), |pacer| pacer.summary.priority = priority);
}

pub(crate) fn record_config_error(error: Option<String>) {
    with_active((), |pacer| pacer.summary.config_error = error);
}

pub(crate) fn record_requests_coalesced(count: u64) {
    with_active((), |pacer| {
        pacer.summary.requests_coalesced = pacer.summary.requests_coalesced.saturating_add(count);
    });
}

/// The installed pacer's telemetry so far, or `None` in foreground commands.
pub(crate) fn current_summary(at_ms: i64) -> Option<UnattendedPacingSummary> {
    with_active(None, |pacer| Some(pacer.summary(at_ms)))
}

/// Fake clock: time moves only when the pacer sleeps or a test advances it.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FakeClock {
    now: std::rc::Rc<std::cell::Cell<Instant>>,
    pub(crate) slept: std::rc::Rc<std::cell::Cell<Duration>>,
}

#[cfg(test)]
impl FakeClock {
    pub(crate) fn new() -> Self {
        Self {
            now: std::rc::Rc::new(std::cell::Cell::new(Instant::now())),
            slept: std::rc::Rc::new(std::cell::Cell::new(Duration::ZERO)),
        }
    }

    fn advance(&self, duration: Duration) {
        self.now.set(self.now.get() + duration);
    }
}

#[cfg(test)]
impl PacingClock for FakeClock {
    fn now(&self) -> Instant {
        self.now.get()
    }

    fn sleep(&mut self, duration: Duration) {
        self.advance(duration);
        self.slept.set(self.slept.get() + duration);
    }
}

/// Pressure signal that replays samples, repeating the last one.
#[cfg(test)]
pub(crate) struct Scripted {
    pub(crate) samples: Vec<PressureSample>,
    pub(crate) reads: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(test)]
impl PressureSignal for Scripted {
    fn sample(&mut self) -> PressureSample {
        let index = self.reads.get();
        self.reads.set(index + 1);
        self.samples
            .get(index)
            .or(self.samples.last())
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn calm() -> Box<dyn PressureSignal> {
        Box::new(Scripted {
            samples: vec![PressureSample::default()],
            reads: Rc::new(Cell::new(0)),
        })
    }

    fn memory(elevated: bool) -> PressureSample {
        PressureSample {
            memory_elevated: Some(elevated),
            load_elevated: Some(false),
        }
    }

    #[test]
    fn limiter_caps_the_unlink_rate_on_a_fake_clock() {
        let clock = FakeClock::new();
        let mut pacer =
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm());
        let entries = PACING_BATCH_ENTRIES * 8;
        for _ in 0..entries {
            assert!(pacer.before_unlink(None));
            pacer.after_unlink(10);
        }
        // One more call pays the eighth batch.
        assert!(pacer.before_unlink(None));
        let elapsed = clock.now().saturating_duration_since(pacer.installed_at);
        // Removal itself took no fake time, so every millisecond is the
        // limiter's: 2,048 entries at 1,000/s is 2.048 s.
        assert_eq!(elapsed, Duration::from_millis(2_048));
        assert_eq!(clock.slept.get(), elapsed);
        let summary = pacer.summary(7);
        assert_eq!(summary.entries_removed, u64::from(entries));
        assert_eq!(summary.logical_bytes_removed, u64::from(entries) * 10);
        assert_eq!(summary.limiter_sleep_ms, 2_048);
        assert_eq!(summary.removal_rate_per_second, 1_000);
        assert_eq!(summary.unlinks_per_second_limit, Some(1_000));
    }

    /// A whole checkout Git removed in one burst is paid for at the limit
    /// afterwards, so worker checkout removal averages the configured rate;
    /// foreground callers, with no pacer, are never slowed.
    #[test]
    fn a_bulk_removal_is_charged_to_the_installed_pacer_only() {
        assert_eq!(
            charge_bulk_unattended_removal(10_000, Duration::ZERO, None),
            BulkCharge::default()
        );
        assert_eq!(unattended_pressure_check(None), PressureVerdict::Clear);
        let clock = FakeClock::new();
        let _scope = install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            calm(),
        ));
        let entries = u64::from(PACING_BATCH_ENTRIES) * 8 + 1;
        let charge = charge_bulk_unattended_removal(entries, Duration::ZERO, None);
        assert_eq!(charge.charged, entries);
        assert!(!charge.memory_backoff);
        assert_eq!(charge.unpaid, Duration::ZERO);
        assert_eq!(clock.slept.get(), Duration::from_millis(2_048));
        let summary = current_summary(0).expect("the pacer is installed");
        assert_eq!(summary.entries_removed, entries);
        // A deadline bounds the throttle and reports what it left unpaid.
        let deadline = clock.now() + Duration::from_millis(300);
        let cut = charge_bulk_unattended_removal(entries, Duration::ZERO, Some(deadline));
        assert!(cut.charged < entries && !cut.memory_backoff);
        assert!(cut.unpaid >= Duration::from_millis(1_500), "{cut:?}");
        assert_eq!(clock.now(), deadline);
    }

    /// Git's own removal time paid for part of a checkout at the configured
    /// rate; only the rest is slept, and the credit never exceeds the charge.
    #[test]
    fn a_bulk_charge_credits_only_the_removals_own_duration() {
        let clock = FakeClock::new();
        let mut pacer =
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm());
        // 20,480 entries at 1,000/s is 20.48 s; Git took 8.192 s of it.
        let entries = u64::from(PACING_BATCH_ENTRIES) * 80;
        let charge = pacer.charge_bulk(entries, Duration::from_millis(8_192), None);
        assert_eq!(charge.credited, 8_192);
        assert_eq!(charge.charged, entries - 8_192);
        // 12,288 charged entries are 48 batches of 256 ms; as for tree
        // removal, a batch is paid when the next one starts, so the 48th is
        // paid by the next unlink.
        assert_eq!(clock.slept.get(), Duration::from_millis(47 * 256));
        assert_eq!(pacer.summary(0).entries_removed, entries);
        let before = clock.slept.get();
        let charge = pacer.charge_bulk(1_000, Duration::from_secs(60), None);
        assert_eq!((charge.credited, charge.charged), (1_000, 0));
        assert_eq!(clock.slept.get(), before);
    }

    /// Disk-pressure mode lifts the rate for tree removal, never for a
    /// whole-checkout charge: capacity removal is always paced.
    #[test]
    fn a_bulk_charge_ignores_the_disk_pressure_lift() {
        let clock = FakeClock::new();
        let mut pacer =
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm());
        pacer.disk_pressure = true;
        let entries = u64::from(PACING_BATCH_ENTRIES) * 4 + 1;
        let charge = pacer.charge_bulk(entries, Duration::ZERO, None);
        assert_eq!((charge.credited, charge.charged), (0, entries));
        assert_eq!(clock.slept.get(), Duration::from_millis(1_024));
        assert_eq!(
            pacer.effective_limit(),
            None,
            "the lift still applies elsewhere"
        );
    }

    /// Pressure is read before the credit is applied, so a removal fully
    /// paid for by its own duration still waits out memory pressure, and a
    /// pressure that outlasts the pause budget asks the caller to back off.
    /// The pre-removal gate reads the same signal.
    #[test]
    fn a_bulk_charge_and_the_pre_removal_gate_back_off_under_pressure() {
        let clock = FakeClock::new();
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![memory(true), memory(true), memory(false)],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        let charge = pacer.charge_bulk(1_000, Duration::from_secs(5), None);
        assert_eq!((charge.credited, charge.charged), (1_000, 0));
        assert!(!charge.memory_backoff);
        assert_eq!(clock.slept.get(), PRESSURE_POLL * 2);

        let clock = FakeClock::new();
        let _scope = install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![memory(true)],
                reads: Rc::new(Cell::new(0)),
            }),
        ));
        assert_eq!(
            unattended_pressure_check(None),
            PressureVerdict::MemoryBackoff,
            "memory pressure outlasts the budget"
        );
        assert_eq!(clock.slept.get(), MAX_PRESSURE_PAUSE_PER_RUN);
        let charge = charge_bulk_unattended_removal(1_000, Duration::from_secs(5), None);
        assert!(charge.memory_backoff);
        assert_eq!((charge.credited, charge.charged), (0, 0));
        assert_eq!(current_summary(0).unwrap().pressure_yields, 2);
    }

    #[test]
    fn limiter_only_sleeps_the_deficit_of_a_slow_batch() {
        let clock = FakeClock::new();
        let mut pacer =
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm());
        for _ in 0..PACING_BATCH_ENTRIES {
            assert!(pacer.before_unlink(None));
            // Removal is already slower than the limit: 1 ms per entry.
            clock.advance(Duration::from_millis(1));
        }
        assert!(pacer.before_unlink(None));
        assert_eq!(clock.slept.get(), Duration::ZERO);
    }

    #[test]
    fn unlimited_and_disk_pressure_never_sleep() {
        for (rate, disk_pressure) in [
            (UnlinkRate::Unlimited, false),
            (UnlinkRate::Limited(1), true),
        ] {
            let clock = FakeClock::new();
            let mut pacer = UnattendedPacer::new(rate, Box::new(clock.clone()), calm());
            pacer.disk_pressure = disk_pressure;
            for _ in 0..(PACING_BATCH_ENTRIES * 4 + 1) {
                assert!(pacer.before_unlink(None));
            }
            assert_eq!(
                clock.slept.get(),
                Duration::ZERO,
                "{rate:?} {disk_pressure}"
            );
        }
    }

    #[test]
    fn limiter_never_sleeps_past_the_deadline() {
        let clock = FakeClock::new();
        let deadline = clock.now() + Duration::from_millis(5);
        let mut pacer =
            UnattendedPacer::new(UnlinkRate::Limited(1), Box::new(clock.clone()), calm());
        for _ in 0..=PACING_BATCH_ENTRIES {
            pacer.before_unlink(Some(deadline));
        }
        assert_eq!(clock.slept.get(), Duration::from_millis(5));
    }

    #[test]
    fn memory_pressure_pauses_then_resumes() {
        let clock = FakeClock::new();
        let reads = Rc::new(Cell::new(0));
        let pressure = Scripted {
            samples: vec![memory(true), memory(true), memory(true), memory(false)],
            reads: reads.clone(),
        };
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(clock.clone()),
            Box::new(pressure),
        );
        assert!(pacer.before_unlink(None), "pressure that clears resumes");
        assert_eq!(reads.get(), 4);
        assert_eq!(clock.slept.get(), PRESSURE_POLL * 3);
        // The rest of the batch does not sample again.
        for _ in 1..PACING_BATCH_ENTRIES {
            assert!(pacer.before_unlink(None));
        }
        assert_eq!(reads.get(), 4);
        let summary = pacer.summary(0);
        assert_eq!(summary.memory_pause_ms, 3_000);
        assert_eq!(summary.load_pause_ms, 0);
        assert_eq!(summary.pressure_yields, 0);
    }

    #[test]
    fn persistent_memory_pressure_yields_within_the_deadline_and_the_run_budget() {
        let clock = FakeClock::new();
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![memory(true)],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        let deadline = clock.now() + Duration::from_millis(2_500);
        assert!(!pacer.before_unlink(Some(deadline)));
        assert_eq!(clock.slept.get(), Duration::from_millis(2_500));
        // Without a deadline the per-run cap still bounds the pause.
        assert!(!pacer.before_unlink(None));
        assert_eq!(clock.slept.get(), MAX_PRESSURE_PAUSE_PER_RUN);
        let summary = pacer.summary(0);
        assert_eq!(summary.pressure_yields, 2);
        assert_eq!(summary.memory_pause_ms, 120_000);
        assert_eq!(summary.load_pause_ms, 0);
        assert_eq!(summary.entries_removed, 0);
    }

    /// Memory pressure and elevated load in the same sample are handled as
    /// memory pressure: a hard stop that spends the memory pause budget and
    /// backs off, never a bounded load wait that proceeds throttled.
    #[test]
    fn memory_and_load_in_the_same_sample_is_a_memory_back_off() {
        let both = PressureSample {
            memory_elevated: Some(true),
            load_elevated: Some(true),
        };
        let clock = FakeClock::new();
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![both],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        assert_eq!(pacer.pressure_check(None), PressureVerdict::MemoryBackoff);
        assert_eq!(clock.slept.get(), MAX_PRESSURE_PAUSE_PER_RUN);
        assert!(
            !pacer.before_unlink(None),
            "no unlink under memory pressure"
        );
        let charge = pacer.charge_bulk(1_000, Duration::ZERO, None);
        assert!(charge.memory_backoff, "{charge:?}");
        assert_eq!((charge.credited, charge.charged), (0, 0));
        let summary = pacer.summary(0);
        assert_eq!(summary.memory_pause_ms, 120_000);
        assert_eq!(summary.load_pause_ms, 0);
        assert_eq!(summary.load_throttled, 0);
        assert_eq!(summary.pressure_yields, 3);
        assert_eq!(summary.entries_removed, 0);
    }

    fn sustained_load() -> Box<dyn PressureSignal> {
        Box::new(Scripted {
            samples: vec![PressureSample {
                memory_elevated: Some(false),
                load_elevated: Some(true),
            }],
            reads: Rc::new(Cell::new(0)),
        })
    }

    /// Sustained load (a busy machine whose normal load average is far above
    /// the core count) throttles tree removal and never stops it: each batch
    /// waits one bounded load wait and goes on, for longer than the memory
    /// pause budget, and the load waits spend none of that budget.
    #[test]
    fn sustained_load_throttles_tree_removal_and_never_stops_it() {
        let clock = FakeClock::new();
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            sustained_load(),
        );
        // 200 batches: 200 s of load waits, past the 120 s memory budget.
        let batches = 200u32;
        for _ in 0..(PACING_BATCH_ENTRIES * batches) {
            assert!(pacer.before_unlink(None), "load never stops removal");
            pacer.after_unlink(0);
        }
        let summary = pacer.summary(0);
        assert_eq!(
            summary.entries_removed,
            u64::from(PACING_BATCH_ENTRIES * batches)
        );
        assert_eq!(summary.pressure_yields, 0);
        assert_eq!(summary.load_throttled, u64::from(batches));
        assert_eq!(summary.load_pause_ms, u64::from(batches) * 1_000);
        assert_eq!(summary.memory_pause_ms, 0);
        // The rate limit still applies on top of the load wait: 199 paid
        // batches of 256 ms plus 200 load waits of one second.
        assert_eq!(summary.limiter_sleep_ms, 199 * 256);
        assert!(summary.removal_rate_per_second > 150, "{summary:?}");
        assert!(summary.removal_rate_per_second < 300, "{summary:?}");
        // Memory pressure after all that still gets its whole pause budget.
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![
                    PressureSample {
                        memory_elevated: Some(false),
                        load_elevated: Some(true),
                    },
                    memory(true),
                ],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        let before = clock.slept.get();
        assert!(!pacer.before_unlink(None));
        assert_eq!(
            clock.slept.get() - before,
            LOAD_BATCH_WAIT + MAX_PRESSURE_PAUSE_PER_RUN
        );
    }

    /// The check before a removal waits at most [`LOAD_GATE_WAIT`] for load
    /// to clear and then lets the removal proceed throttled, however long
    /// the load lasts; a removal's bulk charge under the same load is paid
    /// in full at the throttled rate, never abandoned.
    #[test]
    fn sustained_load_lets_a_removal_start_after_a_bounded_wait() {
        let clock = FakeClock::new();
        let _scope = install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            sustained_load(),
        ));
        for round in 1..=60u64 {
            assert_eq!(
                unattended_pressure_check(None),
                PressureVerdict::LoadThrottled
            );
            assert_eq!(clock.slept.get(), LOAD_GATE_WAIT * round as u32);
        }
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.pressure_yields, 0);
        assert_eq!(summary.load_throttled, 60);
        // A deadline shorter than the wait bounds it too.
        let before = clock.slept.get();
        let deadline = clock.now() + Duration::from_millis(1_500);
        assert_eq!(
            unattended_pressure_check(Some(deadline)),
            PressureVerdict::LoadThrottled
        );
        assert_eq!(clock.slept.get() - before, Duration::from_millis(1_500));
        let entries = u64::from(PACING_BATCH_ENTRIES) * 4 + 1;
        let charge = charge_bulk_unattended_removal(entries, Duration::ZERO, None);
        assert!(!charge.memory_backoff);
        assert_eq!(charge.charged, entries);
        assert_eq!(charge.unpaid, Duration::ZERO);
    }

    /// Load that clears during the bounded wait is a normal, unthrottled
    /// verdict.
    #[test]
    fn load_that_clears_mid_wait_is_clear() {
        let clock = FakeClock::new();
        let load = PressureSample {
            memory_elevated: Some(false),
            load_elevated: Some(true),
        };
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![load, load, PressureSample::default()],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        assert_eq!(pacer.pressure_check(None), PressureVerdict::Clear);
        assert_eq!(clock.slept.get(), PRESSURE_POLL * 2);
        let summary = pacer.summary(0);
        assert_eq!(summary.load_pause_ms, 2_000);
        assert_eq!(summary.load_throttled, 0);
        assert_eq!(summary.pressure_yields, 0);
    }

    #[test]
    fn unavailable_signals_proceed_at_the_rate_limit() {
        let clock = FakeClock::new();
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![PressureSample::default()],
                reads: Rc::new(Cell::new(0)),
            }),
        );
        assert!(pacer.before_unlink(None));
        assert_eq!(clock.slept.get(), Duration::ZERO);
    }

    #[test]
    fn hooks_are_inert_without_an_installed_pacer() {
        // Foreground commands never install a pacer.
        assert!(current_summary(0).is_none());
        for _ in 0..(PACING_BATCH_ENTRIES * 2) {
            assert!(before_unattended_unlink(Some(Instant::now())));
        }
        after_unattended_unlink(1);
        assert!(current_summary(0).is_none());
    }

    #[test]
    fn installed_pacer_is_scoped_to_its_guard() {
        let clock = FakeClock::new();
        let scope = install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            calm(),
        ));
        record_requests_coalesced(3);
        set_disk_pressure(true);
        assert!(before_unattended_unlink(None));
        after_unattended_unlink(5);
        let summary = current_summary(9).unwrap();
        assert_eq!(summary.requests_coalesced, 3);
        // No host directory: deletion is unserialised and never waits.
        assert_eq!(summary.host_lock_wait_ms, 0);
        assert_eq!(summary.host_slot_acquisitions, 0);
        assert_eq!(summary.entries_removed, 1);
        assert!(summary.rate_lifted_for_disk_pressure);
        drop(scope);
        assert!(current_summary(0).is_none());
    }

    #[test]
    fn rate_configuration_accepts_integers_zero_and_unlimited() {
        assert_eq!(
            parse_unlink_rate(None).unwrap(),
            UnlinkRate::Limited(DEFAULT_UNATTENDED_UNLINKS_PER_SECOND)
        );
        assert_eq!(
            parse_unlink_rate(Some(&toml::Value::Integer(0))).unwrap(),
            UnlinkRate::Unlimited
        );
        assert_eq!(
            parse_unlink_rate(Some(&toml::Value::String("unlimited".into()))).unwrap(),
            UnlinkRate::Unlimited
        );
        assert_eq!(
            parse_unlink_rate(Some(&toml::Value::Integer(250))).unwrap(),
            UnlinkRate::Limited(250)
        );
        for invalid in [
            toml::Value::Integer(-1),
            toml::Value::Integer(MAX_UNATTENDED_UNLINKS_PER_SECOND + 1),
            toml::Value::String("fast".into()),
            toml::Value::Boolean(true),
        ] {
            assert!(parse_unlink_rate(Some(&invalid)).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn rate_loads_from_the_cleanup_table_and_falls_back_to_the_default() {
        let repository = tempfile::tempdir().unwrap();
        assert_eq!(
            load_unlink_rate(repository.path()),
            (
                UnlinkRate::Limited(DEFAULT_UNATTENDED_UNLINKS_PER_SECOND),
                None
            )
        );
        let config = repository.path().join(aethyme_config::CONFIG_DIR);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[cleanup]\nunattended_unlinks_per_second = \"unlimited\"\n",
        )
        .unwrap();
        assert_eq!(
            load_unlink_rate(repository.path()),
            (UnlinkRate::Unlimited, None)
        );
        std::fs::write(
            config.join("config.toml"),
            "[cleanup]\nunattended_unlinks_per_second = \"warp\"\n",
        )
        .unwrap();
        let (rate, error) = load_unlink_rate(repository.path());
        assert_eq!(
            rate,
            UnlinkRate::Limited(DEFAULT_UNATTENDED_UNLINKS_PER_SECOND)
        );
        assert!(error.is_some());
    }

    #[test]
    fn signal_parsers_read_psi_loadavg_and_pressure_levels() {
        let psi = "some avg10=12.50 avg60=3.00 avg300=1.00 total=1\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(parse_psi_some_avg10(psi), Some(12.5));
        assert_eq!(parse_psi_some_avg10("full avg10=1.00\n"), None);
        assert_eq!(parse_loadavg("3.25 2.00 1.00 2/300 999\n"), Some(3.25));
        assert!(!memory_pressure_level_elevated(1));
        assert!(memory_pressure_level_elevated(2));
        assert!(memory_pressure_level_elevated(4));
        assert!(!load_far_above_cores(19.9, 10));
        assert!(load_far_above_cores(20.1, 10));
    }

    /// Another repository's worker, as seen from this process: a second
    /// open of the lock file, which `flock` treats as a separate holder.
    fn contender_takes_slot(directory: &Path) -> Option<crate::file_lock::ExclusiveFileLock> {
        match super::super::wait_for_host_deleter_slot(
            Some(directory),
            Duration::ZERO,
            Duration::from_millis(1),
            |_| unreachable!("a zero wait never sleeps"),
        )
        .0
        {
            super::super::HostDeleterSlot::Held(lock) => Some(lock),
            _ => None,
        }
    }

    /// A fake clock that, on every sleep, records whether another worker
    /// could have taken the host deleter slot at that moment, and can hand
    /// a contender's slot back on its first sleep.
    struct ProbingClock {
        now: Rc<Cell<Instant>>,
        directory: PathBuf,
        free_during_sleeps: Rc<std::cell::RefCell<Vec<bool>>>,
        release_on_first_sleep: Rc<std::cell::RefCell<Option<crate::file_lock::ExclusiveFileLock>>>,
    }

    impl PacingClock for ProbingClock {
        fn now(&self) -> Instant {
            self.now.get()
        }

        fn sleep(&mut self, duration: Duration) {
            self.release_on_first_sleep.borrow_mut().take();
            let free = contender_takes_slot(&self.directory).is_some();
            self.free_during_sleeps.borrow_mut().push(free);
            self.now.set(self.now.get() + duration);
        }
    }

    struct Probe {
        now: Rc<Cell<Instant>>,
        free_during_sleeps: Rc<std::cell::RefCell<Vec<bool>>>,
        release_on_first_sleep: Rc<std::cell::RefCell<Option<crate::file_lock::ExclusiveFileLock>>>,
    }

    fn probing_clock(directory: &Path) -> (Box<dyn PacingClock>, Probe) {
        let now = Rc::new(Cell::new(Instant::now()));
        let free = Rc::new(std::cell::RefCell::new(Vec::new()));
        let release = Rc::new(std::cell::RefCell::new(None));
        (
            Box::new(ProbingClock {
                now: now.clone(),
                directory: directory.to_path_buf(),
                free_during_sleeps: free.clone(),
                release_on_first_sleep: release.clone(),
            }),
            Probe {
                now,
                free_during_sleeps: free,
                release_on_first_sleep: release,
            },
        )
    }

    fn scripted(samples: Vec<PressureSample>) -> (Box<dyn PressureSignal>, Rc<Cell<usize>>) {
        let reads = Rc::new(Cell::new(0));
        (
            Box::new(Scripted {
                samples,
                reads: reads.clone(),
            }),
            reads,
        )
    }

    /// A tree removal takes the slot at its first unlink and gives it back
    /// when it returns: while it deletes no other worker can, and once it
    /// returned (its caller back to judging) the slot is free at once.
    #[test]
    fn the_deleter_slot_is_held_only_while_a_tree_is_removed() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert!(!host_deleter_slot_held(), "judging holds nothing");
        assert!(contender_takes_slot(host.path()).is_some());
        {
            let _removal = TreeRemovalSlotScope::enter();
            assert!(before_unattended_unlink(None));
            after_unattended_unlink(0);
            assert!(host_deleter_slot_held());
            assert!(
                contender_takes_slot(host.path()).is_none(),
                "no second deleter while this one deletes"
            );
        }
        assert!(!host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_some());
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 1);
        assert_eq!(summary.host_slot_busy, 0);
        assert_eq!(summary.host_lock_wait_ms, 0);
    }

    /// With the slot kept by another worker, a deletion waits at most the
    /// bounded wait and is put off resumably; the whole-checkout gate says
    /// why, once.
    #[test]
    fn a_busy_slot_puts_a_deletion_off_after_the_bounded_wait() {
        let host = tempfile::tempdir().unwrap();
        let _other = contender_takes_slot(host.path()).unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert!(!before_unattended_unlink(None));
        assert_eq!(clock.slept.get(), HOST_DELETER_ACQUIRE_WAIT);
        // Sticky for the pass: later batches and root unlinks refuse at
        // once instead of waiting again.
        assert!(host_deleter_busy_this_pass());
        assert!(!before_unattended_unlink(None));
        assert!(!ensure_host_deleter_slot(None));
        assert_eq!(clock.slept.get(), HOST_DELETER_ACQUIRE_WAIT);
        assert_eq!(enter_unattended_deletion(), DeletionGate::HostDeleterBusy);
        assert_eq!(take_deletion_refusal(), Some(DeletionGate::HostDeleterBusy));
        assert_eq!(take_deletion_refusal(), None);
        clear_host_deleter_busy();
        assert!(!host_deleter_busy_this_pass());
        assert!(!ensure_host_deleter_slot(None));
        assert!(pin_host_deleter_slot().is_none());
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_busy, 4);
        assert_eq!(summary.host_slot_acquisitions, 0);
        assert_eq!(
            Duration::from_millis(summary.host_lock_wait_ms),
            HOST_DELETER_ACQUIRE_WAIT * 3 + HOST_DELETER_PIN_WAIT
        );
    }

    /// Memory and load waits never hold the slot: it goes back before the
    /// pacer sleeps, and is taken again for the next batch.
    #[test]
    fn pressure_waits_never_hold_the_slot() {
        let host = tempfile::tempdir().unwrap();
        let (clock, probe) = probing_clock(host.path());
        let load = PressureSample {
            memory_elevated: Some(false),
            load_elevated: Some(true),
        };
        // First batch clear; the second waits out memory, then load (its
        // bounded wait), then goes on.
        let (pressure, _reads) = scripted(vec![
            memory(false),
            memory(true),
            memory(true),
            load,
            memory(false),
        ]);
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, clock, pressure)
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let _removal = TreeRemovalSlotScope::enter();
        for _ in 0..PACING_BATCH_ENTRIES {
            assert!(before_unattended_unlink(None));
        }
        assert!(host_deleter_slot_held());
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held(), "taken again for the next batch");
        let sleeps = probe.free_during_sleeps.borrow().clone();
        assert!(sleeps.len() >= 3, "{sleeps:?}");
        assert!(sleeps.iter().all(|free| *free), "{sleeps:?}");
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 2);
        assert!(summary.memory_pause_ms >= 2_000, "{summary:?}");
        assert!(summary.load_pause_ms >= 1_000, "{summary:?}");
        // The pressure waits are not part of any hold.
        assert_eq!(summary.host_slot_held_ms, 0, "{summary:?}");
    }

    /// When taking the slot meant waiting, memory pressure is read again
    /// once it is held, so the check stays right before the deletion; if
    /// it rose, the slot goes back and the pressure is waited out first.
    #[test]
    fn memory_is_read_again_once_a_waited_for_slot_is_held() {
        let host = tempfile::tempdir().unwrap();
        let (clock, probe) = probing_clock(host.path());
        // The other worker gives the slot back during this one's wait.
        *probe.release_on_first_sleep.borrow_mut() = contender_takes_slot(host.path());
        let (pressure, reads) = scripted(vec![
            memory(false),
            memory(true),
            memory(false),
            memory(false),
        ]);
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, clock, pressure)
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let _removal = TreeRemovalSlotScope::enter();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held());
        // Clear, then elevated once the slot was held (given back), then
        // clear again before it is taken at once.
        assert_eq!(reads.get(), 3);
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 2);
        assert!(summary.host_lock_wait_ms > 0, "{summary:?}");
    }

    /// The whole-checkout gate samples memory once it holds the slot and
    /// never waits under the GC lock: elevated, it gives the slot back and
    /// refuses.
    #[test]
    fn the_checkout_gate_refuses_under_memory_pressure_without_waiting() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let (pressure, _reads) = scripted(vec![memory(true), memory(false)]);
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), pressure)
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert_eq!(enter_unattended_deletion(), DeletionGate::MemoryBackoff);
        assert_eq!(clock.slept.get(), Duration::ZERO);
        assert!(!host_deleter_slot_held());
        assert_eq!(take_deletion_refusal(), Some(DeletionGate::MemoryBackoff));
        assert_eq!(enter_unattended_deletion(), DeletionGate::Proceed);
        assert!(host_deleter_slot_held());
        release_host_deleter_slot();
        assert!(contender_takes_slot(host.path()).is_some());
    }

    /// A lane that pins the slot keeps it through pressure waits and tree
    /// removals inside it, and gives it back when the pin drops.
    #[test]
    fn a_pinned_lane_keeps_the_slot_until_the_pin_drops() {
        let host = tempfile::tempdir().unwrap();
        let (clock, probe) = probing_clock(host.path());
        let (pressure, _reads) = scripted(vec![memory(true), memory(false)]);
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, clock, pressure)
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let pin = pin_host_deleter_slot().expect("free slot");
        {
            let _removal = TreeRemovalSlotScope::enter();
            assert!(before_unattended_unlink(None));
        }
        release_host_deleter_slot();
        assert!(host_deleter_slot_held(), "the pin outlives a removal");
        assert_eq!(probe.free_during_sleeps.borrow().as_slice(), &[false]);
        drop(pin);
        assert!(!host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_some());
    }

    /// A tree removal that has held the slot for the maximum hands it over:
    /// it gives it back and leaves it free for longer than a waiter's poll
    /// before it tries again.
    #[test]
    fn a_long_tree_removal_hands_the_slot_over() {
        let host = tempfile::tempdir().unwrap();
        let (clock, probe) = probing_clock(host.path());
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, clock, calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let _removal = TreeRemovalSlotScope::enter();
        for _ in 0..PACING_BATCH_ENTRIES {
            assert!(before_unattended_unlink(None));
        }
        probe.now.set(probe.now.get() + HOST_DELETER_MAX_HOLD);
        assert!(before_unattended_unlink(None));
        assert_eq!(probe.free_during_sleeps.borrow().as_slice(), &[true]);
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 2);
        assert!(
            Duration::from_millis(summary.host_slot_held_ms) >= HOST_DELETER_MAX_HOLD,
            "{summary:?}"
        );
    }

    /// The charge for a whole-checkout removal keeps the slot that removal
    /// took (its sleep is the host's rate limit) and never takes one.
    #[test]
    fn a_charge_keeps_the_slot_its_removal_took_and_takes_none() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let entries = u64::from(PACING_BATCH_ENTRIES) * 2 + 1;
        // No removal took the slot: the charge takes none.
        charge_bulk_unattended_removal(entries, Duration::ZERO, None);
        assert!(!host_deleter_slot_held());
        assert_eq!(enter_unattended_deletion(), DeletionGate::Proceed);
        charge_bulk_unattended_removal(entries, Duration::ZERO, None);
        assert!(host_deleter_slot_held());
        release_host_deleter_slot();
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 1);
        // The charge's two batches, and the two entries of its partial batch
        // settled before the slot went back.
        assert_eq!(summary.host_slot_held_ms, 514, "{summary:?}");
        assert_eq!(summary.host_slot_deleting_ms, 0, "{summary:?}");
    }

    /// At most `HOST_JUDGING_PERMITS` workers judge at once; a third waits
    /// the bounded wait and is refused, and gets one once a permit drops.
    /// Without a host directory judging is not bounded.
    #[test]
    fn at_most_two_workers_judge_at_once() {
        assert_eq!(HOST_JUDGING_PERMITS, 2);
        assert!(take_judging_permit().is_some(), "no pacer: not bounded");
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let first = take_judging_permit().expect("a free permit");
        let _second = take_judging_permit().expect("a second free permit");
        assert_eq!(clock.slept.get(), Duration::ZERO);
        assert!(take_judging_permit().is_none());
        assert_eq!(clock.slept.get(), HOST_JUDGING_PERMIT_WAIT);
        drop(first);
        assert!(take_judging_permit().is_some());
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.judge_permit_busy, 1);
        assert_eq!(
            Duration::from_millis(summary.judge_permit_wait_ms),
            HOST_JUDGING_PERMIT_WAIT
        );
        // The deleter slot is a separate lock: judging never blocks it.
        assert!(contender_takes_slot(host.path()).is_some());
    }
    /// No unattended unlink happens without the slot, whatever the batch
    /// counter says: a second tree removal right after one that stopped
    /// mid-batch takes the slot, after a pressure sample, at its first
    /// unlink, and so does one that follows a charge paid without a slot.
    #[test]
    fn every_tree_removal_takes_the_slot_at_its_first_unlink() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let (pressure, reads) = scripted(vec![memory(false)]);
        let _scope = install(
            UnattendedPacer::new(
                UnlinkRate::Limited(1_000),
                Box::new(clock.clone()),
                pressure,
            )
            .with_host_directory(Some(host.path().to_path_buf())),
        );
        for removal in 0..2 {
            let _removal = TreeRemovalSlotScope::enter();
            let sampled_before = reads.get();
            assert!(before_unattended_unlink(None), "removal {removal}");
            assert!(host_deleter_slot_held(), "removal {removal}");
            assert!(
                contender_takes_slot(host.path()).is_none(),
                "removal {removal}: nobody else deletes during its first unlink"
            );
            assert!(reads.get() > sampled_before, "pressure read first");
            for _ in 1..100 {
                assert!(before_unattended_unlink(None));
            }
        }
        assert!(!host_deleter_slot_held());
        // A charge paid with no slot, at a limited rate (so it runs the
        // batch gate), leaves the counter mid-batch: 300 entries is a whole
        // batch and 44 more.
        charge_bulk_unattended_removal(300, Duration::ZERO, None);
        assert!(!host_deleter_slot_held());
        assert_eq!(with_active(0, |pacer| pacer.in_batch), 44);
        // The gate itself, not the scope, must take the slot here.
        let sampled_before = reads.get();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_none());
        assert!(reads.get() > sampled_before);
        release_host_deleter_slot();
        let _removal = TreeRemovalSlotScope::enter();
        let sampled_before = reads.get();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_none());
        assert!(reads.get() > sampled_before);
        assert_eq!(current_summary(0).unwrap().host_slot_acquisitions, 4);
    }

    /// Load makes a charge give its removal's slot back mid-charge (the
    /// pacer never holds it through a pressure wait); a tree removal right
    /// after, with the counter mid-batch and no slot held, takes it again at
    /// its first unlink.
    #[test]
    fn a_tree_removal_after_load_released_a_charges_slot_takes_it_again() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let load = PressureSample {
            memory_elevated: Some(false),
            load_elevated: Some(true),
        };
        // The checkout gate clear, then load through the charge's first
        // wait, then clear for good.
        let (pressure, reads) = scripted(vec![memory(false), load, load, memory(false)]);
        let _scope = install(
            UnattendedPacer::new(
                UnlinkRate::Limited(1_000),
                Box::new(clock.clone()),
                pressure,
            )
            .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert_eq!(enter_unattended_deletion(), DeletionGate::Proceed);
        assert!(host_deleter_slot_held());
        charge_bulk_unattended_removal(300, Duration::ZERO, None);
        assert!(!host_deleter_slot_held(), "load gave the slot back");
        assert_ne!(with_active(0, |pacer| pacer.in_batch), 0);
        let sampled_before = reads.get();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_none());
        assert!(reads.get() > sampled_before, "pressure read first");
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 2);
        assert!(summary.load_pause_ms >= 1_000, "{summary:?}");
    }

    /// A host directory that cannot hold the slot turns serialisation off
    /// only until a later take succeeds, and both transitions are counted.
    #[test]
    fn a_transient_host_directory_failure_ends_when_a_take_succeeds() {
        let base = tempfile::tempdir().unwrap();
        let blocker = base.path().join("host");
        std::fs::write(&blocker, "not a directory").unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(blocker.join("run"))),
        );
        {
            let _removal = TreeRemovalSlotScope::enter();
            assert!(before_unattended_unlink(None), "unserialised, as before");
            assert!(!host_deleter_slot_held());
        }
        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        let _removal = TreeRemovalSlotScope::enter();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held(), "serialised again");
        assert!(contender_takes_slot(&blocker.join("run")).is_none());
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_unavailable, 1);
        assert_eq!(summary.host_slot_recovered, 1);
    }

    /// The slot taken for a tree's witness or root unlink reads memory
    /// pressure first; elevated, it goes back and the unlink waits.
    #[test]
    fn a_root_unlinks_slot_reads_memory_pressure_first() {
        let host = tempfile::tempdir().unwrap();
        let (pressure, reads) = scripted(vec![memory(true), memory(false)]);
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(FakeClock::new()), pressure)
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert!(!ensure_host_deleter_slot(None));
        assert!(!host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_some());
        assert_eq!(reads.get(), 1);
        assert!(ensure_host_deleter_slot(None));
        assert!(host_deleter_slot_held());
        // Already held: no second read.
        assert!(ensure_host_deleter_slot(None));
        assert_eq!(reads.get(), 2);
        assert_eq!(current_summary(0).unwrap().pressure_yields, 1);
    }

    /// Rotation-debt recovery unlinks single control files outside the
    /// batch gate, so the worker pins the slot for the whole step; with the
    /// slot busy the step is left unfinished for the next pass, as a counted
    /// outcome and never a recovery error.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn rotation_debt_recovery_waits_for_the_slot_without_failing() {
        let host = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let _other = contender_takes_slot(host.path()).unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        let progress = crate::preparation::recover_rotation_debt_for_cache_sweep_with_binding(
            cache.path(),
            None,
            &cache.path().join("resources.db"),
            "repository",
            None,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("a busy slot is not a recovery error");
        assert!(!progress.complete);
        assert_eq!(progress.entries_reclaimed, 0);
        assert_eq!(clock.slept.get(), HOST_DELETER_ACQUIRE_WAIT);
        assert!(!host_deleter_slot_held());
    }

    /// Many small trees (50 entries, a fifth of a batch) each hold the slot
    /// for every one of their unlinks.
    #[test]
    fn many_small_tree_removals_each_hold_the_slot() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        for removal in 0..10 {
            let _removal = TreeRemovalSlotScope::enter();
            for unlink in 0..50 {
                assert!(before_unattended_unlink(None));
                if unlink == 0 || unlink == 49 {
                    assert!(
                        contender_takes_slot(host.path()).is_none(),
                        "removal {removal} unlink {unlink}"
                    );
                }
                after_unattended_unlink(0);
            }
        }
        let summary = current_summary(0).unwrap();
        assert_eq!(summary.host_slot_acquisitions, 10);
        assert_eq!(summary.entries_removed, 500);
        // Each removal's partial batch is paid before its slot goes back:
        // 500 entries at 1,000 a second.
        assert_eq!(summary.limiter_sleep_ms, 500, "{summary:?}");
    }

    /// A tree removal right after a checkout removal and its charge takes
    /// the slot at its first unlink: the charge's partial batch was settled
    /// when the slot went back.
    #[test]
    fn a_tree_removal_after_a_checkout_charge_takes_the_slot() {
        let host = tempfile::tempdir().unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Limited(1_000), Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert_eq!(enter_unattended_deletion(), DeletionGate::Proceed);
        charge_bulk_unattended_removal(300, Duration::ZERO, None);
        assert!(
            host_deleter_slot_held(),
            "the charge keeps its removal's slot"
        );
        release_host_deleter_slot();
        assert!(!host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_some());
        let _removal = TreeRemovalSlotScope::enter();
        assert!(before_unattended_unlink(None));
        assert!(host_deleter_slot_held());
        assert!(contender_takes_slot(host.path()).is_none());
        assert_eq!(current_summary(0).unwrap().host_slot_acquisitions, 2);
    }

    /// A tree's witness and root are unlinked outside the batch gate; they
    /// too wait for the slot, and a busy one leaves the tree in place for a
    /// later pass.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_trees_witness_and_root_wait_for_the_slot() {
        let host = tempfile::tempdir().unwrap();
        let trees = tempfile::tempdir().unwrap();
        let empty = trees.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let witnessed = trees.path().join("witnessed");
        std::fs::create_dir(&witnessed).unwrap();
        std::fs::write(
            witnessed.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let other = contender_takes_slot(host.path()).unwrap();
        let clock = FakeClock::new();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(clock.clone()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert_eq!(
            super::super::remove_condemned_tree(&empty, None, None).unwrap(),
            super::super::TreeRemoval::Interrupted
        );
        assert!(empty.is_dir());
        clear_host_deleter_busy();
        assert_eq!(
            super::super::remove_condemned_tree(&witnessed, Some("CACHEDIR.TAG"), None).unwrap(),
            super::super::TreeRemoval::Interrupted
        );
        assert!(
            witnessed.join("CACHEDIR.TAG").is_file(),
            "the witness stays"
        );
        drop(other);
        clear_host_deleter_busy();
        for (tree, witness) in [(&empty, None), (&witnessed, Some("CACHEDIR.TAG"))] {
            assert_eq!(
                super::super::remove_condemned_tree(tree, witness, None).unwrap(),
                super::super::TreeRemoval::Complete
            );
            assert!(!tree.exists());
        }
        assert!(
            !host_deleter_slot_held(),
            "given back when each removal returned"
        );
        assert!(contender_takes_slot(host.path()).is_some());
    }

    /// A gate whose pacer is already borrowed refuses: it never lets a
    /// deletion through unserialised by accident.
    #[test]
    fn a_borrowed_pacer_refuses_deletion() {
        let host = tempfile::tempdir().unwrap();
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Unlimited, Box::new(FakeClock::new()), calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        with_active((), |_pacer| {
            assert!(!before_unattended_unlink(None));
            assert!(!ensure_host_deleter_slot(None));
            assert_eq!(enter_unattended_deletion(), DeletionGate::HostDeleterBusy);
            assert!(pin_host_deleter_slot().is_none());
            assert!(take_judging_permit().is_none());
        });
        // The borrowed refusal is kept for the slice like any other, once.
        assert_eq!(take_deletion_refusal(), Some(DeletionGate::HostDeleterBusy));
        assert_eq!(take_deletion_refusal(), None);
        assert!(before_unattended_unlink(None));
    }

    /// The whole-checkout gate reads memory pressure even when no host
    /// directory serialises deletion.
    #[test]
    fn the_checkout_gate_reads_memory_pressure_without_a_host_directory() {
        let (pressure, reads) = scripted(vec![memory(true)]);
        let _scope = install(UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(FakeClock::new()),
            pressure,
        ));
        assert_eq!(enter_unattended_deletion(), DeletionGate::MemoryBackoff);
        assert_eq!(reads.get(), 1);
    }

    /// A charge that has held the slot for the maximum hands it over like a
    /// tree removal: it leaves it free for a moment, then takes it again.
    #[test]
    fn a_long_charge_hands_the_slot_over() {
        let host = tempfile::tempdir().unwrap();
        let (clock, probe) = probing_clock(host.path());
        let _scope = install(
            UnattendedPacer::new(UnlinkRate::Limited(1_000), clock, calm())
                .with_host_directory(Some(host.path().to_path_buf())),
        );
        assert_eq!(enter_unattended_deletion(), DeletionGate::Proceed);
        // 25,600 entries at 1,000 a second: more than the maximum hold.
        charge_bulk_unattended_removal(u64::from(PACING_BATCH_ENTRIES) * 100, Duration::ZERO, None);
        let sleeps = probe.free_during_sleeps.borrow().clone();
        assert!(sleeps.iter().any(|free| *free), "handed over once");
        assert!(
            sleeps.iter().filter(|free| **free).count() <= 2,
            "free only during the hand-over: {sleeps:?}"
        );
        assert!(host_deleter_slot_held(), "taken again after the hand-over");
        assert_eq!(current_summary(0).unwrap().host_slot_acquisitions, 2);
    }
}
