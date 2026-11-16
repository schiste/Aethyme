//! Which agent process holds a session (#393).
//!
//! A session id used to be a bearer token: any process that knew `694` could
//! submit, push or force-push for session 694. The holder is the long-lived
//! agent process that started or adopted the session, identified by its pid
//! and start time so a recycled pid is not mistaken for it.
//!
//! An environment token cannot carry this. Agent harnesses run every command
//! in a fresh shell, so nothing exported by one command reaches the next. What
//! does persist is the process tree: each of those shells is a child of the
//! same agent runtime. The caller is therefore the nearest ancestor that is a
//! known agent runtime (`claude`, `codex`), or the process named by
//! `AETHYME_AGENT_PID`.
//!
//! A caller with no such ancestor -- an operator script reparented to launchd,
//! a CI job -- is unidentified. It is let through: refusing it would break
//! every unattended script, and it cannot be told apart from a second agent
//! anyway.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub(crate) const PROCESS_TABLE_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_TABLE_STREAM_LIMIT: usize = 4 * 1024 * 1024;

/// Environment override for the caller's agent process: a pid, or `0`/`none`
/// to declare the caller unidentified.
pub const AGENT_PID_ENV: &str = "AETHYME_AGENT_PID";

/// The finite holder lease written by current broker builds. Expiry is only
/// actionable when the repository explicitly enables the task-lease policy.
pub(crate) const TASK_LEASE_VERSION: u32 = 1;
pub(crate) const TASK_LEASE_DURATION_MS: i64 = 24 * 60 * 60 * 1_000;
pub(crate) const TASK_LEASE_RENEWAL_INTERVAL_MS: i64 = 5 * 60 * 1_000;
/// How long one open-file observation answers for every retirement candidate
/// of a pass. It equals the re-take interval, so a pass is covered by one
/// observation at a time (at most four per pass) instead of each of hundreds
/// of candidates asking for a system-wide `lsof`. A close resting on an
/// observation up to this old deletes nothing; checkout removal checks
/// process use again before it removes anything.
pub(crate) const TASK_CLOSE_PROCESS_USE_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(10);
/// A pass takes another open-file observation for a later retirement
/// candidate only while at least this much of its deadline remains.
const PROCESS_USE_RESNAPSHOT_MIN_BUDGET: Duration = Duration::from_secs(3);
/// At most one open-file observation per this interval within a pass.
const PROCESS_USE_RESNAPSHOT_MIN_INTERVAL: Duration = Duration::from_secs(10);
/// At most this many open-file observations per pass.
const PROCESS_USE_MAX_SNAPSHOTS_PER_PASS: usize = 4;
/// A failed process-table snapshot is retried at once, then again only after
/// this interval: a `ps` that timed out once on a loaded host usually answers
/// a few seconds later, and one failure must not leave a whole pass unknown.
pub(crate) const PROCESS_TABLE_RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// At most this many process-table retries per pass.
pub(crate) const PROCESS_TABLE_MAX_RETRIES_PER_PASS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskLease {
    pub version: u32,
    pub duration_ms: i64,
}

/// Outcome of an explicit same-holder task-lease heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskLeaseHeartbeat {
    /// The five-minute renewal interval elapsed and a new holder event landed.
    Renewed,
    /// The current binding is still fresh; only session activity was touched.
    Current,
    /// A valid legacy binding was upgraded by its current holder under guard.
    Upgraded,
    /// The current process is not the exact recorded holder.
    NotHolder,
    /// The holder event or lease version is malformed or unsupported.
    LegacyProtected,
}

impl TaskLeaseHeartbeat {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Renewed => "renewed",
            Self::Current => "current",
            Self::Upgraded => "upgraded",
            Self::NotHolder => "not_holder",
            Self::LegacyProtected => "legacy_protected",
        }
    }
}

/// Whether an unsupported command safely removed age-expiry authority before
/// it was allowed to act for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskLeaseDowngrade {
    NotFinite,
    Downgraded,
    NotHolder,
}

impl TaskLease {
    pub(crate) const fn current() -> Self {
        Self {
            version: TASK_LEASE_VERSION,
            duration_ms: TASK_LEASE_DURATION_MS,
        }
    }

    pub(crate) fn is_supported(self) -> bool {
        self.version == TASK_LEASE_VERSION && self.duration_ms == TASK_LEASE_DURATION_MS
    }
}

/// One agent process, as identified by `ps`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentProcess {
    pub pid: i64,
    /// `ps -o lstart` text. Paired with the pid so a recycled pid is a
    /// different process.
    pub started: String,
    pub command: String,
}

/// Who is running this broker command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Agent(AgentProcess),
    Unidentified,
}

#[derive(Debug, Clone)]
struct ProcessRow {
    ppid: i64,
    started: String,
    /// The command line, as `ps -o args` prints it with whitespace runs
    /// collapsed. A native listing reads it only when it is first needed.
    args: std::sync::OnceLock<String>,
    /// The short command name a native listing shows, `(name)` like `ps`,
    /// when the command line itself cannot be read.
    comm: String,
}

impl ProcessRow {
    fn args(&self, pid: i64) -> &str {
        self.args
            .get_or_init(|| native_process_args(pid, &self.comm))
    }
}

/// One snapshot of the process table: a single `ps` call, walked in memory.
#[derive(Debug, Default)]
pub struct ProcessTable {
    rows: HashMap<i64, ProcessRow>,
}

/// How one attempt to snapshot the process table ended.
#[derive(Debug)]
pub(crate) enum ProcessTableObservation {
    Complete(ProcessTable),
    /// The host refused to list processes to this caller (typically a
    /// sandboxed command). Retrying within the same command cannot help.
    Denied,
    /// Timed out, truncated, or failed for another reason; a later retry may
    /// succeed.
    Failed,
}

impl ProcessTableObservation {
    pub(crate) fn into_table(self) -> Option<ProcessTable> {
        match self {
            Self::Complete(table) => Some(table),
            Self::Denied | Self::Failed => None,
        }
    }
}

impl ProcessTable {
    /// Snapshot the host's process table, or `None` when the complete
    /// observation is unavailable. Never parse a timed-out or truncated prefix.
    pub fn snapshot() -> Option<Self> {
        Self::observe().into_table()
    }

    /// Snapshot the host's process table. On macOS the table is listed
    /// natively, without spawning a child a sandboxed caller may be refused;
    /// `ps` remains the fallback there and the only source elsewhere.
    pub(crate) fn observe() -> ProcessTableObservation {
        #[cfg(target_os = "macos")]
        let native_denied = match native_process_table::snapshot() {
            Ok(table) => {
                crate::phase_telemetry::add("holder_process_table_native", Duration::ZERO);
                return ProcessTableObservation::Complete(table);
            }
            Err(native_process_table::Failure::Denied) => {
                crate::phase_telemetry::add("holder_process_table_native_denied", Duration::ZERO);
                true
            }
            Err(native_process_table::Failure::Failed) => {
                crate::phase_telemetry::add("holder_process_table_native_fallback", Duration::ZERO);
                false
            }
        };
        #[cfg(not(target_os = "macos"))]
        let native_denied = false;
        let mut command = Command::new(crate::system_programs::ps());
        command.args(["-A", "-o", "pid=,ppid=,lstart=,args="]);
        match Self::observe_with_budget(
            &mut command,
            PROCESS_TABLE_TIMEOUT,
            PROCESS_TABLE_STREAM_LIMIT,
        ) {
            // Both sources refused, or `ps` alone was refused: no retry in
            // this command will see the table.
            ProcessTableObservation::Failed if native_denied => ProcessTableObservation::Denied,
            observation => observation,
        }
    }

    #[cfg(test)]
    fn snapshot_with_budget(
        command: &mut Command,
        timeout: Duration,
        stream_limit: usize,
    ) -> Option<Self> {
        Self::observe_with_budget(command, timeout, stream_limit).into_table()
    }

    fn observe_with_budget(
        command: &mut Command,
        timeout: Duration,
        stream_limit: usize,
    ) -> ProcessTableObservation {
        let output = match crate::bounded_output::output_limited(command, timeout, stream_limit) {
            Ok(crate::bounded_output::LimitedOutput::Completed(output)) => output,
            Ok(_) => return ProcessTableObservation::Failed,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return ProcessTableObservation::Denied;
            }
            Err(_) => return ProcessTableObservation::Failed,
        };
        if output.status.success() {
            return ProcessTableObservation::Complete(Self::parse(&String::from_utf8_lossy(
                &output.stdout,
            )));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Operation not permitted") || stderr.contains("Permission denied") {
            ProcessTableObservation::Denied
        } else {
            ProcessTableObservation::Failed
        }
    }

    /// Parse `ps -o pid=,ppid=,lstart=,args=` output. `lstart` is five
    /// whitespace-separated fields (`Mon Oct  5 21:56:54 2026`).
    pub fn parse(text: &str) -> Self {
        let mut rows = HashMap::new();
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 7 {
                continue;
            }
            let (Ok(pid), Ok(ppid)) = (fields[0].parse::<i64>(), fields[1].parse::<i64>()) else {
                continue;
            };
            rows.insert(
                pid,
                ProcessRow {
                    ppid,
                    started: fields[2..7].join(" "),
                    args: std::sync::OnceLock::from(fields[7..].join(" ")),
                    comm: String::new(),
                },
            );
        }
        Self { rows }
    }

    /// The process `pid`, if it is in the table.
    pub fn process(&self, pid: i64) -> Option<AgentProcess> {
        self.rows.get(&pid).map(|row| AgentProcess {
            pid,
            started: row.started.clone(),
            command: row.args(pid).to_owned(),
        })
    }

    /// Whether `process` is still running: same pid and same start time.
    pub fn is_alive(&self, process: &AgentProcess) -> bool {
        self.rows
            .get(&process.pid)
            .is_some_and(|row| row.started == process.started)
    }

    /// Every process that is itself an agent runtime.
    pub fn agent_pids(&self) -> Vec<i64> {
        let mut pids: Vec<i64> = self
            .rows
            .iter()
            .filter(|(pid, row)| is_agent_runtime(row.args(**pid)))
            .map(|(pid, _)| *pid)
            .collect();
        pids.sort_unstable();
        pids
    }

    /// The agent runtime `pid` runs under: its nearest ancestor (itself
    /// included) that is an agent runtime, extended upward while that
    /// process's direct parent is one too. One agent can be several such
    /// processes -- Codex runs as a `node .../codex` wrapper around a native
    /// `codex-*` child -- and they must not read as two agents. Only a direct
    /// chain merges: a `codex` started from a `claude` shell is its own agent.
    pub fn agent_root(&self, pid: i64) -> Option<AgentProcess> {
        let mut current = pid;
        // A cycle cannot occur in a real table; the bounds keep a malformed
        // snapshot from looping.
        let mut found = false;
        for _ in 0..64 {
            let row = self.rows.get(&current)?;
            if is_agent_runtime(row.args(current)) {
                found = true;
                break;
            }
            if row.ppid <= 1 || row.ppid == current {
                return None;
            }
            current = row.ppid;
        }
        if !found {
            return None;
        }
        for _ in 0..64 {
            let ppid = self.rows.get(&current)?.ppid;
            match self.rows.get(&ppid) {
                Some(parent) if ppid != current && is_agent_runtime(parent.args(ppid)) => {
                    current = ppid;
                }
                _ => break,
            }
        }
        self.process(current)
    }

    /// The caller's process ancestry up to its agent runtime root. Full command
    /// lines stay in memory only; callers that persist this chain must redact
    /// each row with `operation_agent_identity`.
    pub fn lineage_to_agent_root(&self, pid: i64) -> Option<Vec<AgentProcess>> {
        let root = self.agent_root(pid)?;
        let mut lineage = Vec::new();
        let mut current = pid;
        for _ in 0..64 {
            lineage.push(self.process(current)?);
            if current == root.pid {
                return Some(lineage);
            }
            let parent = self.rows.get(&current)?.ppid;
            if parent <= 1 || parent == current {
                return None;
            }
            current = parent;
        }
        None
    }
}

