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

    /// The command lines of every running Codex process.
    fn codex_command_lines(&mut self) -> Result<Vec<String>, String>;
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

/// Whether a command line runs Codex: its executable is `codex` (or Codex's
/// native `codex-*` binary), or a JavaScript runtime runs one of them.
pub(crate) fn is_codex(args: &str) -> bool {
    let is_codex_name = |word: &str| {
        let name = basename(word);
        name == "codex" || name.starts_with("codex-")
    };
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if is_codex_name(first) {
        return true;
    }
    matches!(basename(first), "node" | "bun" | "deno") && words.next().is_some_and(is_codex_name)
}

/// Whether any of `command_lines` names `session_id`.
pub(crate) fn names_session(command_lines: &[String], session_id: &str) -> bool {
    command_lines.iter().any(|line| {
        providers::uuids_in(line)
            .iter()
            .any(|found| found == session_id)
    })
}

impl AgentProbe for SystemAgents {
    fn codex_command_lines(&mut self) -> Result<Vec<String>, String> {
        let table = crate::session_holder::ProcessTable::snapshot()
            .ok_or("the process table is unavailable")?;
        Ok(table
            .processes_matching(is_codex)
            .into_iter()
            .map(|(_, args)| args)
            .collect())
    }

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
/// UTF-16 code unit other than an ASCII letter or digit becomes `-`, as
/// Node's string replace does (a character outside the Basic Multilingual
/// Plane is two code units, so two dashes).
pub(crate) fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte),
            _ => '-',
        })
        .collect()
}

/// From this length on a project directory name may have been shortened
/// (file systems cap a name at 255 bytes, and an agent may truncate earlier
/// and add a suffix), so a shared prefix is enough to count as related.
pub(crate) const PLAUSIBLE_TRUNCATION: usize = 128;

fn related(a: &str, b: &str) -> bool {
    if a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('-'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('-'))
    {
        return true;
    }
    // A possibly truncated name (cut anywhere, perhaps with a suffix added
    // after the cut): related when the names agree up to the shorter one or
    // for at least the truncation length.
    if a.len().max(b.len()) < PLAUSIBLE_TRUNCATION {
        return false;
    }
    let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let shorter = a.len().min(b.len());
    common > 0 && common >= shorter.min(PLAUSIBLE_TRUNCATION)
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
