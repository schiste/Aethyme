//! Command metrics for commands that never return: interrupted (`SIGINT`) or
//! terminated (`SIGTERM`, `SIGHUP`, an agent's tool timeout).
//!
//! The command runner writes its `command-metrics.jsonl` row after the command
//! returns, so a command killed by a signal used to leave no row at all: a
//! `submit` stopped three times after sitting an hour in one step left no
//! trace in any repository's metrics. While a recording command runs, this
//! module catches those three signals, writes the row the runner would have
//! written (exit `128 + signal`, the spans accumulated so far including the
//! time of the spans still open, and an `interrupted` field naming the
//! signal and the open spans), then restores the default disposition and
//! re-raises the signal, so the process still dies of it: the exit status a
//! parent sees, and what happens to children, are unchanged.
//!
//! The handler only records the signal and writes one byte to a pipe; a
//! watcher thread does the work. The flush is bounded ([`FLUSH_BUDGET`]): it
//! polls rather than waits for any lock the interrupted thread may hold, and
//! when the budget runs out the signal is re-raised whether or not the row
//! was written. A second signal while the row is being written kills at once.
//!
//! The same watcher keeps a heartbeat for a long command: every
//! [`HEARTBEAT_INTERVAL`] it writes `.aethyme/run/commands/<pid>.json` (the
//! command, its start, the spans open now), removed when the command ends, so
//! a stalled command can be attributed while it is still running.
//!
//! A signal that was ignored when the command started (`nohup`) stays
//! ignored, and a disposition someone else installed is left alone. A thread
//! that blocks these signals itself (a supervised `resources run` waiting for
//! them with `sigwait`) keeps receiving them: the watcher blocks them too, so
//! the handler only runs where the default action would have.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

/// Longest an interrupted command spends writing its row before it dies.
#[cfg_attr(not(unix), allow(dead_code))]
const FLUSH_BUDGET: Duration = Duration::from_secs(2);
/// Longest the row waits for the shared metrics append lock (held exclusively
/// only by a GC trim) before appending without it, as every command does.
#[cfg_attr(not(unix), allow(dead_code))]
const APPEND_LOCK_BUDGET: Duration = Duration::from_millis(500);
/// How often a running command refreshes its heartbeat file.
#[cfg_attr(not(unix), allow(dead_code))]
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Overrides [`HEARTBEAT_INTERVAL`] in milliseconds (at least 50).
#[cfg_attr(not(unix), allow(dead_code))]
const HEARTBEAT_INTERVAL_ENV: &str = "AETHYME_COMMAND_HEARTBEAT_MS";
/// Holds a command inside the `test_command_hold` span for this many
/// milliseconds before it runs: the seam the interrupted-metrics tests stop.
const TEST_HOLD_ENV: &str = "AETHYME_TEST_COMMAND_HOLD_MS";
/// Heartbeat files, relative to the main checkout.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const HEARTBEAT_DIR: &str = ".aethyme/run/commands";

