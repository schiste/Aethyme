//! `aethyme mcp bypass`: running managed servers that did not start
//! through `aethyme mcp connect`.
//!
//! Read-only, one snapshot within a one second budget. On Linux it reads
//! `/proc` (real argv from `cmdline`, the parent from `stat`, and the
//! environment where it is readable); on macOS one `ps` listing (pid, parent,
//! command line) and, for the candidates, the real argv and environment from
//! `KERN_PROCARGS2`; elsewhere `ps` alone, its command line split on spaces.
//!
//! Every process whose argv matches a managed entry
//! ([`super::managed::MatchSet::managed_match`]) is counted once, as:
//! - `via_connect`: it carries [`super::caller::CONNECT_MARKER`] (set by
//!   `connect` on the server it starts, and inherited by its children), or
//!   descends from `aethyme mcp connect` or the shared hub
//!   (`aethyme mcp hub serve`), or its command line is exactly the
//!   resolution `connect` last exec'd for the entry;
//! - `fallback`: it carries [`super::caller::FALLBACK_MARKER`]: `connect`
//!   could not start the server and ran the caller's own command;
//! - `bypass`: anything else.
//!
//! Only counts per entry and the matched package or executable name are
//! reported; no command line or environment value leaves this module. When
//! the snapshot cannot be taken in time the result is `inconclusive`, never
//! "none".

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::caller::{CONNECT_MARKER, FALLBACK_MARKER};
use super::managed::MatchSet;
use super::telemetry::{self, Event, Remembered};

/// The scan's budget.
pub const BUDGET: Duration = Duration::from_secs(1);
/// How far up the parent chain a process is followed.
const MAX_ANCESTORS: usize = 64;

/// A marker `connect` set on a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Connect,
    Fallback,
}

