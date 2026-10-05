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
use std::process::Command;

/// Environment override for the caller's agent process: a pid, or `0`/`none`
/// to declare the caller unidentified.
pub const AGENT_PID_ENV: &str = "AETHYME_AGENT_PID";

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
    args: String,
}

/// One snapshot of the process table: a single `ps` call, walked in memory.
#[derive(Debug, Default)]
pub struct ProcessTable {
    rows: HashMap<i64, ProcessRow>,
}

impl ProcessTable {
    /// Snapshot the host's process table, or `None` when `ps` is unavailable.
    pub fn snapshot() -> Option<Self> {
        let output = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,lstart=,args="])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| Self::parse(&String::from_utf8_lossy(&output.stdout)))
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
                    args: fields[7..].join(" "),
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
            command: row.args.clone(),
        })
    }

    /// Whether `process` is still running: same pid and same start time.
    pub fn is_alive(&self, process: &AgentProcess) -> bool {
        self.rows
            .get(&process.pid)
            .is_some_and(|row| row.started == process.started)
    }

    /// The nearest ancestor of `pid` (itself included) that is an agent
    /// runtime.
    pub fn agent_root(&self, pid: i64) -> Option<AgentProcess> {
        let mut current = pid;
        // A cycle cannot occur in a real table; the bound keeps a malformed
        // snapshot from looping.
        for _ in 0..64 {
            let row = self.rows.get(&current)?;
            if is_agent_runtime(&row.args) {
                return self.process(current);
            }
            if row.ppid <= 1 || row.ppid == current {
                return None;
            }
            current = row.ppid;
        }
        None
    }
}

/// Whether a command line is an agent runtime: its executable is `claude` or
/// `codex` (including Codex's native `codex-*` binary), or it is `node`
/// running one of them.
fn is_agent_runtime(args: &str) -> bool {
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
    identified.map_or(Caller::Unidentified, Caller::Agent)
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
}

/// The session's current holder, from its latest `session.holder_bound` event.
/// `None` when it never had one, or retention pruned the record: the next
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
    let Some(holder) = recorded_holder(store, session_id)? else {
        record(store, session_id, agent, reason::FIRST_USE, None)?;
        return Ok(HolderCheck::Bound {
            reason: reason::FIRST_USE,
        });
    };
    if &holder == agent {
        return Ok(HolderCheck::Held);
    }
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
    fn the_caller_is_the_nearest_agent_ancestor() {
        let table = ProcessTable::parse(TABLE);
        let root = table.agent_root(88900).unwrap();
        assert_eq!(root.pid, 1977);
        assert_eq!(root.started, "Fri Oct 2 15:22:59 2026");
        assert_eq!(table.agent_root(2100).unwrap().pid, 2023);
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
}