/// Whether a command line is an agent runtime: its executable is `claude` or
/// `codex` (including Codex's native `codex-*` binary), or it is `node`
/// running one of them.
/// A shared app-server transports many independent tasks. Its process lifetime
/// cannot establish that any one broker session is still being worked on.
pub(crate) fn is_shared_agent_service(args: &str) -> bool {
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let name = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
    let executable = if name(first) == "node" {
        words.next().unwrap_or("")
    } else {
        first
    };
    let executable = name(executable);
    (executable == "codex" || executable.starts_with("codex-"))
        && words.next() == Some("app-server")
}

fn is_agent_runtime(args: &str) -> bool {
    if is_shared_agent_service(args) {
        return false;
    }
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let is_agent = |word: &str| {
        let name = word.rsplit('/').next().unwrap_or(word);
        name == "claude" || name == "codex" || name.starts_with("codex-")
    };
    if is_agent(first) {
        return true;
    }
    let first_name = first.rsplit('/').next().unwrap_or(first);
    first_name == "node" && words.next().is_some_and(is_agent)
}

const LSTART_WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const LSTART_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Epoch milliseconds of a C-locale `ps -o lstart` start text
/// (`Mon Oct 5 21:56:54 2026`), read as local time the way `ps` printed it.
/// `None` for anything else, a localized text included: a start the broker
/// cannot read exactly is never compared with a clock.
pub(crate) fn lstart_epoch_ms(started: &str) -> Option<i64> {
    let fields: Vec<&str> = started.split_whitespace().collect();
    let [weekday, month, day, time, year] = fields.as_slice() else {
        return None;
    };
    let weekday = LSTART_WEEKDAYS.iter().position(|name| name == weekday)?;
    let month = LSTART_MONTHS.iter().position(|name| name == month)?;
    let day = day
        .parse::<i32>()
        .ok()
        .filter(|day| (1..=31).contains(day))?;
    let year = year
        .parse::<i32>()
        .ok()
        .filter(|year| (1970..=9999).contains(year))?;
    let mut clock = time.split(':').map(|part| part.parse::<i32>().ok());
    let (Some(Some(hour)), Some(Some(minute)), Some(Some(second)), None) =
        (clock.next(), clock.next(), clock.next(), clock.next())
    else {
        return None;
    };
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    local_epoch_seconds(year, month, day, hour, minute, second, weekday)?.checked_mul(1_000)
}

/// The epoch seconds of a local calendar time, or `None` when it does not
/// name a real day (a normalized date, or the weekday disagrees).
#[cfg(unix)]
// `time_t` is already `i64` on some targets only.
#[allow(clippy::useless_conversion)]
fn local_epoch_seconds(
    year: i32,
    month: usize,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    weekday: usize,
) -> Option<i64> {
    // SAFETY: `libc::tm` is plain old data; an all-zero value (including a
    // null `tm_zone` on platforms that have it) is a valid input to `mktime`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as libc::c_int;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a live, writable, initialized `libc::tm`; `mktime`
    // reads and normalizes it in place.
    let seconds = unsafe { libc::mktime(&mut tm) };
    if seconds == -1 {
        return None;
    }
    (tm.tm_mday == day && tm.tm_mon == month as libc::c_int && tm.tm_wday == weekday as libc::c_int)
        .then_some(i64::from(seconds))
}

#[cfg(not(unix))]
fn local_epoch_seconds(
    _year: i32,
    _month: usize,
    _day: i32,
    _hour: i32,
    _minute: i32,
    _second: i32,
    _weekday: usize,
) -> Option<i64> {
    None
}

/// A process's command line, read natively for a row a native listing
/// produced; `(name)` like `ps` when it cannot be read.
#[cfg(target_os = "macos")]
fn native_process_args(pid: i64, comm: &str) -> String {
    native_process_table::args(pid).unwrap_or_else(|| format!("({comm})"))
}

/// Rows parsed from `ps` carry their command line; nothing else creates rows.
#[cfg(not(target_os = "macos"))]
fn native_process_args(_pid: i64, _comm: &str) -> String {
    String::new()
}

/// The process table listed with libproc instead of a `ps` child: a sandboxed
/// caller may be refused the spawn, and each refusal used to leave a whole
/// retirement pass judging every holder unknown.
#[cfg(target_os = "macos")]
mod native_process_table {
    use std::collections::HashMap;

    pub(super) enum Failure {
        /// The kernel refused the listing to this caller.
        Denied,
        Failed,
    }

    fn failure_from_errno() -> Failure {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) | Some(libc::EACCES) => Failure::Denied,
            _ => Failure::Failed,
        }
    }

    pub(super) fn snapshot() -> Result<super::ProcessTable, Failure> {
        let pids = list_pids()?;
        let locale = StartTimeLocale::from_environment();
        let mut rows = HashMap::with_capacity(pids.len());
        for pid in pids {
            // A process may exit between the listing and this read; it is
            // simply not in the table, as it would not be in `ps` output.
            let Some(info) = bsd_info(pid) else {
                continue;
            };
            let Some(started) = locale.format(info.pbi_start_tvsec) else {
                continue;
            };
            let comm = info
                .pbi_comm
                .iter()
                .take_while(|byte| **byte != 0)
                .map(|byte| *byte as u8 as char)
                .collect::<String>();
            rows.insert(
                i64::from(pid),
                super::ProcessRow {
                    ppid: i64::from(info.pbi_ppid),
                    started,
                    args: std::sync::OnceLock::new(),
                    comm,
                },
            );
        }
        // The caller must at least see itself; anything less is not a
        // complete observation.
        if !rows.contains_key(&i64::from(std::process::id())) {
            return Err(Failure::Failed);
        }
        Ok(super::ProcessTable { rows })
    }

    fn list_pids() -> Result<Vec<libc::pid_t>, Failure> {
        // SAFETY: a null buffer asks only for the current number of pids.
        let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if estimate <= 0 {
            return Err(failure_from_errno());
        }
        let mut capacity = estimate as usize + 256;
        for _ in 0..3 {
            let mut pids: Vec<libc::pid_t> = vec![0; capacity];
            let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>())
                .map_err(|_| Failure::Failed)?;
            // SAFETY: `pids` owns `bytes` writable bytes of `pid_t` storage.
            let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            if count <= 0 {
                return Err(failure_from_errno());
            }
            let count = count as usize;
            if count < capacity {
                pids.truncate(count);
                return Ok(pids);
            }
            // A full buffer may have been truncated: list again, larger.
            capacity = capacity.saturating_mul(2);
        }
        Err(Failure::Failed)
    }

    fn bsd_info(pid: libc::pid_t) -> Option<libc::proc_bsdinfo> {
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        // SAFETY: `proc_bsdinfo` is plain old data; zero is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is writable storage of exactly `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size as libc::c_int,
            )
        };
        (written > 0 && written as usize == size).then_some(info)
    }

    /// `ps` prints `lstart` with `strftime("%c")` under the caller's locale;
    /// this formats the same start the same way so recorded holders match.
    struct StartTimeLocale {
        locale: libc::locale_t,
    }

    impl StartTimeLocale {
        fn from_environment() -> Self {
            // SAFETY: both names are NUL-terminated; a null base asks for a
            // new locale object, which `Drop` frees.
            let mut locale =
                unsafe { libc::newlocale(libc::LC_ALL_MASK, c"".as_ptr(), std::ptr::null_mut()) };
            if locale.is_null() {
                // `ps` keeps the C locale when the environment names an
                // unusable one.
                // SAFETY: as above.
                locale = unsafe {
                    libc::newlocale(libc::LC_ALL_MASK, c"C".as_ptr(), std::ptr::null_mut())
                };
            }
            Self { locale }
        }

        fn format(&self, start_seconds: u64) -> Option<String> {
            if self.locale.is_null() {
                return None;
            }
            let seconds = libc::time_t::try_from(start_seconds).ok()?;
            // SAFETY: plain old data; `localtime_r` fills it before it is read.
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            // SAFETY: both pointers reference live, correctly aligned values.
            if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
                return None;
            }
            let mut buffer = [0 as libc::c_char; 128];
            // SAFETY: the buffer, format and `tm` are live for the call, the
            // format is NUL-terminated, and `self.locale` is a valid locale.
            let written = unsafe {
                libc::strftime_l(
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    c"%c".as_ptr(),
                    &tm,
                    self.locale,
                )
            };
            if written == 0 {
                return None;
            }
            let bytes = buffer[..written]
                .iter()
                .map(|byte| *byte as u8)
                .collect::<Vec<u8>>();
            // `ProcessTable::parse` rejoins `ps` fields with single spaces.
            Some(
                String::from_utf8_lossy(&bytes)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
    }

    impl Drop for StartTimeLocale {
        fn drop(&mut self) {
            if !self.locale.is_null() {
                // SAFETY: the locale came from `newlocale` and is freed once.
                unsafe {
                    libc::freelocale(self.locale);
                }
            }
        }
    }

    /// The command line of `pid` from `KERN_PROCARGS2`, its argument words
    /// joined by single spaces as `ProcessTable::parse` reads `ps` output.
    pub(super) fn args(pid: i64) -> Option<String> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut argmax: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let mut name = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: `name` holds two MIB entries, and `argmax`/`size` describe
        // writable storage of the size `KERN_ARGMAX` returns.
        let result = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                2,
                (&mut argmax as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || argmax <= 0 {
            return None;
        }
        let mut buffer = vec![0u8; argmax as usize];
        let mut size = buffer.len();
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: `name` holds three MIB entries and `buffer` owns `size`
        // writable bytes; the kernel updates `size` to the bytes written.
        let result = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return None;
        }
        parse_procargs2(buffer.get(..size)?)
    }

    /// `KERN_PROCARGS2`: a native-endian `argc`, the executable path, NUL
    /// padding, then `argc` NUL-terminated arguments (then the environment,
    /// which is never read).
    pub(super) fn parse_procargs2(buffer: &[u8]) -> Option<String> {
        let argc_bytes = buffer.get(..std::mem::size_of::<libc::c_int>())?;
        let argc = libc::c_int::from_ne_bytes(argc_bytes.try_into().ok()?);
        let argc = usize::try_from(argc).ok().filter(|argc| *argc > 0)?;
        let rest = &buffer[std::mem::size_of::<libc::c_int>()..];
        let path_end = rest.iter().position(|byte| *byte == 0)?;
        let mut cursor = path_end;
        while rest.get(cursor) == Some(&0) {
            cursor += 1;
        }
        let mut words = Vec::new();
        for argument in rest.get(cursor..)?.split(|byte| *byte == 0).take(argc) {
            words.extend(
                String::from_utf8_lossy(argument)
                    .split_whitespace()
                    .map(str::to_owned),
            );
        }
        (!words.is_empty()).then(|| words.join(" "))
    }
}