/// What the watcher needs to write a running command's row from another
/// thread: the runner keeps the rest in thread-locals.
#[cfg_attr(not(unix), allow(dead_code))]
struct Running {
    args: Vec<String>,
    started: Instant,
    started_unix_ms: i64,
    recorder: crate::phase_telemetry::Recorder,
    output_before: u64,
    cwd: Option<PathBuf>,
    /// The heartbeat file once one was written, removed when the command ends.
    heartbeat: Option<PathBuf>,
    /// The main checkout, resolved once, by the watcher, when first needed.
    main_root: Option<PathBuf>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
const NO_SESSION: i64 = i64::MIN;
static SESSION: AtomicI64 = AtomicI64::new(NO_SESSION);
static REPOSITORY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The session the running command resolved, for its interrupted row.
pub(super) fn note_session(id: i64) {
    SESSION.store(id, Ordering::Relaxed);
}

/// The repository a command that can remove its own cwd recorded.
pub(super) fn note_repository(root: &Path) {
    if let Ok(mut repository) = REPOSITORY.try_lock() {
        *repository = Some(root.to_path_buf());
    }
}

/// Catches the termination signals while alive; see the module docs.
pub(super) struct Scope {
    #[cfg(unix)]
    handlers: Option<unix::Handlers>,
}

/// Start covering a command that will write a metrics row. A command that
/// would not write one (`command_metric_enabled` is false) is not covered:
/// read-only inspection stays telemetry-free even when interrupted.
pub(super) fn enter(
    args: &[String],
    started: Instant,
    recorder: &crate::phase_telemetry::Recorder,
) -> Option<Scope> {
    if !super::telemetry::command_metric_enabled(
        args,
        super::telemetry::output_measurement_opted_in(),
        false,
    ) {
        return None;
    }
    let started_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    SESSION.store(NO_SESSION, Ordering::Relaxed);
    if let Ok(mut repository) = REPOSITORY.lock() {
        *repository = None;
    }
    let running = Running {
        args: args.to_vec(),
        started,
        started_unix_ms,
        recorder: recorder.clone(),
        output_before: crate::cli_output::process_emitted_bytes(),
        cwd: std::env::current_dir().ok(),
        heartbeat: None,
        main_root: None,
    };
    *RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(running);
    Some(Scope {
        #[cfg(unix)]
        handlers: unix::Handlers::install(),
    })
}

impl Drop for Scope {
    fn drop(&mut self) {
        #[cfg(unix)]
        drop(self.handlers.take());
        let running = RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(path) = running.and_then(|running| running.heartbeat) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The test seam: hold inside the `test_command_hold` span when asked to.
pub(super) fn test_hold() {
    let Some(millis) = std::env::var(TEST_HOLD_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    let _span = crate::phase_telemetry::span("test_command_hold");
    std::thread::sleep(Duration::from_millis(millis));
}

#[cfg_attr(not(unix), allow(dead_code))]
fn signal_name(signal: i32) -> &'static str {
    #[cfg(unix)]
    {
        match signal {
            libc::SIGINT => "SIGINT",
            libc::SIGTERM => "SIGTERM",
            libc::SIGHUP => "SIGHUP",
            _ => "other",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        "other"
    }
}

/// The value of `--session` before any `--`, unvalidated: the interrupted
/// row never opens the broker store, which the interrupted thread may hold.
#[cfg_attr(not(unix), allow(dead_code))]
fn session_flag(args: &[String]) -> Option<i64> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            return None;
        }
        if arg == "--session" {
            return args.next()?.parse().ok();
        }
        if super::VALUE_FLAGS.contains(&arg.as_str()) {
            args.next();
        }
    }
    None
}

#[cfg_attr(not(unix), allow(dead_code))]
fn lock_within<T>(mutex: &Mutex<T>, deadline: Instant) -> Option<std::sync::MutexGuard<'_, T>> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn resolve_main_root(cwd: &Path) -> Option<PathBuf> {
    crate::GitRepo::discover(cwd).ok()?.main_root().ok()
}

/// The checkout a row or heartbeat belongs to: the repository the command
/// recorded, else its starting directory. Resolved once and cached.
#[cfg_attr(not(unix), allow(dead_code))]
fn main_root_for(deadline: Instant) -> Option<PathBuf> {
    let cwd = {
        let running = lock_within(&RUNNING, deadline)?;
        let running = running.as_ref()?;
        if let Some(root) = &running.main_root {
            return Some(root.clone());
        }
        let recorded = lock_within(&REPOSITORY, deadline).and_then(|path| path.clone());
        recorded.or_else(|| running.cwd.clone())?
    };
    let root = resolve_main_root(&cwd)?;
    if let Some(mut running) = lock_within(&RUNNING, deadline)
        && let Some(running) = running.as_mut()
    {
        running.main_root = Some(root.clone());
    }
    Some(root)
}

#[cfg_attr(not(unix), allow(dead_code))]
fn open_spans_json(open: &[crate::phase_telemetry::OpenSpan]) -> serde_json::Value {
    serde_json::Value::Array(open.iter().map(|span| span.name.into()).collect())
}