/// One process of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: i64,
    pub ppid: i64,
    pub argv: Vec<String>,
    /// `None` when there is no marker or the environment is unreadable.
    pub marker: Option<Marker>,
    /// Whether `argv` is the real argv. `false`: only `ps`'s command line
    /// was available (the kernel refused the process's arguments), so the
    /// process is counted `unreadable`, never `bypass`.
    pub readable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EntryCount {
    pub name: String,
    /// Processes started around `connect`.
    pub processes: usize,
    /// Bypassing processes per matched package or executable name.
    pub matched: BTreeMap<String, usize>,
    /// Processes `connect` (or the shared hub) started.
    pub via_connect: usize,
    /// Fallback commands `connect` ran.
    pub fallback: usize,
    /// Processes that seem to match but whose argv could not be read.
    pub unreadable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BypassReport {
    /// `complete`, or `inconclusive` with a reason.
    pub state: &'static str,
    pub reason: Option<String>,
    /// `proc`, `ps+procargs` or `ps`.
    pub source: &'static str,
    pub processes_scanned: usize,
    /// Processes whose argv could not be read; non-zero makes the result
    /// inconclusive.
    pub unreadable: usize,
    pub entries: Vec<EntryCount>,
    pub phases_ms: BTreeMap<&'static str, u64>,
    /// How to read the counts.
    pub note: &'static str,
}

/// What `via_connect` covers.
pub const NOTE: &str = "via_connect includes every descendant of a server connect or the shared \
                        hub started; fallback counts the caller's own command run by connect";

impl BypassReport {
    pub fn total(&self) -> usize {
        self.entries.iter().map(|entry| entry.processes).sum()
    }

    fn inconclusive(reason: String, phases_ms: BTreeMap<&'static str, u64>) -> Self {
        Self {
            state: "inconclusive",
            reason: Some(reason),
            source: SOURCE,
            processes_scanned: 0,
            unreadable: 0,
            entries: Vec::new(),
            phases_ms,
            note: NOTE,
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The marker in a NUL-separated environment block.
pub fn marker_in(environment: &[u8]) -> Option<Marker> {
    let connect = format!("{CONNECT_MARKER}=");
    let fallback = format!("{FALLBACK_MARKER}=");
    let mut found = None;
    for variable in environment.split(|byte| *byte == 0) {
        if variable.starts_with(connect.as_bytes()) {
            return Some(Marker::Connect);
        }
        if variable.starts_with(fallback.as_bytes()) {
            found = Some(Marker::Fallback);
        }
    }
    found
}

/// `ps` rows of `pid ppid command`, the command split on spaces.
pub fn parse_ps(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let argv: Vec<String> = words.map(str::to_string).collect();
            (!argv.is_empty()).then_some(Process {
                pid,
                ppid,
                argv,
                marker: None,
                readable: true,
            })
        })
        .collect()
}

fn is_aethyme_starter(argv: &[String]) -> bool {
    let program = argv
        .first()
        .map(|program| program.rsplit('/').next().unwrap_or(program));
    program.is_some_and(|name| name.starts_with("aethyme"))
        && argv.get(1).map(String::as_str) == Some("mcp")
        && matches!(argv.get(2).map(String::as_str), Some("connect" | "hub"))
}

/// Count the matching processes of a snapshot. `remembered` maps an entry
/// name to the command line `connect` last exec'd for it.
pub fn count(
    set: &MatchSet,
    processes: &[Process],
    remembered: &BTreeMap<String, String>,
    own_pid: i64,
) -> Vec<EntryCount> {
    let by_pid: BTreeMap<i64, &Process> = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    let started_by_aethyme = |process: &Process| {
        let mut current = by_pid.get(&process.ppid).copied();
        for _ in 0..MAX_ANCESTORS {
            let Some(ancestor) = current else {
                return false;
            };
            if is_aethyme_starter(&ancestor.argv) || ancestor.marker == Some(Marker::Connect) {
                return true;
            }
            if ancestor.ppid == ancestor.pid {
                return false;
            }
            current = by_pid.get(&ancestor.ppid).copied();
        }
        false
    };
    let mut counts: BTreeMap<String, EntryCount> = set
        .entries
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                EntryCount {
                    name: entry.name.clone(),
                    ..EntryCount::default()
                },
            )
        })
        .collect();
    for process in processes {
        if process.pid == own_pid {
            continue;
        }
        let Some(found) = set.managed_match(&process.argv) else {
            continue;
        };
        let Some(entry) = counts.get_mut(&found.name) else {
            continue;
        };
        if !process.readable {
            entry.unreadable += 1;
            continue;
        }
        let remembered_resolution = remembered.get(&found.name).map(String::as_str)
            == Some(process.argv.join(" ").as_str());
        match process.marker {
            Some(Marker::Connect) => entry.via_connect += 1,
            Some(Marker::Fallback) => entry.fallback += 1,
            None if remembered_resolution || started_by_aethyme(process) => {
                entry.via_connect += 1;
            }
            None => {
                entry.processes += 1;
                *entry.matched.entry(found.matched).or_default() += 1;
            }
        }
    }
    counts.into_values().collect()
}

/// The command lines `connect` last exec'd, per managed entry.
fn remembered_commands(set: &MatchSet, host: Option<&Path>) -> BTreeMap<String, String> {
    let Some(host) = host else {
        return BTreeMap::new();
    };
    set.entries
        .iter()
        .filter_map(|entry| {
            let path = telemetry::remembered_path(host, &format!("host-{}", entry.name));
            let remembered: Remembered =
                serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
            Some((entry.name.clone(), remembered.argv.join(" ")))
        })
        .collect()
}

#[cfg(target_os = "linux")]
const SOURCE: &str = "proc";
#[cfg(target_os = "macos")]
const SOURCE: &str = "ps+procargs";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SOURCE: &str = "ps";

