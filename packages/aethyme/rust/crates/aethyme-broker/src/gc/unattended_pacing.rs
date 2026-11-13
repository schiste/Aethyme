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
//! * reads cheap system signals and pauses, bounded by the caller's deadline
//!   and a per-run cap, while memory pressure is above normal or load is far
//!   above the core count. When the pause budget is spent it asks removal to
//!   stop resumably; the reclaim request stays durable for the next run.
//!
//! Everything here is observational: pacing never changes what is deleted.

use std::cell::RefCell;
use std::path::Path;
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
/// Total time one worker run may spend paused for pressure before it yields
/// with its request retained.
const MAX_PRESSURE_PAUSE_PER_RUN: Duration = Duration::from_secs(120);
/// One-minute load average per core above which the machine is "far above"
/// its capacity.
const LOAD_PER_CORE_LIMIT: f64 = 2.0;
/// Linux PSI thresholds (percent of the last ten seconds some task stalled).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PSI_MEMORY_SOME_AVG10_LIMIT: f64 = 10.0;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PSI_IO_SOME_AVG10_LIMIT: f64 = 40.0;

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
    /// Milliseconds paused because load (or Linux I/O pressure) was far above
    /// the core count.
    pub load_pause_ms: u64,
    /// Times removal stopped resumably because the pressure pause budget or
    /// the deadline ran out while pressure persisted.
    pub pressure_yields: u64,
    /// Milliseconds spent waiting for the host-wide single-deleter lock.
    pub host_lock_wait_ms: u64,
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
    pressure_paused: Duration,
    limiter_slept: Duration,
    memory_paused: Duration,
    load_paused: Duration,
    host_lock_waited: Duration,
    summary: UnattendedPacingSummary,
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
            pressure_paused: Duration::ZERO,
            limiter_slept: Duration::ZERO,
            memory_paused: Duration::ZERO,
            load_paused: Duration::ZERO,
            host_lock_waited: Duration::ZERO,
            summary: UnattendedPacingSummary {
                unlinks_per_second_limit: rate.limit(),
                ..UnattendedPacingSummary::default()
            },
        }
    }

    fn effective_limit(&self) -> Option<u32> {
        if self.disk_pressure {
            None
        } else {
            self.rate.limit()
        }
    }

    /// Gate one unlink. `false` asks the caller to stop resumably, exactly as
    /// an expired deadline does.
    pub(crate) fn before_unlink(&mut self, deadline: Option<Instant>) -> bool {
        if self.in_batch == 0 {
            if let Some(started) = self.batch_started.take() {
                self.pay_batch(started, deadline);
            }
            if !self.wait_out_pressure(deadline) {
                self.summary.pressure_yields = self.summary.pressure_yields.saturating_add(1);
                return false;
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
        let Some(limit) = self.effective_limit() else {
            return;
        };
        let minimum = Duration::from_nanos(
            u64::from(self.batch_entries).saturating_mul(1_000_000_000) / u64::from(limit.max(1)),
        );
        let now = self.clock.now();
        let Some(deficit) = minimum.checked_sub(now.saturating_duration_since(started)) else {
            return;
        };
        let sleep = cap_to_deadline(deficit, now, deadline);
        if !sleep.is_zero() {
            self.clock.sleep(sleep);
            self.limiter_slept += sleep;
        }
    }

    /// Pause while a signal reports pressure. Returns `false` when the pause
    /// budget or the deadline is spent with pressure still present.
    fn wait_out_pressure(&mut self, deadline: Option<Instant>) -> bool {
        loop {
            let sample = self.pressure.sample();
            let memory = sample.memory_elevated == Some(true);
            let load = sample.load_elevated == Some(true);
            if !memory && !load {
                return true;
            }
            let now = self.clock.now();
            let budget = MAX_PRESSURE_PAUSE_PER_RUN.saturating_sub(self.pressure_paused);
            let step = cap_to_deadline(PRESSURE_POLL.min(budget), now, deadline);
            if step.is_zero() {
                return false;
            }
            self.clock.sleep(step);
            self.pressure_paused += step;
            if memory {
                self.memory_paused += step;
            } else {
                self.load_paused += step;
            }
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
        UnattendedPacingSummary {
            at_ms,
            elapsed_ms,
            removal_rate_per_second,
            limiter_sleep_ms: duration_ms(self.limiter_slept),
            memory_pause_ms: duration_ms(self.memory_paused),
            load_pause_ms: duration_ms(self.load_paused),
            host_lock_wait_ms: duration_ms(self.host_lock_waited),
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

/// Tree-removal hook before one unlink. Without an installed pacer (every
/// foreground caller) this always permits the unlink immediately.
pub(crate) fn before_unattended_unlink(deadline: Option<Instant>) -> bool {
    with_active(true, |pacer| pacer.before_unlink(deadline))
}

/// Tree-removal hook after one successful unlink.
pub(crate) fn after_unattended_unlink(logical_bytes: u64) {
    with_active((), |pacer| pacer.after_unlink(logical_bytes));
}

/// Charge `entries` unlinks made in one burst outside tree removal (Git
/// removing a whole checkout) to the installed pacer: it sleeps off their
/// share of the bucket and waits out memory or load pressure as it would for
/// its own unlinks, never past `deadline`. Returns how many it charged. Call
/// it with no lock held: the sleep is the throttle. Without a pacer (every
/// foreground caller) it charges nothing and returns at once.
pub(crate) fn charge_bulk_unattended_removal(entries: u64, deadline: Option<Instant>) -> u64 {
    with_active(0, |pacer| {
        let mut charged = 0;
        while charged < entries
            && deadline.is_none_or(|deadline| pacer.clock.now() < deadline)
            && pacer.before_unlink(deadline)
        {
            pacer.after_unlink(0);
            charged += 1;
        }
        charged
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

pub(crate) fn record_host_lock_wait(waited: Duration) {
    with_active((), |pacer| pacer.host_lock_waited += waited);
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
        assert_eq!(charge_bulk_unattended_removal(10_000, None), 0);
        let clock = FakeClock::new();
        let _scope = install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(clock.clone()),
            calm(),
        ));
        let entries = u64::from(PACING_BATCH_ENTRIES) * 8 + 1;
        assert_eq!(charge_bulk_unattended_removal(entries, None), entries);
        assert_eq!(clock.slept.get(), Duration::from_millis(2_048));
        let summary = current_summary(0).expect("the pacer is installed");
        assert_eq!(summary.entries_removed, entries);
        // A deadline bounds the throttle.
        let deadline = clock.now() + Duration::from_millis(300);
        assert!(charge_bulk_unattended_removal(entries, Some(deadline)) < entries);
        assert_eq!(clock.now(), deadline);
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
    fn persistent_pressure_yields_within_the_deadline_and_the_run_budget() {
        let clock = FakeClock::new();
        let load = PressureSample {
            memory_elevated: Some(false),
            load_elevated: Some(true),
        };
        let mut pacer = UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples: vec![load],
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
        assert_eq!(summary.load_pause_ms, 120_000);
        assert_eq!(summary.entries_removed, 0);
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
        record_host_lock_wait(Duration::from_millis(40));
        set_disk_pressure(true);
        assert!(before_unattended_unlink(None));
        after_unattended_unlink(5);
        let summary = current_summary(9).unwrap();
        assert_eq!(summary.requests_coalesced, 3);
        assert_eq!(summary.host_lock_wait_ms, 40);
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
}