/// Identify the caller of this broker command.
pub fn caller() -> Caller {
    let Some(table) = ProcessTable::snapshot() else {
        return Caller::Unidentified;
    };
    caller_in(&table, std::env::var(AGENT_PID_ENV).ok().as_deref())
}

/// [`caller`] against a given table and override, for tests.
pub fn caller_in(table: &ProcessTable, agent_pid_override: Option<&str>) -> Caller {
    let identified = match agent_pid_override.map(str::trim) {
        Some("" | "0" | "none") => None,
        Some(value) => value.parse().ok().and_then(|pid| table.process(pid)),
        None => table.agent_root(std::process::id() as i64),
    };
    identified
        .filter(|agent| !is_shared_agent_service(&agent.command))
        .map_or(Caller::Unidentified, Caller::Agent)
}

/// Why a holder was bound, as recorded in the `session.holder_bound` payload.
pub mod reason {
    /// `start` or `start --adopt` created the session from this agent.
    pub const REGISTERED: &str = "registered";
    /// The session had no recorded holder; its first identified caller binds.
    pub const FIRST_USE: &str = "first_use";
    /// The recorded holder is no longer running.
    pub const HOLDER_GONE: &str = "holder_gone";
    /// `--take-over` moved the session from a live holder.
    pub const TAKE_OVER: &str = "take_over";
}

/// What a holder check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderCheck {
    /// The caller held the session already.
    Held,
    /// The caller now holds it; the payload's `reason` says why.
    Bound { reason: &'static str },
    /// The caller is not an identifiable agent and was let through unbound.
    Unidentified,
    /// The latest holder event is malformed or uses an unknown task-lease
    /// protocol. The command remains admissible under the existing policy,
    /// but this event is never replaced by an age-expiring binding.
    UnknownBinding,
}

/// Whether a PID-less session's recorded task holder is still the same
/// process. Missing and shared-service holders provide no task-specific
/// liveness evidence; incomplete observations remain unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordedHolderLiveness {
    Unbound,
    Alive,
    Gone,
    Unknown,
}