/// Every process from `/proc`: real argv, parent, and the marker when the
/// environment is readable (this user's processes).
#[cfg(target_os = "linux")]
fn snapshot(_set: &MatchSet, deadline: Instant) -> Result<(Vec<Process>, usize), String> {
    let entries = std::fs::read_dir("/proc").map_err(|error| format!("/proc: {error}"))?;
    let mut processes = Vec::new();
    let mut unreadable = 0;
    for entry in entries.flatten() {
        if Instant::now() > deadline {
            return Err("the process table could not be read within the budget".into());
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i64>().ok())
        else {
            continue;
        };
        let directory = entry.path();
        // `pid (comm) state ppid ...`: comm may hold spaces and parentheses.
        let Ok(stat) = std::fs::read_to_string(directory.join("stat")) else {
            continue;
        };
        let Some(ppid) = stat
            .rfind(')')
            .and_then(|end| stat[end + 1..].split_whitespace().nth(1))
            .and_then(|field| field.parse().ok())
        else {
            continue;
        };
        let cmdline = match std::fs::read(directory.join("cmdline")) {
            Ok(cmdline) => cmdline,
            // The process exited meanwhile.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let argv: Vec<String> = cmdline
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8_lossy(word).into_owned())
            .collect();
        if argv.is_empty() {
            continue;
        }
        let marker = std::fs::read(directory.join("environ"))
            .ok()
            .and_then(|environment| marker_in(&environment));
        processes.push(Process {
            pid,
            ppid,
            argv,
            marker,
            readable: true,
        });
    }
    Ok((processes, unreadable))
}

#[cfg(not(target_os = "linux"))]
fn ps_snapshot(deadline: Instant) -> Result<Vec<Process>, String> {
    let budget = deadline.saturating_duration_since(Instant::now());
    let mut command = std::process::Command::new(crate::system_programs::ps());
    command
        .args(["-A", "-ww", "-o", "pid=,ppid=,command="])
        .env("LC_ALL", "C");
    match crate::bounded_output::output_limited(&mut command, budget, 16 * 1024 * 1024) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(parse_ps(&String::from_utf8_lossy(&output.stdout)))
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => {
            Err(format!("ps exited with {}", output.status))
        }
        Ok(crate::bounded_output::LimitedOutput::Deadline) => Err(format!(
            "ps did not finish within {} ms",
            budget.as_millis()
        )),
        Ok(crate::bounded_output::LimitedOutput::OutputLimit) => {
            Err("the process table was too large".into())
        }
        Err(error) => Err(format!("ps could not run: {error}")),
    }
}

