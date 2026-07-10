//! CLI for `aethyme broker ...` — a thin client of [`crate::Broker`].
//!
//! Owned by the broker crate (not the router binary) so the command
//! surface and the library evolve together; the `aethyme` router just
//! dispatches here. Contract: no logic beyond argument parsing and
//! rendering, and every command has a `--json` form whose shape comes
//! from the library's serializable types (#32).

use std::path::PathBuf;

use crate::broker::Broker;

const USAGE: &str = "\
aethyme broker — coordinate concurrent AI agent sessions on this repository

Usage:
  aethyme broker adopt [<path>] [--task <text>] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory.
  aethyme broker start-agent --task <text> --cmd <command> [--json]
      Create a worktree + branch and spawn <command> in it (sh -c),
      logging to .aethyme/logs/.
  aethyme broker agents [--json]
      List live sessions with activity-derived liveness, refreshing
      diff-derived leases and warning on overlapping edits.
  aethyme broker leases [--json]
      Refresh and list active leases plus current overlaps.
  aethyme broker leases claim <path> --session <id> [--ttl <seconds>] [--json]
      Explicitly claim a path (end it with / for a directory claim).
  aethyme broker leases release <path> --session <id> [--json]
      Release an explicit claim.
  aethyme broker cleanup <session-id> [--force] [--json]
      Remove a session's worktree. Refuses on uncommitted changes or
      unmerged commits unless --force.

Overlaps warn — they never block (v0 policy).
";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Entry point for the router. Returns a process exit code.
pub fn run(args: &[String]) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(UsageError::Help) => {
            eprint!("{USAGE}");
            2
        }
        Err(UsageError::Message(message)) => {
            eprintln!("Error: {message}");
            1
        }
    }
}

enum UsageError {
    Help,
    Message(String),
}

impl<E: std::fmt::Display> From<E> for UsageError {
    fn from(err: E) -> Self {
        UsageError::Message(err.to_string())
    }
}

struct Parsed {
    positional: Vec<String>,
    task: Option<String>,
    cmd: Option<String>,
    session: Option<i64>,
    ttl_seconds: Option<i64>,
    json: bool,
    force: bool,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        positional: Vec::new(),
        task: None,
        cmd: None,
        session: None,
        ttl_seconds: None,
        json: false,
        force: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--force" => parsed.force = true,
            "--task" => {
                parsed.task = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--task requires a value".into()))?
                        .clone(),
                )
            }
            "--cmd" => {
                parsed.cmd = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--cmd requires a value".into()))?
                        .clone(),
                )
            }
            "--session" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--session requires a value".into()))?;
                parsed.session = Some(value.parse().map_err(|_| {
                    UsageError::Message("--session must be an integer session id".into())
                })?);
            }
            "--ttl" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--ttl requires a value".into()))?;
                parsed.ttl_seconds = Some(value.parse().map_err(|_| {
                    UsageError::Message("--ttl must be an integer number of seconds".into())
                })?);
            }
            "-h" | "--help" => return Err(UsageError::Help),
            other if other.starts_with('-') => {
                return Err(UsageError::Message(format!("unknown flag {other}")));
            }
            positional => parsed.positional.push(positional.to_string()),
        }
    }
    Ok(parsed)
}

fn print_overlap_warnings(overlaps: &[crate::Overlap]) {
    for overlap in overlaps {
        eprintln!(
            "⚠ overlap: sessions {} and {} are both touching {}",
            overlap.session_a, overlap.session_b, overlap.path
        );
    }
}

fn open_broker() -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    Ok(Broker::open(&cwd)?)
}