/// Elapsed milliseconds per open span name; a name open more than once (two
/// worker threads in the same step) reports the longest.
#[cfg_attr(not(unix), allow(dead_code))]
fn open_elapsed_json(open: &[crate::phase_telemetry::OpenSpan]) -> serde_json::Value {
    let mut elapsed = serde_json::Map::new();
    for span in open {
        let millis = span.elapsed.as_millis() as u64;
        let longest = elapsed
            .get(span.name)
            .and_then(serde_json::Value::as_u64)
            .map_or(millis, |seen| seen.max(millis));
        elapsed.insert(span.name.to_string(), longest.into());
    }
    serde_json::Value::Object(elapsed)
}

/// Write the interrupted command's row. Every wait is bounded by `deadline`.
#[cfg_attr(not(unix), allow(dead_code))]
fn write_interrupted_row(signal: i32, deadline: Instant) {
    let ended = Instant::now();
    let Some((args, started, recorder, output_before)) = ({
        let running = lock_within(&RUNNING, deadline);
        running.and_then(|running| {
            running.as_ref().map(|running| {
                (
                    running.args.clone(),
                    running.started,
                    running.recorder.clone(),
                    running.output_before,
                )
            })
        })
    }) else {
        return;
    };
    let Some(label) = super::telemetry::safe_command_surface(&args) else {
        return;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (spans, open) = recorder
        .snapshot_closing_open_spans(ended, remaining / 4)
        .unwrap_or_default();
    let mut details = super::telemetry::with_spans(serde_json::Map::new(), &spans);
    if let Some(cpu) = super::telemetry::process_cpu_ms() {
        details.insert("cpu_ms".into(), cpu);
    }
    details.insert(
        "interrupted".into(),
        serde_json::json!({
            "signal": signal_name(signal),
            "in_span": open.last().map(|span| span.name),
            "open_spans": open_spans_json(&open),
            "open_span_elapsed_ms": open_elapsed_json(&open),
        }),
    );
    let session = match SESSION.load(Ordering::Relaxed) {
        NO_SESSION => session_flag(&args),
        id => Some(id),
    };
    if args.first().is_some_and(|command| command == "hook") && session.is_none() {
        return;
    }
    let Some(main_root) = main_root_for(deadline) else {
        return;
    };
    let line = super::telemetry::command_metric_line(
        &label,
        (ended - started).as_millis() as i64,
        u8::try_from(128 + signal).unwrap_or(u8::MAX),
        crate::cli_output::process_emitted_bytes().saturating_sub(output_before),
        session,
        spans.attribution(started, ended),
        details,
    );
    let Some(path) = super::telemetry::command_metrics_path(&main_root) else {
        return;
    };
    let wait = APPEND_LOCK_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
    crate::gc::metrics_trim::append_command_metric_line_within(
        &main_root,
        &path,
        line.as_bytes(),
        wait,
    );
}

/// Remove the heartbeat file of the command being interrupted.
#[cfg_attr(not(unix), allow(dead_code))]
fn remove_heartbeat(deadline: Instant) {
    if let Some(running) = lock_within(&RUNNING, deadline)
        && let Some(path) = running
            .as_ref()
            .and_then(|running| running.heartbeat.clone())
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Refresh the running command's heartbeat file. Only in a checkout that
/// already has `.aethyme/`: a heartbeat never creates repository state.
#[cfg_attr(not(unix), allow(dead_code))]
fn write_heartbeat() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let Some(main_root) = main_root_for(deadline) else {
        return;
    };
    if !main_root.join(".aethyme").is_dir() {
        return;
    }
    // Held while writing, so a command that ends meanwhile removes the file
    // after it is written, never before.
    let Some(mut running) = lock_within(&RUNNING, deadline) else {
        return;
    };
    let Some(running) = running.as_mut() else {
        return;
    };
    let Some(label) = super::telemetry::safe_command_surface(&running.args) else {
        return;
    };
    let now = Instant::now();
    let open = running.recorder.open_spans(now);
    let updated_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    let session = match SESSION.load(Ordering::Relaxed) {
        NO_SESSION => session_flag(&running.args),
        id => Some(id),
    };
    let record = serde_json::json!({
        "schema_version": 1,
        "pid": std::process::id(),
        "aethyme_version": env!("CARGO_PKG_VERSION"),
        "command": label,
        "session_id": session,
        "started_at_ms": running.started_unix_ms,
        "updated_at_ms": updated_at_ms,
        "elapsed_ms": (now - running.started).as_millis() as u64,
        "in_span": open.last().map(|span| span.name),
        "open_spans": open_spans_json(&open),
        "open_span_elapsed_ms": open_elapsed_json(&open),
    });
    let directory = main_root.join(HEARTBEAT_DIR);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join(format!("{}.json", std::process::id()));
    let temporary = directory.join(format!(".{}.json.tmp", std::process::id()));
    let Ok(bytes) = serde_json::to_vec(&record) else {
        return;
    };
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, &path).is_ok() {
        running.heartbeat = Some(path);
    } else {
        let _ = std::fs::remove_file(&temporary);
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn heartbeat_interval() -> Duration {
    std::env::var(HEARTBEAT_INTERVAL_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(HEARTBEAT_INTERVAL, |millis| {
            Duration::from_millis(millis.max(50))
        })
}

#[cfg(unix)]
mod unix {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::Instant;

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The pipe's write end, for the handler; `-1` until the watcher runs.
    static PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);
    /// The first signal caught, `0` before any.
    static CAUGHT: AtomicI32 = AtomicI32::new(0);
    static WATCHER: OnceLock<bool> = OnceLock::new();

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: `__errno_location` returns this thread's errno slot.
        unsafe { libc::__errno_location() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: `__error` returns this thread's errno slot.
        unsafe { libc::__error() }
    }

    /// Async-signal-safe: atomics, `write`, `signal` and `raise` only, and
    /// errno is restored.
    extern "C" fn on_signal(signal: libc::c_int) {
        // SAFETY: the errno slot of the interrupted thread, valid for the
        // thread's lifetime; read and restored around `write`.
        let saved = unsafe { *errno_location() };
        if CAUGHT
            .compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // A second signal while the row is written: die now. The signal
            // is blocked while this handler runs, so it is delivered with the
            // default disposition as soon as the handler returns.
            // SAFETY: `signal` and `raise` are async-signal-safe and `signal`
            // is one of the caught signals.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        } else {
            let fd = PIPE_WRITE.load(Ordering::SeqCst);
            if fd >= 0 {
                let byte = signal as u8;
                // SAFETY: `write` is async-signal-safe; `byte` is a live local
                // and the descriptor is the pipe's non-blocking write end,
                // which is never closed.
                unsafe {
                    libc::write(fd, (&raw const byte).cast(), 1);
                }
            }
        }
        // SAFETY: as above.
        unsafe { *errno_location() = saved };
    }

    fn set_cloexec(fd: libc::c_int, nonblocking: bool) -> bool {
        // SAFETY: `fcntl` on a descriptor this module owns, with integer
        // arguments only.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                return false;
            }
            if nonblocking {
                let status = libc::fcntl(fd, libc::F_GETFL);
                if status < 0 || libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) < 0 {
                    return false;
                }
            }
        }
        true
    }

    /// Start the watcher thread once per process; whether it runs.
    fn ensure_watcher() -> bool {
        *WATCHER.get_or_init(|| {
            let mut fds: [libc::c_int; 2] = [0; 2];
            // SAFETY: `fds` is a live two-element array `pipe` fills.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return false;
            }
            let [read, write] = fds;
            if !set_cloexec(read, false) || !set_cloexec(write, true) {
                // SAFETY: both descriptors were just created here.
                unsafe {
                    libc::close(read);
                    libc::close(write);
                }
                return false;
            }
            let spawned = std::thread::Builder::new()
                .name("aethyme-interrupt-watcher".into())
                .spawn(move || watch(read));
            if spawned.is_err() {
                // SAFETY: as above; the thread that would own them never ran.
                unsafe {
                    libc::close(read);
                    libc::close(write);
                }
                return false;
            }
            PIPE_WRITE.store(write, Ordering::SeqCst);
            true
        })
    }

    fn signal_set() -> libc::sigset_t {
        // SAFETY: `sigset_t` is plain data; `sigemptyset` initialises it and
        // `sigaddset` adds valid signal numbers.
        unsafe {
            let mut set = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut set);
            for signal in SIGNALS {
                libc::sigaddset(&mut set, signal);
            }
            set
        }
    }

    fn watch(read: libc::c_int) {
        // The handler must never run here: a thread that blocks these signals
        // itself relies on the process having no other thread that takes them.
        let set = signal_set();
        // SAFETY: `set` is a live, initialised signal set; only this thread's
        // mask changes.
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let interval = super::heartbeat_interval();
        let mut next_heartbeat = Instant::now() + interval;
        loop {
            let timeout = next_heartbeat
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as libc::c_int;
            let mut poll = libc::pollfd {
                fd: read,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` is one live, initialised `pollfd`.
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready > 0 {
                let mut byte = 0u8;
                // SAFETY: reads at most one byte into a live local.
                unsafe {
                    libc::read(read, (&raw mut byte).cast(), 1);
                }
                let signal = CAUGHT.load(Ordering::SeqCst);
                if signal != 0 {
                    interrupted(signal);
                }
            } else if ready == 0 {
                super::write_heartbeat();
                next_heartbeat = Instant::now() + interval;
            }
        }
    }

    /// Write the row (bounded), then die of `signal` as the default
    /// disposition would have.
    fn interrupted(signal: libc::c_int) -> ! {
        let deadline = Instant::now() + super::FLUSH_BUDGET;
        let (sender, receiver) = std::sync::mpsc::channel();
        let flush = std::thread::Builder::new()
            .name("aethyme-interrupt-flush".into())
            .spawn(move || {
                super::write_interrupted_row(signal, deadline);
                super::remove_heartbeat(deadline);
                let _ = sender.send(());
            });
        if flush.is_ok() {
            let _ = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
        // SAFETY: restoring the default disposition and unblocking `signal`
        // on this thread only; `raise` then delivers it here, and its default
        // action ends the process.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            let mut set = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::raise(signal);
        }
        // Unreachable unless the signal could not be delivered.
        // SAFETY: `_exit` ends the process without running anything else.
        unsafe { libc::_exit(128 + signal) }
    }

    /// The dispositions replaced while a command runs, restored on drop.
    pub(super) struct Handlers {
        replaced: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl Handlers {
        pub(super) fn install() -> Option<Self> {
            if !ensure_watcher() {
                return None;
            }
            let mut replaced = Vec::new();
            for signal in SIGNALS {
                // SAFETY: `sigaction` is plain data; zeroed is a valid output.
                let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
                // SAFETY: a null new action only reads the current one.
                if unsafe { libc::sigaction(signal, std::ptr::null(), &mut previous) } != 0 {
                    continue;
                }
                // Ignored (`nohup`) or handled by someone else: leave it.
                if previous.sa_sigaction != libc::SIG_DFL {
                    continue;
                }
                // SAFETY: as above.
                let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                // SAFETY: `sa_mask` is a live field `sigemptyset` initialises.
                unsafe { libc::sigemptyset(&mut action.sa_mask) };
                // SAFETY: both structs are live for the call.
                if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } == 0 {
                    replaced.push((signal, previous));
                }
            }
            Some(Self { replaced })
        }
    }

    impl Drop for Handlers {
        fn drop(&mut self) {
            for (signal, previous) in &self.replaced {
                // SAFETY: restores a disposition `sigaction` returned.
                unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_flag_stops_at_the_wrapped_command() {
        let args = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(
            session_flag(&args(&["submit", "--session", "12"])),
            Some(12)
        );
        assert_eq!(
            session_flag(&args(&["exec", "--", "--session", "12"])),
            None
        );
        assert_eq!(session_flag(&args(&["submit", "--session", "x"])), None);
    }

    #[test]
    fn open_elapsed_keeps_the_longest_per_name() {
        let open = [
            crate::phase_telemetry::OpenSpan {
                name: "gate_run",
                elapsed: Duration::from_millis(9),
            },
            crate::phase_telemetry::OpenSpan {
                name: "subprocess_other",
                elapsed: Duration::from_millis(4),
            },
            crate::phase_telemetry::OpenSpan {
                name: "subprocess_other",
                elapsed: Duration::from_millis(7),
            },
        ];
        assert_eq!(
            open_elapsed_json(&open),
            serde_json::json!({"gate_run": 9, "subprocess_other": 7})
        );
        assert_eq!(
            open_spans_json(&open),
            serde_json::json!(["gate_run", "subprocess_other", "subprocess_other"])
        );
    }
}