/// One `ps` listing; the candidates' real argv and marker from
/// `KERN_PROCARGS2`.
#[cfg(target_os = "macos")]
fn snapshot(set: &MatchSet, deadline: Instant) -> Result<(Vec<Process>, usize), String> {
    let mut processes = ps_snapshot(deadline)?;
    for process in &mut processes {
        if Instant::now() > deadline {
            return Err("the process table could not be read within the budget".into());
        }
        if set.managed_match(&process.argv).is_none() {
            continue;
        }
        match procargs::read(process.pid) {
            Some((argv, marker)) => {
                process.argv = argv;
                process.marker = marker;
            }
            None => process.readable = false,
        }
    }
    Ok((processes, 0))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn snapshot(_set: &MatchSet, deadline: Instant) -> Result<(Vec<Process>, usize), String> {
    ps_snapshot(deadline).map(|processes| (processes, 0))
}

#[cfg(target_os = "macos")]
mod procargs {
    use super::{Marker, marker_in};

    /// `KERN_PROCARGS2` of `pid`: its argv and marker. `None` when the
    /// kernel refuses (another user's process).
    pub(super) fn read(pid: i64) -> Option<(Vec<String>, Option<Marker>)> {
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
        let mut buffer = vec![0u8; usize::try_from(argmax).ok()?];
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
        parse(buffer.get(..size)?)
    }

    /// A native-endian `argc`, the executable path, NUL padding, `argc`
    /// NUL-terminated arguments, then the environment.
    fn parse(buffer: &[u8]) -> Option<(Vec<String>, Option<Marker>)> {
        let width = std::mem::size_of::<libc::c_int>();
        let argc = libc::c_int::from_ne_bytes(buffer.get(..width)?.try_into().ok()?);
        let argc = usize::try_from(argc).ok().filter(|argc| *argc > 0)?;
        let rest = &buffer[width..];
        let mut cursor = rest.iter().position(|byte| *byte == 0)?;
        while rest.get(cursor) == Some(&0) {
            cursor += 1;
        }
        let mut words = rest.get(cursor..)?.split(|byte| *byte == 0);
        let argv: Vec<String> = words
            .by_ref()
            .take(argc)
            .map(|word| String::from_utf8_lossy(word).into_owned())
            .collect();
        let environment: Vec<u8> = words
            .take_while(|word| !word.is_empty())
            .flat_map(|word| word.iter().copied().chain(std::iter::once(0)))
            .collect();
        Some((argv, marker_in(&environment)))
    }
}

/// Take one snapshot and count. Never claims "none" without a snapshot.
pub fn scan(set: &MatchSet, host: Option<&Path>, budget: Duration) -> BypassReport {
    let started = Instant::now();
    let mut phases = BTreeMap::new();
    if set.is_empty() {
        phases.insert("total", elapsed_ms(started));
        return BypassReport {
            state: "complete",
            reason: None,
            source: SOURCE,
            processes_scanned: 0,
            unreadable: 0,
            entries: Vec::new(),
            phases_ms: phases,
            note: NOTE,
        };
    }
    let (processes, unreadable_unmatched) = match snapshot(set, started + budget) {
        Ok(snapshot) => snapshot,
        Err(reason) => {
            phases.insert("snapshot", elapsed_ms(started));
            return BypassReport::inconclusive(reason, phases);
        }
    };
    phases.insert("snapshot", elapsed_ms(started));
    if processes.is_empty() {
        return BypassReport::inconclusive("no process was listed".into(), phases);
    }
    let phase = Instant::now();
    let remembered = remembered_commands(set, host);
    let entries = count(set, &processes, &remembered, i64::from(std::process::id()));
    phases.insert("match", elapsed_ms(phase));
    phases.insert("total", elapsed_ms(started));
    let unreadable =
        unreadable_unmatched + entries.iter().map(|entry| entry.unreadable).sum::<usize>();
    BypassReport {
        state: if unreadable == 0 {
            "complete"
        } else {
            "inconclusive"
        },
        reason: (unreadable > 0).then(|| {
            format!("{unreadable} process(es) could not be read; they are not counted as bypass")
        }),
        source: SOURCE,
        processes_scanned: processes.len(),
        unreadable,
        entries,
        phases_ms: phases,
        note: NOTE,
    }
}

/// Record a scan in `events.jsonl` (counts and timings only).
pub fn record(host: &Path, report: &BypassReport) {
    let mut phases: BTreeMap<String, u64> = report
        .phases_ms
        .iter()
        .map(|(name, value)| ((*name).to_string(), *value))
        .collect();
    phases.insert("bypassing".into(), report.total() as u64);
    phases.insert(
        "via_connect".into(),
        report
            .entries
            .iter()
            .map(|entry| entry.via_connect as u64)
            .sum(),
    );
    phases.insert(
        "fallback".into(),
        report
            .entries
            .iter()
            .map(|entry| entry.fallback as u64)
            .sum(),
    );
    crate::warn_unrecorded(
        "record the MCP bypass event",
        telemetry::append(
            host,
            &Event {
                at_ms: telemetry::now_ms(),
                command: "bypass".into(),
                name: "bypass".into(),
                outcome: report.state.into(),
                reason: None,
                copy: None,
                key: None,
                phases_ms: phases,
                shadowed: false,
                fallback: false,
                warnings: Vec::new(),
            },
        ),
    );
}
