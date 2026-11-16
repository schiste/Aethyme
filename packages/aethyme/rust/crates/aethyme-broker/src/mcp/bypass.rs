//! `aethyme mcp bypass`: running managed servers that did not start
//! through `aethyme mcp connect`.
//!
//! Read-only: one `ps` snapshot (`pid` and command line), within a one
//! second budget. A process whose command line matches a managed entry
//! ([`super::managed::MatchSet::managed_match`]: a launcher running a
//! managed package, or a managed executable) is a bypass, unless its
//! command line is exactly the stable resolution `connect` last exec'd for
//! that entry (remembered under `<host state>/mcp/resolved/`): `connect`
//! replaces itself with the server, so a server it started carries the
//! resolved argv, not a launcher. A fallback `connect` ran (the original
//! launcher command) counts as a bypass: the server did not get the
//! resolved start.
//!
//! Only counts per entry, and the matched package or executable name, are
//! reported; no command line leaves this module. When the snapshot cannot
//! be taken in time the result is `inconclusive`, never "none".

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::managed::MatchSet;
use super::telemetry::{self, Event, Remembered};

/// The scan's budget.
pub const BUDGET: Duration = Duration::from_secs(1);
const PS_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EntryCount {
    pub name: String,
    /// Bypassing processes.
    pub processes: usize,
    /// Per matched package or executable name.
    pub matched: BTreeMap<String, usize>,
    /// Processes running the entry's remembered `connect` resolution.
    pub via_connect: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BypassReport {
    /// `complete`, or `inconclusive` with a reason.
    pub state: &'static str,
    pub reason: Option<String>,
    pub processes_scanned: usize,
    pub entries: Vec<EntryCount>,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

impl BypassReport {
    pub fn total(&self) -> usize {
        self.entries.iter().map(|entry| entry.processes).sum()
    }

    fn inconclusive(reason: String, phases_ms: BTreeMap<&'static str, u64>) -> Self {
        Self {
            state: "inconclusive",
            reason: Some(reason),
            processes_scanned: 0,
            entries: Vec::new(),
            phases_ms,
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One `ps` row: the pid and the command line.
pub fn parse_ps(text: &str) -> Vec<(i64, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, command.trim().to_string()))
        })
        .collect()
}

/// Count bypasses in a process table. `remembered` maps an entry name to
/// the command line `connect` last exec'd for it.
pub fn count(
    set: &MatchSet,
    rows: &[(i64, String)],
    remembered: &BTreeMap<String, String>,
    own_pid: i64,
) -> Vec<EntryCount> {
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
    for (pid, command) in rows {
        if *pid == own_pid {
            continue;
        }
        let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        let Some(found) = set.managed_match(&words) else {
            continue;
        };
        let Some(entry) = counts.get_mut(&found.name) else {
            continue;
        };
        if remembered.get(&found.name) == Some(command) {
            entry.via_connect += 1;
            continue;
        }
        entry.processes += 1;
        *entry.matched.entry(found.matched).or_default() += 1;
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

/// Take one snapshot and count. Never claims "none" without a snapshot.
pub fn scan(set: &MatchSet, host: Option<&Path>, budget: Duration) -> BypassReport {
    let started = Instant::now();
    let mut phases = BTreeMap::new();
    if set.is_empty() {
        phases.insert("total", elapsed_ms(started));
        return BypassReport {
            state: "complete",
            reason: None,
            processes_scanned: 0,
            entries: Vec::new(),
            phases_ms: phases,
        };
    }
    let mut command = Command::new(crate::system_programs::ps());
    command
        .args(["-A", "-ww", "-o", "pid=,command="])
        .env("LC_ALL", "C");
    let output = crate::bounded_output::output_limited(&mut command, budget, PS_LIMIT);
    phases.insert("ps", elapsed_ms(started));
    let output = match output {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            output
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => {
            return BypassReport::inconclusive(format!("ps exited with {}", output.status), phases);
        }
        Ok(crate::bounded_output::LimitedOutput::Deadline) => {
            return BypassReport::inconclusive(
                format!("ps did not finish within {} ms", budget.as_millis()),
                phases,
            );
        }
        Ok(crate::bounded_output::LimitedOutput::OutputLimit) => {
            return BypassReport::inconclusive("the process table was too large".into(), phases);
        }
        Err(error) => {
            return BypassReport::inconclusive(format!("ps could not run: {error}"), phases);
        }
    };
    let phase = Instant::now();
    let rows = parse_ps(&String::from_utf8_lossy(&output.stdout));
    if rows.is_empty() {
        return BypassReport::inconclusive("ps listed no processes".into(), phases);
    }
    let remembered = remembered_commands(set, host);
    let entries = count(set, &rows, &remembered, i64::from(std::process::id()));
    phases.insert("match", elapsed_ms(phase));
    phases.insert("total", elapsed_ms(started));
    BypassReport {
        state: "complete",
        reason: None,
        processes_scanned: rows.len(),
        entries,
        phases_ms: phases,
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
            },
        ),
    );
}