fn run_inner(args: &[String]) -> Result<(), UsageError> {
    let Some(subcommand) = args.first() else {
        return Err(UsageError::Help);
    };
    let parsed = parse(&args[1..])?;

    match subcommand.as_str() {
        "adopt" => {
            let mut broker = open_broker()?;
            let path = parsed.positional.first().map(PathBuf::from).unwrap_or(
                std::env::current_dir().map_err(|e| UsageError::Message(e.to_string()))?,
            );
            let session = broker.adopt(&path, parsed.task.as_deref())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                println!(
                    "Adopted session {} — worktree {} on branch {}",
                    session.id, session.worktree_path, session.branch
                );
            }
        }
        "start-agent" => {
            let task = parsed
                .task
                .ok_or(UsageError::Message("start-agent requires --task".into()))?;
            let cmd = parsed
                .cmd
                .ok_or(UsageError::Message("start-agent requires --cmd".into()))?;
            let mut broker = open_broker()?;
            let session = broker.start_agent(&task, &cmd)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                println!(
                    "Started session {} (pid {}) — worktree {} on branch {}\nLog: {}",
                    session.id,
                    session.pid.unwrap_or(-1),
                    session.worktree_path,
                    session.branch,
                    session.log_path.as_deref().unwrap_or("-"),
                );
            }
        }
        "agents" => {
            let mut broker = open_broker()?;
            let overlaps = broker.refresh_leases()?;
            let views = broker.agents(now_ms())?;
            if parsed.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "agents": views,
                        "overlaps": overlaps,
                    }))?
                );
            } else if views.is_empty() {
                println!("No live sessions. Register one with `aethyme broker adopt`.");
            } else {
                println!(
                    "{:<4} {:<8} {:<8} {:<24} {}",
                    "ID", "STATUS", "ORIGIN", "BRANCH", "TASK"
                );
                for view in views {
                    println!(
                        "{:<4} {:<8} {:<8} {:<24} {}",
                        view.session.id,
                        view.derived_status.as_str(),
                        view.session.origin.as_str(),
                        view.session.branch,
                        view.session.task.as_deref().unwrap_or("-"),
                    );
                }
                print_overlap_warnings(&overlaps);
            }
        }
        "leases" => {
            let mut broker = open_broker()?;
            match parsed.positional.first().map(String::as_str) {
                None => {
                    let overlaps = broker.refresh_leases()?;
                    let leases = broker.store().active_leases()?;
                    if parsed.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "leases": leases,
                                "overlaps": overlaps,
                            }))?
                        );
                    } else if leases.is_empty() {
                        println!("No active leases.");
                    } else {
                        println!("{:<4} {:<9} {}", "SID", "KIND", "PATH");
                        for lease in leases {
                            println!(
                                "{:<4} {:<9} {}",
                                lease.session_id,
                                lease.kind.as_str(),
                                lease.path
                            );
                        }
                        print_overlap_warnings(&overlaps);
                    }
                }
                Some("claim") => {
                    let path = parsed
                        .positional
                        .get(1)
                        .ok_or(UsageError::Message("claim requires a path".into()))?;
                    let session = parsed
                        .session
                        .ok_or(UsageError::Message("claim requires --session <id>".into()))?;
                    let lease = broker.store().claim_lease(
                        session,
                        path,
                        parsed.ttl_seconds.map(|s| s * 1000),
                    )?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&lease)?);
                    } else {
                        println!("Session {session} claimed {path}.");
                    }
                }
                Some("release") => {
                    let path = parsed
                        .positional
                        .get(1)
                        .ok_or(UsageError::Message("release requires a path".into()))?;
                    let session = parsed.session.ok_or(UsageError::Message(
                        "release requires --session <id>".into(),
                    ))?;
                    broker.store().release_lease(session, path)?;
                    if parsed.json {
                        println!("{{\"released\":{}}}", serde_json::to_string(path)?);
                    } else {
                        println!("Session {session} released {path}.");
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown leases action {other:?} — expected claim or release"
                    )));
                }
            }
        }
        "cleanup" => {
            let id: i64 = parsed
                .positional
                .first()
                .ok_or(UsageError::Message("cleanup requires a session id".into()))?
                .parse()
                .map_err(|_| UsageError::Message("session id must be an integer".into()))?;
            let mut broker = open_broker()?;
            broker.cleanup(id, parsed.force)?;
            if parsed.json {
                println!("{{\"cleaned\":{id}}}");
            } else {
                println!("Cleaned session {id}.");
            }
        }
        "-h" | "--help" => return Err(UsageError::Help),
        other => {
            return Err(UsageError::Message(format!(
                "unknown broker subcommand {other:?} — see `aethyme broker --help`"
            )));
        }
    }
    Ok(())
}
