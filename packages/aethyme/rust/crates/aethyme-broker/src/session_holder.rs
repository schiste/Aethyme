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

    /// Every process that is itself an agent runtime.
    pub fn agent_pids(&self) -> Vec<i64> {
        let mut pids: Vec<i64> = self
            .rows
            .iter()
            .filter(|(_, row)| is_agent_runtime(&row.args))
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
            if is_agent_runtime(&row.args) {
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
                Some(parent) if ppid != current && is_agent_runtime(&parent.args) => {
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

/// A compact process identity for local operation history; never the full command line.
fn operation_agent_identity(process: &AgentProcess) -> serde_json::Value {
    let program = process
        .command
        .split_whitespace()
        .next()
        .and_then(|command| Path::new(command).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string();
    serde_json::json!({
        "pid": process.pid,
        "started": process.started,
        "program": program,
    })
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
}
