//! Whether a Claude Code session may still be live for a transcript.
//!
//! Claude Code does not keep its transcript open while a session is idle, so
//! "not open" proves nothing: archiving an idle session's transcript would
//! split it (the next message starts a new file at the same path). A Claude
//! transcript is therefore never archived while any `claude` process runs
//! whose working directory maps to the transcript's project directory, or
//! whose command line names the transcript's session id. A process whose
//! working directory cannot be read counts as a match, and an unavailable
//! process table skips every Claude transcript.

use std::path::{Path, PathBuf};

use super::providers;

/// One running Claude Code process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeProcess {
    /// `None` when the working directory could not be read.
    pub cwd: Option<PathBuf>,
    pub args: String,
}

/// A source of running Claude Code processes.
pub(crate) trait AgentProbe {
    /// Every running Claude Code process, or why the table is unavailable.
    fn claude_processes(&mut self) -> Result<Vec<ClaudeProcess>, String>;
}

/// The host's own process table.
#[derive(Debug, Default)]
pub(crate) struct SystemAgents;

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Whether a command line runs Claude Code: its executable is `claude`, or a
/// JavaScript runtime runs `claude` or the `claude-code` package.
pub(crate) fn is_claude(args: &str) -> bool {
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if basename(first) == "claude" {
        return true;
    }
    let runtime = matches!(basename(first), "node" | "bun" | "deno");
    runtime
        && words
            .next()
            .is_some_and(|script| basename(script) == "claude" || script.contains("claude-code"))
}

impl AgentProbe for SystemAgents {
    fn claude_processes(&mut self) -> Result<Vec<ClaudeProcess>, String> {
        let table = crate::session_holder::ProcessTable::snapshot()
            .ok_or("the process table is unavailable")?;
        let processes = table.processes_matching(is_claude);
        if processes.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(target_os = "linux")]
        {
            let mut found = Vec::new();
            for (pid, args) in processes {
                let cwd = match std::fs::read_link(format!("/proc/{pid}/cwd")) {
                    Ok(cwd) => Some(cwd),
                    // Exited since the snapshot: no longer live.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => None,
                };
                found.push(ClaudeProcess { cwd, args });
            }
            Ok(found)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let pids: Vec<i64> = processes.iter().map(|(pid, _)| *pid).collect();
            let cwds = crate::session_holder::working_directories(
                &pids,
                std::time::Duration::from_secs(5),
            )
            .ok_or("working directories of agent processes are unavailable")?;
            Ok(processes
                .into_iter()
                .map(|(pid, args)| ClaudeProcess {
                    cwd: cwds
                        .iter()
                        .find(|(cwd_pid, _)| *cwd_pid == pid)
                        .map(|(_, cwd)| PathBuf::from(cwd)),
                    args,
                })
                .collect())
        }
    }
}

/// Claude Code's project directory name for a working directory: every
/// character other than an ASCII letter or digit becomes `-`.
pub(crate) fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn related(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('-'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('-'))
}

/// Whether any of `processes` may be the live session of a transcript in
/// project directory `project` with session id `session_id`. Errs towards
/// "live": an unreadable working directory matches, and so does a project
/// directory nested in or enclosing the process's.
pub(crate) fn claude_session_live(
    processes: &[ClaudeProcess],
    project: &str,
    session_id: Option<&str>,
) -> bool {
    processes.iter().any(|process| {
        let named = session_id.is_some_and(|id| {
            providers::uuids_in(&process.args)
                .iter()
                .any(|found| found == id)
        });
        named
            || match &process.cwd {
                None => true,
                Some(cwd) => related(&project_dir_name(cwd), project),
            }
    })
}