#[cfg(test)]
thread_local! {
    /// The open-file observation retirement passes on this test thread use
    /// instead of running `lsof`.
    static TEST_PROCESS_USE_SNAPSHOT: std::cell::RefCell<Option<crate::reclaim::OpenFileSnapshot>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Observations taken, in order, before falling back to the fixed one.
    static TEST_PROCESS_USE_QUEUE: std::cell::RefCell<std::collections::VecDeque<crate::reclaim::OpenFileSnapshot>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    /// Treat every held observation as aged and unthrottled, so the next
    /// request takes a new one.
    static TEST_FORCE_PROCESS_USE_RESNAPSHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Queue observations retirement passes on this test thread take in order,
/// and whether a held observation is always re-taken.
#[cfg(test)]
pub(crate) fn queue_process_use_snapshots_for_tests(
    snapshots: Vec<crate::reclaim::OpenFileSnapshot>,
    force_resnapshot: bool,
) {
    TEST_PROCESS_USE_QUEUE.with(|queue| *queue.borrow_mut() = snapshots.into());
    TEST_FORCE_PROCESS_USE_RESNAPSHOT.with(|force| force.set(force_resnapshot));
}

/// Make retirement passes on this test thread observe `snapshot` instead of
/// running `lsof`; `None` restores the real observation.
#[cfg(test)]
pub(crate) fn set_process_use_snapshot_for_tests(
    snapshot: Option<crate::reclaim::OpenFileSnapshot>,
) {
    TEST_PROCESS_USE_SNAPSHOT.with(|cell| *cell.borrow_mut() = snapshot);
}

/// The process snapshot is valid only for the holder event it observed.
/// Holder events are append-only identities, so a rebind forces a fresh
/// snapshot instead of reusing a process table captured for another owner.
#[derive(Default)]
pub(crate) struct RecordedHolderProcessTable {
    prefetched_event_ids: HashMap<i64, i64>,
    observed_event_ids: HashMap<i64, i64>,
    snapshot: Option<Option<ProcessTable>>,
    /// A failed process-table snapshot (`ps` timed out or failed) is retried
    /// at once, then after [`PROCESS_TABLE_RETRY_INTERVAL`], at most
    /// [`PROCESS_TABLE_MAX_RETRIES_PER_PASS`] times per pass, instead of
    /// answering "unknown" for every later holder.
    snapshot_retries: usize,
    snapshot_failed_at: Option<Instant>,
    /// The host refused this command the process listing (sandboxed); no
    /// snapshot is retried for the rest of the pass.
    process_listing_denied: bool,
    /// Why the last automatic close judged with this table kept its session
    /// (`AbandonmentKeepReason`); the table is the one per-pass context every
    /// exit of the close path can reach.
    keep_reason: std::cell::Cell<Option<crate::broker::AbandonmentKeepReason>>,
    /// When the last session kept for recent activity can first be due: its
    /// newest activity plus the window it was judged against.
    not_due_before_ms: std::cell::Cell<Option<i64>>,
    process_use_snapshot: Option<(Instant, crate::reclaim::OpenFileSnapshot)>,
    /// Open-file observations this pass took, when the last one started, and
    /// whether the last request was refused by the per-pass throttle.
    process_use_snapshots_taken: usize,
    process_use_last_taken: Option<Instant>,
    process_use_throttled: bool,
    #[cfg(test)]
    snapshots_taken: usize,
    /// The next process-table snapshot, instead of running `ps`.
    #[cfg(test)]
    next_process_table_for_test: Option<Option<ProcessTable>>,
    /// The next process-table snapshot is refused, as in a sandbox.
    #[cfg(test)]
    next_process_table_denied_for_test: bool,
    /// Runs immediately before the landed lane's last HEAD and activity
    /// re-read, so a test can interleave an agent's commit there.
    #[cfg(test)]
    before_landed_close: Option<Box<dyn FnMut()>>,
}

impl RecordedHolderProcessTable {
    /// Prime the bounded session IDs before the single process-table
    /// snapshot. A fresh holder event discovered later is checked with a new
    /// snapshot, never borrowed from a different binding.
    pub(crate) fn prime(
        &mut self,
        store: &crate::BrokerStore,
        session_ids: &[i64],
        deadline: Option<Instant>,
    ) {
        for session_id in session_ids {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            if let Ok(Some(event)) = store.latest_session_holder_event(*session_id) {
                self.prefetched_event_ids.insert(*session_id, event.id);
            }
        }
    }

    fn get_or_snapshot(
        &mut self,
        session_id: i64,
        event_id: i64,
        allow_snapshot: bool,
    ) -> Option<&ProcessTable> {
        if matches!(self.snapshot, Some(None)) && allow_snapshot && self.process_table_retry_due() {
            crate::phase_telemetry::add("holder_process_table_retried", Duration::ZERO);
            self.snapshot_retries += 1;
            self.snapshot = None;
        }
        if self.snapshot.is_none() {
            if !allow_snapshot {
                return None;
            }
            self.snapshot = Some(self.take_process_table());
            #[cfg(test)]
            {
                self.snapshots_taken += 1;
            }
            self.observed_event_ids.extend(
                self.prefetched_event_ids
                    .iter()
                    .map(|(id, event)| (*id, *event)),
            );
            // The requested binding was read before this first snapshot. Use
            // the snapshot just taken for it instead of immediately taking a
            // second one when `prime` saw an older binding.
            self.observed_event_ids.insert(session_id, event_id);
        } else if self.observed_event_ids.get(&session_id) != Some(&event_id) {
            if !allow_snapshot {
                return None;
            }
            self.snapshot = Some(self.take_process_table());
            #[cfg(test)]
            {
                self.snapshots_taken += 1;
            }
            self.observed_event_ids.insert(session_id, event_id);
        }
        self.snapshot.as_ref().and_then(Option::as_ref)
    }

    /// Take one process-table snapshot, remembering when one fails.
    fn take_process_table(&mut self) -> Option<ProcessTable> {
        #[cfg(test)]
        if std::mem::take(&mut self.next_process_table_denied_for_test) {
            return self.record_observation(ProcessTableObservation::Denied);
        }
        #[cfg(test)]
        if let Some(snapshot) = self.next_process_table_for_test.take() {
            if snapshot.is_none() {
                self.snapshot_failed_at = Some(Instant::now());
            }
            return snapshot;
        }
        self.record_observation(ProcessTable::observe())
    }

    /// Keep a complete observation; remember a failure for the retry
    /// schedule, and a refusal for the rest of the pass.
    fn record_observation(&mut self, observation: ProcessTableObservation) -> Option<ProcessTable> {
        match observation {
            ProcessTableObservation::Complete(table) => Some(table),
            ProcessTableObservation::Denied => {
                // A refusal is a property of this command, not of the host's
                // load: retrying cannot help, and the holders it hides are
                // not judged "unknown" one by one.
                crate::phase_telemetry::add("holder_process_table_denied", Duration::ZERO);
                self.process_listing_denied = true;
                self.snapshot_retries = PROCESS_TABLE_MAX_RETRIES_PER_PASS;
                self.snapshot_failed_at = Some(Instant::now());
                None
            }
            ProcessTableObservation::Failed => {
                crate::phase_telemetry::add("holder_process_table_failed", Duration::ZERO);
                self.snapshot_failed_at = Some(Instant::now());
                None
            }
        }
    }

    /// Whether this command was refused the process listing. Retirement
    /// judgements in such a command are skipped, not counted holder by
    /// holder as unknown.
    pub(crate) fn process_listing_denied(&self) -> bool {
        self.process_listing_denied
    }

    /// Whether a failed process-table snapshot may be taken again now: the
    /// first retry at once, later ones only once
    /// [`PROCESS_TABLE_RETRY_INTERVAL`] has passed since the last failure,
    /// and never more than [`PROCESS_TABLE_MAX_RETRIES_PER_PASS`] per pass.
    fn process_table_retry_due(&self) -> bool {
        self.snapshot_retries < PROCESS_TABLE_MAX_RETRIES_PER_PASS
            && (self.snapshot_retries == 0
                || self
                    .snapshot_failed_at
                    .is_none_or(|failed| failed.elapsed() >= PROCESS_TABLE_RETRY_INTERVAL))
    }

    /// Record why the close path keeps the session it is judging.
    pub(crate) fn note_keep_reason(&self, reason: crate::broker::AbandonmentKeepReason) {
        self.keep_reason.set(Some(reason));
    }

    /// Forget the last keep reason, before judging another session.
    pub(crate) fn clear_keep_reason(&self) {
        self.keep_reason.set(None);
        self.not_due_before_ms.set(None);
    }

    /// Record that the session being judged cannot be due before `at_ms`
    /// (kept for recent activity).
    pub(crate) fn note_not_due_before(&self, at_ms: i64) {
        self.not_due_before_ms.set(Some(at_ms));
    }

    /// When the last session kept for recent activity can first be due.
    pub(crate) fn not_due_before(&self) -> Option<i64> {
        self.not_due_before_ms.get()
    }

    /// Why the last session judged with this table was kept, if it was.
    pub(crate) fn keep_reason(&self) -> Option<crate::broker::AbandonmentKeepReason> {
        self.keep_reason.get()
    }

    /// A final abandonment decision must not reuse an earlier pass's process
    /// observation for a legacy shared-service binding.
    pub(crate) fn refresh_for_binding(
        &mut self,
        session_id: i64,
        event_id: i64,
        allow_snapshot: bool,
    ) -> Option<&ProcessTable> {
        if !allow_snapshot || self.process_listing_denied {
            return None;
        }
        let snapshot = self.record_observation(ProcessTable::observe());
        self.install_refreshed_snapshot(session_id, event_id, snapshot)
    }

    fn install_refreshed_snapshot(
        &mut self,
        session_id: i64,
        event_id: i64,
        snapshot: Option<ProcessTable>,
    ) -> Option<&ProcessTable> {
        self.snapshot = Some(snapshot);
        // This is a new complete host snapshot, so all previously observed
        // bindings remain covered. Preserve the primed cohort too; later
        // callers still compare their current binding ID before reuse.
        for (&id, &binding) in &self.prefetched_event_ids {
            self.observed_event_ids.entry(id).or_insert(binding);
        }
        self.observed_event_ids.insert(session_id, event_id);
        self.snapshot.as_ref().and_then(Option::as_ref)
    }

    pub(crate) fn table_for(&self, session_id: i64, event_id: i64) -> Option<&ProcessTable> {
        (self.observed_event_ids.get(&session_id) == Some(&event_id))
            .then(|| self.snapshot.as_ref().and_then(Option::as_ref))
            .flatten()
    }

    /// Share one complete-or-unknown `lsof` observation across retirement
    /// candidates in this maintenance pass. An observation older than its
    /// two-second freshness bound is taken again, but at most once per
    /// [`PROCESS_USE_RESNAPSHOT_MIN_INTERVAL`] and
    /// [`PROCESS_USE_MAX_SNAPSHOTS_PER_PASS`] times per pass, and only while
    /// [`PROCESS_USE_RESNAPSHOT_MIN_BUDGET`] of the deadline remains: a
    /// system-wide `lsof` every two seconds for a whole pass is the kind of
    /// unthrottled background work that hurts the machine. When no fresh
    /// observation may be taken the answer is `None` and
    /// [`Self::process_use_throttled`] says that the pass, not the evidence,
    /// ran out; the caller retries the session next pass.
    pub(crate) fn process_use_snapshot_for_retirement(
        &mut self,
        root: &Path,
        deadline: Option<Instant>,
    ) -> Option<&crate::reclaim::OpenFileSnapshot> {
        self.process_use_throttled = false;
        let aged = self
            .process_use_snapshot
            .as_ref()
            .is_some_and(|(captured_at, _)| {
                captured_at.elapsed() > TASK_CLOSE_PROCESS_USE_SNAPSHOT_MAX_AGE
            });
        #[cfg(test)]
        let aged = if self.process_use_snapshot.is_some()
            && TEST_FORCE_PROCESS_USE_RESNAPSHOT.with(std::cell::Cell::get)
        {
            self.process_use_last_taken = None;
            true
        } else {
            aged
        };
        if aged {
            let room_for_another = deadline.is_none_or(|deadline| {
                deadline.saturating_duration_since(Instant::now())
                    >= PROCESS_USE_RESNAPSHOT_MIN_BUDGET
            });
            let interval_elapsed = self
                .process_use_last_taken
                .is_none_or(|taken| taken.elapsed() >= PROCESS_USE_RESNAPSHOT_MIN_INTERVAL);
            let under_cap = self.process_use_snapshots_taken < PROCESS_USE_MAX_SNAPSHOTS_PER_PASS;
            if !(room_for_another && interval_elapsed && under_cap) {
                crate::phase_telemetry::add("retirement_process_use_throttled", Duration::ZERO);
                self.process_use_throttled = true;
                return None;
            }
            self.process_use_snapshot = None;
        }
        if self.process_use_snapshot.is_none() {
            #[cfg(test)]
            if let Some(snapshot) = TEST_PROCESS_USE_QUEUE
                .with(|queue| queue.borrow_mut().pop_front())
                .or_else(|| TEST_PROCESS_USE_SNAPSHOT.with(|cell| cell.borrow().clone()))
            {
                self.record_process_use_take(Instant::now(), snapshot, Duration::ZERO);
                let (_, snapshot) = self.process_use_snapshot.as_ref()?;
                return Some(snapshot);
            }
            let budget = deadline.map_or(Duration::from_secs(10), |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if budget.is_zero() {
                return None;
            }
            let started = Instant::now();
            let snapshot = crate::reclaim::open_file_snapshot_under_with_budget(root, budget);
            self.record_process_use_take(started, snapshot, started.elapsed());
        }
        let (captured_at, snapshot) = self.process_use_snapshot.as_ref()?;
        (captured_at.elapsed() <= TASK_CLOSE_PROCESS_USE_SNAPSHOT_MAX_AGE).then_some(snapshot)
    }

    fn record_process_use_take(
        &mut self,
        captured_at: Instant,
        snapshot: crate::reclaim::OpenFileSnapshot,
        elapsed: Duration,
    ) {
        if self.process_use_snapshots_taken > 0 {
            crate::phase_telemetry::add("retirement_process_use_resnapshot", elapsed);
        } else {
            crate::phase_telemetry::add("retirement_process_use_snapshot", elapsed);
        }
        self.process_use_snapshots_taken += 1;
        self.process_use_last_taken = Some(captured_at);
        self.process_use_snapshot = Some((captured_at, snapshot));
    }

    /// Whether the last [`Self::process_use_snapshot_for_retirement`] answered
    /// `None` because this pass may not take another observation now.
    pub(crate) fn process_use_throttled(&self) -> bool {
        self.process_use_throttled
    }

    /// Record a failed process-table snapshot, already retried or not.
    #[cfg(test)]
    pub(crate) fn fail_process_table_for_test(
        &mut self,
        session_id: i64,
        event_id: i64,
        retried: bool,
    ) {
        self.observed_event_ids.insert(session_id, event_id);
        self.snapshot = Some(None);
        self.snapshot_retries = usize::from(retried);
        self.snapshot_failed_at = Some(Instant::now());
    }

    /// Pretend the last process-table failure happened `age` ago.
    #[cfg(test)]
    pub(crate) fn age_process_table_failure_for_test(&mut self, age: Duration) {
        self.snapshot_failed_at = self
            .snapshot_failed_at
            .and_then(|failed| failed.checked_sub(age));
    }

    /// Make the next process-table snapshot this table takes answer
    /// `snapshot` instead of running `ps`.
    #[cfg(test)]
    pub(crate) fn queue_process_table_for_test(&mut self, snapshot: Option<ProcessTable>) {
        self.next_process_table_for_test = Some(snapshot);
    }

    /// Make the next process-table snapshot this table takes be refused, as
    /// it is for a sandboxed caller.
    #[cfg(test)]
    pub(crate) fn queue_denied_process_table_for_test(&mut self) {
        self.next_process_table_denied_for_test = true;
    }

    #[cfg(test)]
    pub(crate) fn snapshots_taken_for_test(&self) -> usize {
        self.snapshots_taken
    }

    #[cfg(test)]
    pub(crate) fn install_process_use_snapshot_for_test(
        &mut self,
        snapshot: crate::reclaim::OpenFileSnapshot,
    ) {
        self.process_use_snapshot = Some((Instant::now(), snapshot));
    }

    #[cfg(test)]
    pub(crate) fn install_before_landed_close_for_test(&mut self, hook: Box<dyn FnMut()>) {
        self.before_landed_close = Some(hook);
    }

    #[cfg(test)]
    pub(crate) fn run_before_landed_close_for_test(&mut self) {
        if let Some(hook) = self.before_landed_close.as_mut() {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn install_holder_process_table_for_test(
        &mut self,
        session_id: i64,
        event_id: i64,
        snapshot: ProcessTable,
    ) {
        self.prefetched_event_ids.insert(session_id, event_id);
        self.observed_event_ids.insert(session_id, event_id);
        self.snapshot = Some(Some(snapshot));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordedHolderAssessment {
    pub binding_event_id: Option<i64>,
    pub liveness: RecordedHolderLiveness,
}

/// Observe the latest recorded holder, lazily sharing one bounded process
/// table snapshot across all sessions in a caller's pass. `allow_snapshot`
/// lets a deadline-bound caller decline a new 2-second process-table read
/// when the pass cannot afford it; that session then remains protected.
pub(crate) fn recorded_holder_liveness(
    store: &crate::BrokerStore,
    session_id: i64,
    process_table: &mut RecordedHolderProcessTable,
    allow_snapshot: bool,
) -> RecordedHolderAssessment {
    let event = match store.latest_session_holder_event(session_id) {
        Ok(Some(event)) => event,
        Ok(None) => {
            return RecordedHolderAssessment {
                binding_event_id: None,
                liveness: RecordedHolderLiveness::Unbound,
            };
        }
        Err(_) => {
            return RecordedHolderAssessment {
                binding_event_id: None,
                liveness: RecordedHolderLiveness::Unknown,
            };
        }
    };
    let binding_event_id = Some(event.id);
    let Some(payload) = event.payload_json.as_deref() else {
        return RecordedHolderAssessment {
            binding_event_id,
            liveness: RecordedHolderLiveness::Unknown,
        };
    };
    let Ok(holder) = serde_json::from_str::<AgentProcess>(payload) else {
        return RecordedHolderAssessment {
            binding_event_id,
            liveness: RecordedHolderLiveness::Unknown,
        };
    };
    if is_shared_agent_service(&holder.command) {
        return RecordedHolderAssessment {
            binding_event_id,
            liveness: RecordedHolderLiveness::Unbound,
        };
    }
    let liveness = match process_table.get_or_snapshot(session_id, event.id, allow_snapshot) {
        Some(table) if table.is_alive(&holder) => RecordedHolderLiveness::Alive,
        Some(_) => RecordedHolderLiveness::Gone,
        None => RecordedHolderLiveness::Unknown,
    };
    RecordedHolderAssessment {
        binding_event_id,
        liveness,
    }
}

/// A compact process identity for local operation history; never the full command line.
fn operation_agent_identity(process: &AgentProcess) -> serde_json::Value {
    let program = process
        .command
        .split_whitespace()
        .next()
        .and_then(|command| Path::new(command).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| is_plain_program_name(name))
        .unwrap_or("unknown")
        .to_string();
    serde_json::json!({
        "pid": process.pid,
        "started": process.started,
        "program": program,
    })
}

/// A process can set its own `argv[0]` to any text, so only a short name made
/// of ordinary program-name characters is persisted; anything else is stored
/// as `unknown` rather than risk keeping a token-shaped string in history.
fn is_plain_program_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// Snapshot local process provenance for one coordinated-operation journal row.
/// Only the executable basename is retained from a process command line.
pub(crate) fn operation_agent_provenance(
    store: &crate::BrokerStore,
    session_id: i64,
) -> Result<serde_json::Value, crate::BrokerError> {
    let session = store.session(session_id)?;
    let process_table = ProcessTable::snapshot();
    let agent_pid_override = std::env::var(AGENT_PID_ENV).ok();
    let caller = process_table
        .as_ref()
        .map(|table| caller_in(table, agent_pid_override.as_deref()))
        .unwrap_or(Caller::Unidentified);
    let caller_process = match caller {
        Caller::Agent(process) => Some(process),
        Caller::Unidentified => None,
    };
    let caller_process_chain = process_table
        .as_ref()
        .and_then(|table| table.lineage_to_agent_root(std::process::id() as i64))
        .filter(|chain| {
            chain.last().is_some_and(|root| {
                caller_process
                    .as_ref()
                    .is_some_and(|caller| caller.pid == root.pid && caller.started == root.started)
            })
        })
        .map(|chain| {
            chain
                .iter()
                .map(operation_agent_identity)
                .collect::<Vec<_>>()
        });
    let holder_event = store.latest_session_holder_event(session_id)?;
    let holder = holder_event
        .as_ref()
        .and_then(|event| event.payload_json.as_deref())
        .and_then(|payload| serde_json::from_str::<AgentProcess>(payload).ok())
        .map(|process| operation_agent_identity(&process));
    Ok(serde_json::json!({
        "schema_version": 1,
        "caller": caller_process.as_ref().map(operation_agent_identity),
        "caller_process_chain": caller_process_chain,
        "holder": holder,
        "holder_binding_event_id": holder_event.map(|event| event.id),
        "session_agent_identity": session.agent_identity,
    }))
}
/// The session's current holder, from its latest holder-bound event.
/// If it never had one, or retention pruned the record, the next
/// identified caller then binds, so a lost record fails open.
pub fn recorded_holder(
    store: &crate::BrokerStore,
    session_id: i64,
) -> Result<Option<AgentProcess>, crate::BrokerOpError> {
    let Some(event) = store.latest_session_holder_event(session_id)? else {
        return Ok(None);
    };
    Ok(event
        .payload_json
        .as_deref()
        .and_then(|payload| serde_json::from_str::<AgentProcess>(payload).ok()))
}

/// Read the explicit lease on one exact holder event. Missing, malformed or
/// unknown-version records are legacy ownership and never acquire age-based
/// close authority.
pub(crate) fn task_lease(event: &crate::Event) -> Option<TaskLease> {
    task_lease_from_payload(event.payload_json.as_deref()?)
}

pub(crate) fn task_lease_from_payload(payload: &str) -> Option<TaskLease> {
    let payload = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let lease = serde_json::from_value::<TaskLease>(payload.get("task_lease")?.clone()).ok()?;
    lease.is_supported().then_some(lease)
}

/// Whether this holder event carries the versioned task-lease extension,
/// including malformed and future-version values. Unsupported values never
/// grant age-expiry authority, but still require child-use witnesses before a
/// gone holder can be retired.
pub(crate) fn task_lease_binding_present(event: &crate::Event) -> bool {
    event
        .payload_json
        .as_deref()
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .is_some_and(|payload| payload.get("task_lease").is_some())
}

/// Whether the task lease from this holder event has expired. Activity newer
/// than the binding timestamp extends its deadline; legacy and malformed
/// payloads return `None` so callers cannot confuse them with expiry.
pub(crate) fn task_lease_expired(
    event: &crate::Event,
    last_activity_at: i64,
    now_ms: i64,
) -> Option<bool> {
    let lease = task_lease(event)?;
    let deadline = event
        .ts
        .max(last_activity_at)
        .saturating_add(lease.duration_ms);
    Some(now_ms >= deadline)
}

/// A current lease binding may be renewed by a guarded command only after the
/// five-minute write interval. Clock rollback can delay a renewal but never
/// shortens the task lease.
pub(crate) fn task_lease_renewal_due(event: &crate::Event, now_ms: i64) -> bool {
    task_lease(event)
        .is_some_and(|_| now_ms.saturating_sub(event.ts) >= TASK_LEASE_RENEWAL_INTERVAL_MS)
}

/// Record `caller` as the session's holder, unconditionally. For `start` and
/// `start --adopt`, which create the session from the calling agent.
pub fn bind(
    store: &mut crate::BrokerStore,
    session_id: i64,
    caller: &Caller,
    why: &'static str,
) -> Result<HolderCheck, crate::BrokerOpError> {
    let Caller::Agent(agent) = caller else {
        return Ok(HolderCheck::Unidentified);
    };
    if is_shared_agent_service(&agent.command) {
        return Ok(HolderCheck::Unidentified);
    }
    let previous = recorded_holder(store, session_id)?;
    if previous.as_ref() == Some(agent) {
        return Ok(HolderCheck::Held);
    }
    record(store, session_id, agent, why, previous.as_ref())?;
    Ok(HolderCheck::Bound { reason: why })
}

/// Admit `caller` to act for the session, or refuse it because another live
/// agent holds it. `take_over` moves the session to the caller instead.
pub fn admit(
    store: &mut crate::BrokerStore,
    session_id: i64,
    caller: &Caller,
    table: &ProcessTable,
    take_over: bool,
) -> Result<HolderCheck, crate::BrokerOpError> {
    let Caller::Agent(agent) = caller else {
        return Ok(HolderCheck::Unidentified);
    };
    if is_shared_agent_service(&agent.command) {
        return Ok(HolderCheck::Unidentified);
    }
    let latest = store.latest_session_holder_event(session_id)?;
    let holder = match latest.as_ref() {
        None => None,
        Some(event) => {
            let Some(payload_text) = event.payload_json.as_deref() else {
                return Ok(HolderCheck::UnknownBinding);
            };
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload_text) else {
                return Ok(HolderCheck::UnknownBinding);
            };
            let Ok(holder) = serde_json::from_value::<AgentProcess>(payload.clone()) else {
                return Ok(HolderCheck::UnknownBinding);
            };
            // An unsupported task-lease extension is legacy ownership, not
            // authority to replace the process identity check. Keep the
            // holder's existing process-lifetime protection.
            Some(holder)
        }
    };
    let Some(holder) = holder else {
        record(store, session_id, agent, reason::FIRST_USE, None)?;
        return Ok(HolderCheck::Bound {
            reason: reason::FIRST_USE,
        });
    };
    if &holder == agent {
        return Ok(HolderCheck::Held);
    }
    // Command class does not override process identity: a shared app-server
    // that is still the recorded process requires explicit takeover, while
    // one whose PID/start pair is gone may be replaced like any dead holder.
    let why = if !table.is_alive(&holder) {
        reason::HOLDER_GONE
    } else if take_over {
        reason::TAKE_OVER
    } else {
        let session = store.session(session_id)?;
        let mut holder_context = String::new();
        if let Some(identity) = session.agent_identity.as_deref() {
            holder_context.push_str(&format!(", agent {identity}"));
        }
        if let Some(tab) = session.tab_name.as_deref() {
            holder_context.push_str(&format!(", tab {tab:?}"));
        }
        return Err(crate::BrokerOpError::SessionHeldByAnotherAgent {
            session_id,
            holder_pid: holder.pid,
            holder_command: abbreviate(&holder.command),
            holder_context,
        });
    };
    record(store, session_id, agent, why, Some(&holder))?;
    Ok(HolderCheck::Bound { reason: why })
}

fn record(
    store: &mut crate::BrokerStore,
    session_id: i64,
    holder: &AgentProcess,
    why: &str,
    previous: Option<&AgentProcess>,
) -> Result<(), crate::BrokerOpError> {
    store.append_event(
        crate::events::SESSION_HOLDER_BOUND,
        Some(session_id),
        Some(&crate::events::session_holder_bound_payload(
            holder, why, previous,
        )),
    )?;
    Ok(())
}

/// A command line short enough for a refusal message.
fn abbreviate(command: &str) -> String {
    const LIMIT: usize = 80;
    if command.chars().count() <= LIMIT {
        return command.to_string();
    }
    let kept: String = command.chars().take(LIMIT - 1).collect();
    format!("{kept}…")
}

/// How long `status` may spend asking `lsof` for agent working directories.
/// Past the budget the check is reported as deferred rather than slowing
/// every status call.
pub const FOREIGN_SCAN_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Agent processes working in a session's worktree that do not hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignAgents {
    pub session_id: i64,
    pub holder: Option<AgentProcess>,
    pub foreign: Vec<AgentProcess>,
}

/// The working directories of `pids`, from one bounded `lsof`, or `None` when
/// `lsof` is missing, fails or overruns `budget`. Only agent runtimes are
/// asked about: a system-wide listing took over 800 ms on a busy host, while
/// the co-tenant #393 describes was an agent launched inside the worktree.
pub(crate) fn working_directories(
    pids: &[i64],
    budget: std::time::Duration,
) -> Option<Vec<(i64, String)>> {
    if pids.is_empty() {
        return Some(Vec::new());
    }
    let pid_list = pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // `lsof` lives in /usr/sbin, which a non-login PATH often omits.
    let program = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .unwrap_or("lsof");
    let mut command = Command::new(program);
    command.args(["-a", "-d", "cwd", "-Fpn", "-w", "-p", &pid_list]);
    let output = crate::bounded_output::output_within(&mut command, budget)
        .ok()
        .flatten()?;
    // lsof exits 1 when some process could not be inspected; its output is
    // still the complete list of those it could.
    let text = String::from_utf8_lossy(&output.stdout);
    let mut cwds = Vec::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let (Some(value), Some(pid)) = (line.strip_prefix('n'), pid) {
            cwds.push((pid, value.to_string()));
        }
    }
    Some(cwds)
}

/// The agent processes, other than its holder, whose working directory is
/// inside a session's worktree. `sessions` pairs each live session with its
/// worktree path and recorded holder. A process that is not an agent runtime
/// and has none as an ancestor -- an editor, the operator's own shell -- is not
/// a co-tenant. With no recorded holder, two or more agents are.
pub fn find_foreign(
    table: &ProcessTable,
    cwds: &[(i64, String)],
    sessions: &[(i64, String, Option<AgentProcess>)],
) -> Vec<ForeignAgents> {
    let roots: Vec<(i64, std::path::PathBuf, &Option<AgentProcess>)> = sessions
        .iter()
        .filter(|(_, worktree, _)| !worktree.is_empty())
        .map(|(id, worktree, holder)| {
            let path = std::path::Path::new(worktree);
            (
                *id,
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
                holder,
            )
        })
        .collect();
    let mut found: std::collections::BTreeMap<i64, Vec<AgentProcess>> = Default::default();
    for (pid, cwd) in cwds {
        let cwd = std::path::Path::new(cwd);
        let Some((session_id, _, _)) = roots.iter().find(|(_, root, _)| cwd.starts_with(root))
        else {
            continue;
        };
        let Some(agent) = table.agent_root(*pid) else {
            continue;
        };
        let agents = found.entry(*session_id).or_default();
        if !agents.contains(&agent) {
            agents.push(agent);
        }
    }
    let mut report = Vec::new();
    for (session_id, _, holder) in &roots {
        let Some(agents) = found.remove(session_id) else {
            continue;
        };
        let foreign: Vec<AgentProcess> = match holder {
            Some(holder) => agents.into_iter().filter(|agent| agent != holder).collect(),
            None if agents.len() > 1 => agents,
            None => Vec::new(),
        };
        if !foreign.is_empty() {
            report.push(ForeignAgents {
                session_id: *session_id,
                holder: (*holder).clone(),
                foreign,
            });
        }
    }
    report
}

/// `session.foreign-process` advice for the given live sessions, or `None`
/// when the working-directory scan could not finish within its budget.
pub(crate) fn foreign_process_advice(
    store: &crate::BrokerStore,
    sessions: &[(i64, String)],
) -> Option<Vec<crate::StatusAdvice>> {
    if sessions.is_empty() {
        return Some(Vec::new());
    }
    let table = ProcessTable::snapshot()?;
    let cwds = working_directories(&table.agent_pids(), FOREIGN_SCAN_BUDGET)?;
    let sessions: Vec<(i64, String, Option<AgentProcess>)> = sessions
        .iter()
        .map(|(id, worktree)| {
            let holder = recorded_holder(store, *id).ok().flatten();
            (*id, worktree.clone(), holder)
        })
        .collect();
    Some(
        find_foreign(&table, &cwds, &sessions)
            .into_iter()
            .map(|found| {
                let others = found
                    .foreign
                    .iter()
                    .map(|agent| format!("pid {} ({})", agent.pid, abbreviate(&agent.command)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let held_by = found.holder.as_ref().map_or_else(
                    || "no recorded holder".to_string(),
                    |holder| format!("held by pid {}", holder.pid),
                );
                crate::StatusAdvice {
                    id: "session.foreign-process",
                    severity: crate::StatusAdviceSeverity::Warning,
                    reason: "an agent process that does not hold this session is working in its worktree",
                    summary: format!(
                        "session {}'s worktree is in use by {others}, which does not hold it \
                         ({held_by}); two agents driving one session can overwrite each \
                         other's pushes",
                        found.session_id
                    ),
                    session_id: Some(found.session_id),
                    queue_entry_id: None,
                    evidence: found
                        .foreign
                        .iter()
                        .map(|agent| format!("pid {} started {}", agent.pid, agent.started))
                        .collect(),
                    commands: Vec::new(),
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
    1     0 Fri Oct  2 15:19:49 2026     /sbin/launchd
 1160     1 Fri Oct  2 15:22:48 2026     /Applications/Chau7.app/Contents/MacOS/Chau7
 1297  1160 Fri Oct  2 15:22:54 2026     /bin/zsh
 1977  1297 Fri Oct  2 15:22:59 2026     claude --resume 621c2bfc
88836  1977 Mon Oct  5 21:56:54 2026     /bin/zsh -c source snapshot
88900 88836 Mon Oct  5 21:56:55 2026     aethyme broker submit --session 7
 2000     1 Fri Oct  2 15:30:00 2026     node /usr/local/bin/codex -c x
 2023  2000 Fri Oct  2 15:30:01 2026     /opt/codex/vendor/codex-darwin-arm64 exec
 2100  2023 Fri Oct  2 15:31:00 2026     /bin/bash -lc git status
 3000     1 Fri Oct  2 15:40:00 2026     /usr/bin/nohup zsh queue.sh
";

    #[test]
    fn finite_task_lease_is_versioned_and_uses_newer_activity_as_its_clock() {
        let holder = AgentProcess {
            pid: 3731,
            started: "Fri Oct 23 08:42:41 2026".into(),
            command: "/opt/codex exec task".into(),
        };
        let payload = crate::events::session_holder_task_lease_bound_payload(
            &holder,
            "task_lease_enabled",
            None,
        );
        let event = crate::Event {
            id: 1,
            schema_version: 1,
            ts: 10_000,
            kind: crate::events::SESSION_HOLDER_BOUND.into(),
            session_id: Some(7),
            payload_json: Some(payload),
        };
        let event_timestamp_deadline = 10_000 + TASK_LEASE_DURATION_MS;
        assert_eq!(task_lease(&event), Some(TaskLease::current()));
        assert_eq!(
            task_lease_expired(&event, 1_000, event_timestamp_deadline - 1),
            Some(false),
            "a newer holder event timestamp extends the deadline"
        );
        assert_eq!(
            task_lease_expired(&event, 1_000, event_timestamp_deadline),
            Some(true)
        );
        let activity_newer_event = crate::Event {
            ts: 1_000,
            ..event.clone()
        };
        let activity_deadline = 10_000 + TASK_LEASE_DURATION_MS;
        assert_eq!(
            task_lease_expired(&activity_newer_event, 10_000, activity_deadline - 1),
            Some(false),
            "newer session activity also extends the deadline"
        );
        assert_eq!(
            task_lease_expired(&activity_newer_event, 10_000, activity_deadline),
            Some(true)
        );
        assert!(!task_lease_renewal_due(
            &event,
            event.ts + TASK_LEASE_RENEWAL_INTERVAL_MS - 1
        ));
        assert!(task_lease_renewal_due(
            &event,
            event.ts + TASK_LEASE_RENEWAL_INTERVAL_MS
        ));

        let legacy = crate::events::session_holder_bound_payload(&holder, "registered", None);
        assert_eq!(task_lease_from_payload(&legacy), None);
        for malformed in [
            r#"{"pid":1,"started":"s","command":"c","task_lease":{"version":2,"duration_ms":86400000}}"#,
            r#"{"pid":1,"started":"s","command":"c","task_lease":{"version":1,"duration_ms":86400000,"future":true}}"#,
            r#"{"pid":1,"started":"s","command":"c","task_lease":null}"#,
        ] {
            assert_eq!(task_lease_from_payload(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn malformed_lease_extension_does_not_discard_live_holder_protection() {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&temporary.path().join("broker.db")).unwrap();
        let session_id = register_holder_test_session(&mut store);
        let table = ProcessTable::parse(
            "3731 1 Fri Oct 23 08:42:41 2026 /opt/codex agent --session 7\n\
             5000 1 Fri Oct 23 09:01:00 2026 /opt/codex agent --session 8\n",
        );
        let holder = table.process(3731).unwrap();
        let caller = Caller::Agent(table.process(5000).unwrap());
        let mut payload: serde_json::Value = serde_json::from_str(
            &crate::events::session_holder_bound_payload(&holder, "registered", None),
        )
        .unwrap();
        payload["task_lease"] = serde_json::json!({"version": 9, "duration_ms": 86_400_000});
        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session_id),
                Some(&payload.to_string()),
            )
            .unwrap();

        assert!(matches!(
            admit(&mut store, session_id, &caller, &table, false),
            Err(crate::BrokerOpError::SessionHeldByAnotherAgent {
                holder_pid: 3731,
                ..
            })
        ));
        assert_eq!(recorded_holder(&store, session_id).unwrap(), Some(holder));
    }

    #[cfg(unix)]
    fn snapshot_command(stdout: &str, stderr: &str, status: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s' \"$1\"; printf '%s' \"$2\" >&2; exit \"$3\"",
            "process-fixture",
            stdout,
            stderr,
            status,
        ]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn process_snapshot_requires_complete_success_within_each_stream_budget() {
        let observe = |stdout: &str, stderr: &str, status: &str| {
            ProcessTable::snapshot_with_budget(
                &mut snapshot_command(stdout, stderr, status),
                Duration::from_secs(2),
                TABLE.len(),
            )
        };
        let complete = observe(TABLE, "", "0").expect("exact-cap complete snapshot");
        assert_eq!(
            complete.process(1977).unwrap().command,
            "claude --resume 621c2bfc"
        );
        assert!(complete.is_alive(&complete.process(1977).unwrap()));
        assert!(observe(&format!("{TABLE} "), "", "0").is_none());
        assert!(observe(TABLE, &"x".repeat(TABLE.len() + 1), "0").is_none());
        assert!(observe(TABLE, "", "7").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn process_snapshot_deadline_and_spawn_failure_are_unavailable() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s' \"$1\"; exec /bin/sleep 5",
            "process-fixture",
            TABLE,
        ]);
        assert!(
            ProcessTable::snapshot_with_budget(
                &mut command,
                Duration::from_millis(20),
                TABLE.len(),
            )
            .is_none()
        );
        let owner = tempfile::tempdir().unwrap();
        assert!(
            ProcessTable::snapshot_with_budget(
                &mut Command::new(owner.path().join("missing-command")),
                Duration::from_secs(1),
                TABLE.len(),
            )
            .is_none()
        );
    }

    #[test]
    fn operation_provenance_never_keeps_a_token_shaped_program_name() {
        for command in [
            "token=ghp_secretvalue --flag",
            "/tmp/a$(id) arg",
            "/usr/bin/name-with-a-very-long-suffix-that-goes-on-and-on-past-sixty-four-chars",
        ] {
            let identity = operation_agent_identity(&AgentProcess {
                pid: 7,
                started: "Mon Oct 5 12:34:56 2026".into(),
                command: command.into(),
            });
            assert_eq!(identity["program"], "unknown", "{command}");
        }
    }

    #[test]
    fn operation_provenance_keeps_only_the_agent_program_name() {
        let identity = operation_agent_identity(&AgentProcess {
            pid: 42,
            started: "Mon Oct 5 12:34:56 2026".into(),
            command: "/usr/local/bin/codex --token secret-value".into(),
        });
        assert_eq!(identity["pid"], 42);
        assert_eq!(identity["program"], "codex");
        let json = identity.to_string();
        assert!(!json.contains("secret-value"), "{json}");
        assert!(!json.contains("--token"), "{json}");
    }

    #[test]
    fn operation_provenance_lineage_stops_at_agent_root_and_redacts_arguments() {
        let table = ProcessTable::parse(
            "1 0 Mon Oct 5 12:00:00 2026 launchd\n300 1 Mon Oct 5 12:01:00 2026 node /opt/codex --api-key hidden\n400 300 Mon Oct 5 12:02:00 2026 /bin/zsh -l\n500 400 Mon Oct 5 12:03:00 2026 aethyme broker submit\n",
        );
        let lineage = table.lineage_to_agent_root(500).expect("agent ancestry");
        assert_eq!(
            lineage
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [500, 400, 300]
        );
        let persisted = lineage
            .iter()
            .map(operation_agent_identity)
            .collect::<Vec<_>>();
        let json = serde_json::Value::Array(persisted).to_string();
        assert!(!json.contains("--api-key"), "{json}");
        assert!(!json.contains("hidden"), "{json}");
    }

    #[test]
    fn the_caller_is_the_nearest_agent_ancestor() {
        let table = ProcessTable::parse(TABLE);
        let root = table.agent_root(88900).unwrap();
        assert_eq!(root.pid, 1977);
        assert_eq!(root.started, "Fri Oct 2 15:22:59 2026");
        assert_eq!(
            table.agent_root(2100).unwrap().pid,
            2000,
            "Codex's native child belongs to its node wrapper"
        );
    }

    #[test]
    fn an_agent_started_from_another_agents_shell_is_its_own_agent() {
        let table = ProcessTable::parse(&format!(
            "{TABLE} 4000 88836 Mon Oct  5 22:00:00 2026     /usr/local/bin/codex exec review\n"
        ));
        assert_eq!(table.agent_root(4000).unwrap().pid, 4000);
    }

    #[test]
    fn a_script_reparented_to_launchd_is_unidentified() {
        let table = ProcessTable::parse(TABLE);
        assert_eq!(table.agent_root(3000), None);
    }

    #[test]
    fn the_override_names_or_clears_the_agent() {
        let table = ProcessTable::parse(TABLE);
        assert_eq!(caller_in(&table, Some("0")), Caller::Unidentified);
        assert_eq!(caller_in(&table, Some("none")), Caller::Unidentified);
        let Caller::Agent(agent) = caller_in(&table, Some("2000")) else {
            panic!("override names pid 2000");
        };
        assert_eq!(agent.pid, 2000);
        assert_eq!(caller_in(&table, Some("424242")), Caller::Unidentified);
    }

    #[test]
    fn a_recycled_pid_is_not_alive() {
        let table = ProcessTable::parse(TABLE);
        let mut holder = table.process(1977).unwrap();
        assert!(table.is_alive(&holder));
        holder.started = "Thu Oct  1 09:00:00 2026".into();
        assert!(!table.is_alive(&holder));
    }

    #[test]
    fn shared_app_server_is_not_a_task_holder_or_agent_root() {
        let text = "3731 1 Fri Oct 23 08:42:41 2026 /opt/codex app-server --listen local --managed-daemon\n4000 3731 Fri Oct 23 09:00:00 2026 /bin/zsh\n5000 3731 Fri Oct 23 09:01:00 2026 /opt/codex exec task\n6000 1 Fri Oct 23 09:02:00 2026 node /opt/codex app-server\n";
        let table = ProcessTable::parse(text);
        assert!(!table.agent_pids().contains(&3731));
        assert!(!table.agent_pids().contains(&6000));
        assert_eq!(table.agent_root(4000), None);
        assert_eq!(table.agent_root(5000).unwrap().pid, 5000);
        assert_eq!(caller_in(&table, Some("3731")), Caller::Unidentified);
        assert!(matches!(caller_in(&table, Some("5000")), Caller::Agent(_)));
        assert!(is_agent_runtime("codex exec app-server"));
        assert!(is_agent_runtime("claude app-server"));
    }

    fn register_holder_test_session(store: &mut crate::BrokerStore) -> i64 {
        store
            .register_session(&crate::NewSession {
                worktree_path: "/tmp/holder-admission-fixture".into(),
                branch: "agent/holder-admission-fixture".into(),
                origin: crate::SessionOrigin::Adopted,
                task: Some("holder admission fixture".into()),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap()
            .id
    }

    fn bind_test_holder(store: &mut crate::BrokerStore, session_id: i64, holder: &AgentProcess) {
        store
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session_id),
                Some(&crate::events::session_holder_bound_payload(
                    holder,
                    reason::REGISTERED,
                    None,
                )),
            )
            .unwrap();
    }

    #[test]
    fn abandonment_refresh_preserves_other_primed_and_observed_bindings() {
        let mut table = RecordedHolderProcessTable::default();
        table.prefetched_event_ids.insert(8, 12);
        table.observed_event_ids.insert(9, 13);
        assert!(
            table
                .install_refreshed_snapshot(7, 11, Some(ProcessTable::default()))
                .is_some()
        );
        for (id, binding) in [(7, 11), (8, 12), (9, 13)] {
            assert!(
                table.get_or_snapshot(id, binding, false).is_some(),
                "a fresh global snapshot covers each unchanged binding without another ps"
            );
        }
        assert!(
            table.get_or_snapshot(8, 99, false).is_none(),
            "a rebound session cannot borrow an older binding's observation"
        );
        assert!(table.install_refreshed_snapshot(7, 11, None).is_none());
        assert!(
            table.get_or_snapshot(8, 12, false).is_none(),
            "a failed fresh snapshot cannot reuse old process rows"
        );
    }

    #[cfg(unix)]
    #[test]
    fn first_process_snapshot_is_not_repeated_for_a_rebound_candidate() {
        let mut table = RecordedHolderProcessTable::default();
        table.prefetched_event_ids.insert(7, 11);

        let _ = table.get_or_snapshot(7, 12, true);

        assert_eq!(table.snapshots_taken, 1);
        assert_eq!(table.observed_event_ids.get(&7), Some(&12));
    }

    #[test]
    fn abandonment_refresh_declined_does_not_reuse_cached_process_evidence() {
        let mut table = RecordedHolderProcessTable {
            snapshot: Some(Some(ProcessTable::default())),
            ..Default::default()
        };
        table.observed_event_ids.insert(7, 11);
        assert!(table.table_for(7, 11).is_some());
        assert!(table.refresh_for_binding(7, 11, false).is_none());
    }

    #[test]
    fn dead_shared_app_server_holder_is_rebound_without_takeover() {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&temporary.path().join("broker.db")).unwrap();
        let session_id = register_holder_test_session(&mut store);
        let table = ProcessTable::parse("5000 1 Fri Oct 23 09:01:00 2026 /opt/codex exec task\n");
        let holder = AgentProcess {
            pid: 3731,
            started: "Fri Oct 23 08:42:41 2026".into(),
            command: "/opt/codex app-server --listen local --managed-daemon".into(),
        };
        let caller_process = AgentProcess {
            pid: 5000,
            started: "Fri Oct 23 09:01:00 2026".into(),
            command: "/opt/codex exec task".into(),
        };
        let caller = Caller::Agent(caller_process.clone());
        bind_test_holder(&mut store, session_id, &holder);

        assert_eq!(
            admit(&mut store, session_id, &caller, &table, false).unwrap(),
            HolderCheck::Bound {
                reason: reason::HOLDER_GONE,
            },
            "a dead shared service is no longer a live holder"
        );
        assert_eq!(
            recorded_holder(&store, session_id).unwrap(),
            Some(caller_process)
        );
    }

    #[test]
    fn live_shared_app_server_holder_still_requires_explicit_takeover() {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&temporary.path().join("broker.db")).unwrap();
        let session_id = register_holder_test_session(&mut store);
        let table = ProcessTable::parse(
            "3731 1 Fri Oct 23 08:42:41 2026 /opt/codex app-server --listen local --managed-daemon\n\
             5000 1 Fri Oct 23 09:01:00 2026 /opt/codex exec task\n",
        );
        let holder = table.process(3731).unwrap();
        let caller = Caller::Agent(table.process(5000).unwrap());
        bind_test_holder(&mut store, session_id, &holder);

        assert!(matches!(
            admit(&mut store, session_id, &caller, &table, false),
            Err(crate::BrokerOpError::SessionHeldByAnotherAgent { .. })
        ));
        assert_eq!(recorded_holder(&store, session_id).unwrap(), Some(holder));
        assert_eq!(
            admit(&mut store, session_id, &caller, &table, true).unwrap(),
            HolderCheck::Bound {
                reason: reason::TAKE_OVER,
            }
        );
    }

    #[test]
    fn recycled_shared_app_server_pid_is_gone_when_start_time_differs() {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&temporary.path().join("broker.db")).unwrap();
        let session_id = register_holder_test_session(&mut store);
        let table = ProcessTable::parse(
            "3731 1 Sat Oct 24 08:42:41 2026 /opt/codex app-server --listen local --managed-daemon\n\
             5000 1 Fri Oct 23 09:01:00 2026 /opt/codex exec task\n",
        );
        let holder = AgentProcess {
            pid: 3731,
            started: "Fri Oct 23 08:42:41 2026".into(),
            command: "/opt/codex app-server --listen local --managed-daemon".into(),
        };
        let caller = Caller::Agent(table.process(5000).unwrap());
        bind_test_holder(&mut store, session_id, &holder);

        assert!(
            !table.is_alive(&holder),
            "the PID now names another process"
        );
        assert_eq!(
            admit(&mut store, session_id, &caller, &table, false).unwrap(),
            HolderCheck::Bound {
                reason: reason::HOLDER_GONE,
            }
        );
    }

    #[test]
    fn unidentified_caller_does_not_transfer_a_shared_holder() {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = crate::BrokerStore::open(&temporary.path().join("broker.db")).unwrap();
        let session_id = register_holder_test_session(&mut store);
        let table = ProcessTable::parse(
            "3731 1 Fri Oct 23 08:42:41 2026 /opt/codex app-server --listen local --managed-daemon\n",
        );
        let holder = table.process(3731).unwrap();
        bind_test_holder(&mut store, session_id, &holder);

        assert_eq!(
            admit(&mut store, session_id, &Caller::Unidentified, &table, true).unwrap(),
            HolderCheck::Unidentified
        );
        assert_eq!(recorded_holder(&store, session_id).unwrap(), Some(holder));
    }

    #[test]
    fn only_agents_other_than_the_holder_are_foreign() {
        let table = ProcessTable::parse(TABLE);
        let holder = table.process(1977).unwrap();
        let cwds = vec![
            (88900, "/wt/a/src".to_string()),
            (2100, "/wt/a".to_string()),
            (2023, "/wt/a".to_string()),
            (2000, "/wt/a".to_string()),
            (1297, "/wt/a".to_string()),
            (2100, "/elsewhere".to_string()),
        ];
        let sessions = vec![(7, "/wt/a".to_string(), Some(holder.clone()))];
        let found = find_foreign(&table, &cwds, &sessions);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, 7);
        let pids: Vec<i64> = found[0].foreign.iter().map(|agent| agent.pid).collect();
        assert_eq!(
            pids,
            vec![2000],
            "the holder's shell and a plain shell are not foreign"
        );

        let unheld = vec![(7, "/wt/a".to_string(), None)];
        assert_eq!(find_foreign(&table, &cwds, &unheld)[0].foreign.len(), 2);
        let alone = vec![(7, "/wt/a".to_string(), None)];
        assert!(find_foreign(&table, &cwds[..1], &alone).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn recorded_holder_requires_complete_exact_identity_evidence() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Holder test"],
            vec!["config", "user.email", "holder@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let session = broker
            .start_worktree("recorded holder fixture", None)
            .unwrap();

        let mut no_holder = RecordedHolderProcessTable::default();
        let unbound = recorded_holder_liveness(broker.store(), session.id, &mut no_holder, false);
        assert_eq!(
            unbound.liveness,
            RecordedHolderLiveness::Unbound,
            "sessions without a task-specific holder retain the existing age policy"
        );
        assert_eq!(unbound.binding_event_id, None);

        let table = ProcessTable::parse(TABLE);
        let holder = table.process(1977).unwrap();
        let holder_json = serde_json::to_string(&holder).unwrap();
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&holder_json),
            )
            .unwrap();
        let holder_event_id = broker
            .store()
            .latest_session_holder_event(session.id)
            .unwrap()
            .unwrap()
            .id;

        let mut unavailable = RecordedHolderProcessTable::default();
        assert_eq!(
            recorded_holder_liveness(broker.store(), session.id, &mut unavailable, false).liveness,
            RecordedHolderLiveness::Unknown,
            "a deadline that cannot afford a complete process snapshot protects the row"
        );
        let mut matching = RecordedHolderProcessTable {
            prefetched_event_ids: HashMap::from([(session.id, holder_event_id)]),
            observed_event_ids: HashMap::from([(session.id, holder_event_id)]),
            snapshot: Some(Some(table)),
            ..RecordedHolderProcessTable::default()
        };
        assert_eq!(
            recorded_holder_liveness(broker.store(), session.id, &mut matching, true).liveness,
            RecordedHolderLiveness::Alive
        );
        let mut missing = RecordedHolderProcessTable {
            prefetched_event_ids: HashMap::from([(session.id, holder_event_id)]),
            observed_event_ids: HashMap::from([(session.id, holder_event_id)]),
            snapshot: Some(Some(ProcessTable::parse(""))),
            ..RecordedHolderProcessTable::default()
        };
        assert_eq!(
            recorded_holder_liveness(broker.store(), session.id, &mut missing, true).liveness,
            RecordedHolderLiveness::Gone,
            "a complete snapshot missing the exact pid/start identity proves it gone"
        );

        let reassigned = AgentProcess {
            pid: holder.pid + 1,
            started: holder.started.clone(),
            command: holder.command.clone(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&serde_json::to_string(&reassigned).unwrap()),
            )
            .unwrap();
        assert_eq!(
            recorded_holder_liveness(broker.store(), session.id, &mut matching, false).liveness,
            RecordedHolderLiveness::Unknown,
            "a process snapshot for a previous holder binding cannot prove a rebound owner gone"
        );

        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some("not-json"),
            )
            .unwrap();
        let mut no_snapshot = RecordedHolderProcessTable::default();
        assert_eq!(
            recorded_holder_liveness(broker.store(), session.id, &mut no_snapshot, false).liveness,
            RecordedHolderLiveness::Unknown,
            "malformed holder records do not become age-only cleanup authority"
        );
    }

    /// Only an exact C-locale `lstart` text is read as a clock time.
    #[cfg(unix)]
    #[test]
    fn lstart_reads_only_an_exact_c_locale_start() {
        let monday = lstart_epoch_ms("Mon Jan 1 00:00:00 2024").expect("a real local time");
        assert_eq!(
            lstart_epoch_ms("Mon Jan  1 00:00:01 2024"),
            Some(monday + 1_000)
        );
        assert_eq!(lstart_epoch_ms("Tue Jan 1 00:00:00 2024"), None);
        assert_eq!(lstart_epoch_ms("Thu Feb 30 00:00:00 2024"), None);
        assert_eq!(lstart_epoch_ms("lun. 16 nov. 14:09:50 2026"), None);
        assert_eq!(lstart_epoch_ms("Mon Jan 1 00:00 2024"), None);
    }

    /// A refused listing is remembered for the pass: no retry follows it.
    #[test]
    fn a_refused_process_listing_is_not_retried_in_the_same_pass() {
        let mut table = RecordedHolderProcessTable::default();
        table.queue_denied_process_table_for_test();
        assert!(table.get_or_snapshot(1, 1, true).is_none());
        assert!(table.process_listing_denied());
        assert!(table.get_or_snapshot(1, 1, true).is_none());
        assert!(table.refresh_for_binding(1, 1, true).is_none());
        assert_eq!(table.snapshots_taken_for_test(), 1);
    }

    /// The native listing reports this process exactly as `ps` does, so a
    /// holder recorded from either source matches the other.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_native_process_table_agrees_with_ps_for_this_process() {
        let mut command = Command::new(crate::system_programs::ps());
        command.args(["-A", "-o", "pid=,ppid=,lstart=,args="]);
        let ps = ProcessTable::snapshot_with_budget(
            &mut command,
            PROCESS_TABLE_TIMEOUT,
            PROCESS_TABLE_STREAM_LIMIT,
        )
        .expect("ps lists processes");
        let Ok(native) = native_process_table::snapshot() else {
            panic!("the native listing is available to tests");
        };
        let pid = i64::from(std::process::id());
        let from_ps = ps.process(pid).expect("ps lists this process");
        let from_native = native.process(pid).expect("libproc lists this process");
        assert_eq!(from_native, from_ps);
        assert_eq!(native.rows[&pid].ppid, ps.rows[&pid].ppid);
        assert!(native.is_alive(&from_ps));
        assert!(ps.is_alive(&from_native));
    }
}
