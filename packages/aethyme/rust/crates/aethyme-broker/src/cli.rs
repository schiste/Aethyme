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
  aethyme certify [--json]             (also: aethyme broker certify)
      The certification method: deterministic, strictly read-only checks
      (git version, repo, configs valid, gitignore contract, protocol,
      db integrity). Exits non-zero on failures — CI/cron-able as the
      recurring inspection. Never writes anything.
  aethyme broker scaffold [--json]
      Deterministic setup: ONLY what the broker needs, with content that
      is identical for every repo (config.toml skeleton, .gitignore
      block, broker database). Never overwrites. Certify + scaffold are
      the 'always exactly the same' pair — one reads, one writes.
  aethyme broker gates draft [--json]
      Adaptive (NOT scaffolding): sniff this repo's manifests and draft
      a gates.toml. Output depends on the repo — review it, then run
      certify.
  aethyme broker adopt [<path>] [--task <text>] [--reuse|--replace-stale] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory. If the worktree already has a session:
      --reuse points it at a follow-up task (fresh baseline);
      --replace-stale closes it (state only) and registers fresh;
      neither flag = error listing your options.
  aethyme broker close --session <id> [--json]
      Mark a session finished. State only — never touches the worktree
      (use cleanup to also remove a spawned session's worktree).
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
  aethyme broker gates validate [--json]
      Parse and validate .aethyme/gates.toml.
  aethyme broker gates affected --session <id> [--json]
      Show which gates the session's diff selects and why.
  aethyme broker gates run --session <id> [--json]
      Run affected gates cheap-first with tree-hash caching; cancels this
      session's obsolete in-flight runs; stops at first failure.
  aethyme broker submit --session <id> [--json]
      Submit the session's head commit: simulate the merge onto the local
      integration branch, run affected gates on the merged tree, and
      promote when verified (default; set [promote] mode = 'manual' to
      hold verified entries for explicit promote). Conflicts reject
      before any gate runs and write instructions to
      <worktree>/.aethyme/broker-action-required.md.
  aethyme broker queue [--json]
      Show the merge queue.
  aethyme broker promote --entry <id> [--json]
      Manual-mode only: advance the local integration branch to a verified
      entry's merge commit; other in-flight entries are re-simulated.
      Never pushes.
  aethyme broker status [--json]
      The whole picture: agents, overlaps, merge queue, integration head.
  aethyme broker events [--since <id>] [--kind <prefix>] [--follow] [--json]
      Show the append-only event log (see docs/events-contract.md).
      --kind filters by prefix (e.g. merge. or lease.overlap); --follow
      polls for new events and survives transient read errors.
  aethyme broker events prune --keep-days <n> [--json]
      Retention: delete events older than <n> days. Event ids stay
      strictly increasing, so existing --since cursors remain valid.
  aethyme broker metrics [--json]
      Cost/benefit accounting from safe local telemetry: broker command
      latency (names + numbers only, never task text or paths), gate
      executions vs cache hits with time saved, conflicts caught before
      any gate ran, overlaps warned.
  aethyme broker doctor [--json]
      Health checks: database integrity, sessions whose worktree is
      gone, and orphaned gate pidfiles.
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
    let started = std::time::Instant::now();
    let code = match run_inner(args) {
        Ok(()) => 0,
        Err(UsageError::Help) => {
            eprint!("{USAGE}");
            2
        }
        Err(UsageError::Message(message)) => {
            eprintln!("Error: {message}");
            1
        }
    };
    record_command_metric(args, code, started.elapsed().as_millis() as i64);
    code
}

/// Safe-by-construction command telemetry: the label is built ONLY from
/// an allowlist of known subcommand words, so positional values (paths,
/// session ids, task text) can never leak into the metrics file. Best
/// effort — any failure is silently ignored.
fn record_command_metric(args: &[String], exit: u8, duration_ms: i64) {
    const KNOWN: &[&str] = &[
        "adopt",
        "start-agent",
        "agents",
        "leases",
        "claim",
        "release",
        "gates",
        "draft",
        "validate",
        "affected",
        "run",
        "submit",
        "queue",
        "promote",
        "status",
        "events",
        "prune",
        "metrics",
        "doctor",
        "cleanup",
        "certify",
        "scaffold",
    ];
    let label: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| KNOWN.contains(a))
        .take(2)
        .collect();
    if label.is_empty() || label == ["metrics"] {
        return; // nothing recognizable, or reading metrics itself
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    let dir = main_root.join(".aethyme/logs");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let line = format!(
        "{{\"ts\":{ts},\"command\":\"{}\",\"duration_ms\":{duration_ms},\"exit\":{exit}}}\n",
        label.join(".")
    );
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("command-metrics.jsonl"))
    {
        let _ = file.write_all(line.as_bytes());
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
    entry: Option<i64>,
    ttl_seconds: Option<i64>,
    since: Option<i64>,
    kind: Option<String>,
    keep_days: Option<i64>,
    follow: bool,
    json: bool,
    force: bool,
    check: bool,
    reuse: bool,
    replace_stale: bool,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        positional: Vec::new(),
        task: None,
        cmd: None,
        session: None,
        entry: None,
        ttl_seconds: None,
        since: None,
        kind: None,
        keep_days: None,
        follow: false,
        json: false,
        force: false,
        check: false,
        reuse: false,
        replace_stale: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--follow" => parsed.follow = true,
            "--since" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--since requires a value".into()))?;
                parsed.since = Some(value.parse().map_err(|_| {
                    UsageError::Message("--since must be an integer event id".into())
                })?);
            }
            "--force" => parsed.force = true,
            "--check" => parsed.check = true,
            "--reuse" => parsed.reuse = true,
            "--replace-stale" => parsed.replace_stale = true,
            "--kind" => {
                parsed.kind = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--kind requires a value".into()))?
                        .clone(),
                )
            }
            "--keep-days" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--keep-days requires a value".into()))?;
                parsed.keep_days =
                    Some(value.parse().map_err(|_| {
                        UsageError::Message("--keep-days must be an integer".into())
                    })?);
            }
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
            "--entry" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--entry requires a value".into()))?;
                parsed.entry = Some(value.parse().map_err(|_| {
                    UsageError::Message("--entry must be an integer queue entry id".into())
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

fn aethyme_gates_load(main_root: &std::path::Path) -> Result<Vec<crate::Gate>, UsageError> {
    Ok(crate::load_gates(main_root)?)
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
            let mode = match (parsed.reuse, parsed.replace_stale) {
                (true, true) => {
                    return Err(UsageError::Message(
                        "--reuse and --replace-stale are mutually exclusive".into(),
                    ));
                }
                (true, false) => crate::AdoptMode::Reuse,
                (false, true) => crate::AdoptMode::ReplaceStale,
                (false, false) => crate::AdoptMode::New,
            };
            let session = broker.adopt_with(&path, parsed.task.as_deref(), mode)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                let verb = if parsed.reuse { "Reusing" } else { "Adopted" };
                println!(
                    "{verb} session {} — worktree {} on branch {}",
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
                    "{:<4} {:<8} {:<8} {:<24} TASK",
                    "ID", "STATUS", "ORIGIN", "BRANCH"
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
                        println!("{:<4} {:<9} PATH", "SID", "KIND");
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
        "gates" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "gates requires an action: validate, affected, or run".into(),
                    ))?;
            match action {
                "draft" => {
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let report = crate::init::draft_gates(&cwd)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        for check in &report.checks {
                            println!(
                                "{:<8} {}",
                                format!("{:?}", check.status).to_lowercase(),
                                check.detail
                            );
                        }
                    }
                }
                "validate" => {
                    let broker = open_broker()?;
                    let gates = aethyme_gates_load(broker.main_root())?;
                    if parsed.json {
                        let summary: Vec<_> = gates
                            .iter()
                            .map(|g| {
                                serde_json::json!({
                                    "name": g.name, "command": g.command,
                                    "cost": g.cost, "triggers": g.triggers,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        println!("gates.toml OK — {} gate(s), cheap-first:", gates.len());
                        for gate in gates {
                            println!(
                                "  [{}] {} — {} (triggers: {})",
                                gate.cost,
                                gate.name,
                                gate.command,
                                if gate.triggers.is_empty() {
                                    "always".to_string()
                                } else {
                                    gate.triggers.join(", ")
                                }
                            );
                        }
                    }
                }
                "affected" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates affected requires --session <id>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let selections = broker.affected_gates(session)?;
                    if parsed.json {
                        let out: Vec<_> = selections
                            .iter()
                            .map(|(gate, why)| {
                                serde_json::json!({"gate": gate, "triggered_by": why})
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    } else if selections.is_empty() {
                        println!("No gates affected by this session's diff.");
                    } else {
                        for (gate, why) in selections {
                            match why {
                                Some(path) => println!("{gate}  (triggered by {path})"),
                                None => println!("{gate}  (always runs)"),
                            }
                        }
                    }
                }
                "run" => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "gates run requires --session <id>".into(),
                    ))?;
                    let mut broker = open_broker()?;
                    let outcomes = broker.run_gates(session)?;
                    if parsed.json {
                        println!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else if outcomes.is_empty() {
                        println!("No gates affected — nothing to run.");
                    } else {
                        let mut failed = false;
                        for outcome in &outcomes {
                            println!(
                                "{:<20} {:<10} {}{}",
                                outcome.gate,
                                outcome.status.as_str(),
                                if outcome.cached { "(cached) " } else { "" },
                                outcome
                                    .duration_ms
                                    .map(|ms| format!("{ms}ms"))
                                    .unwrap_or_default(),
                            );
                            failed |= outcome.status.as_str() != "pass";
                        }
                        if failed {
                            return Err(UsageError::Message(
                                "one or more gates did not pass".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown gates action {other:?} — expected draft, validate, affected, or run"
                    )));
                }
            }
        }
        "submit" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("submit requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            // Preflight (dogfood feedback 2026-07-14): show exactly what
            // will be submitted before anything runs — and warn about
            // uncommitted work, which never integrates.
            if !parsed.json {
                if let Ok(info) = broker.store().session(session) {
                    if let Ok(checkout) =
                        crate::GitRepo::discover(std::path::Path::new(&info.worktree_path))
                    {
                        let head = checkout.head_commit().unwrap_or_default();
                        println!(
                            "Submitting session {session} — HEAD {}",
                            &head[..12.min(head.len())]
                        );
                        if let Some(base) = info.diff_base.as_deref() {
                            if let Ok(commits) = checkout.commit_summaries(base, "HEAD", 10) {
                                if commits.is_empty() {
                                    println!(
                                        "  no commits since the session baseline — \
                                         nothing new to integrate"
                                    );
                                }
                                for line in &commits {
                                    println!("  {line}");
                                }
                            }
                        }
                        if let Ok(dirty) = checkout.dirty_paths() {
                            if !dirty.is_empty() {
                                println!(
                                    "  ⚠ {} uncommitted change(s) NOT included \
                                     (only committed work integrates), e.g. {}",
                                    dirty.len(),
                                    dirty.first().map(String::as_str).unwrap_or("")
                                );
                            }
                        }
                    }
                }
            }
            let outcome = broker.submit(session)?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else if !outcome.conflicts.is_empty() {
                eprintln!("✗ conflict — rejected before any gate ran. Conflicting files:");
                for file in &outcome.conflicts {
                    eprintln!("  - {file}");
                }
                eprintln!(
                    "Instructions written to the session worktree at {}",
                    crate::ACTION_REQUIRED_RELPATH
                );
                return Err(UsageError::Message("submission conflicted".into()));
            } else {
                for gate in &outcome.gate_outcomes {
                    println!(
                        "gate {:<20} {}{}",
                        gate.gate,
                        gate.status.as_str(),
                        if gate.cached { " (cached)" } else { "" }
                    );
                }
                println!(
                    "entry {} → {}{}",
                    outcome.entry.id,
                    outcome.entry.status.as_str(),
                    if outcome.promoted {
                        " (auto-promoted)"
                    } else {
                        ""
                    }
                );
                if outcome.entry.status.as_str() == "rejected" {
                    return Err(UsageError::Message(
                        "gates failed on the merged tree".into(),
                    ));
                }
                // "What now?" — the next expected human action was
                // implicit (dogfood feedback 2026-07-14).
                if outcome.promoted {
                    let integration = broker
                        .integration_head()
                        .map(|(_, commit)| commit[..12.min(commit.len())].to_string())
                        .unwrap_or_else(|_| "?".into());
                    println!(
                        "What now: aethyme/integration is at {integration} and contains this work. \
                         Your checkout and branches are untouched — keep working, or start \
                         a follow-up with `aethyme broker adopt --reuse --task \"...\"`, or \
                         finish with `aethyme broker close --session {}`.",
                        outcome.entry.session_id,
                    );
                } else {
                    println!(
                        "What now: entry {} is verified but not promoted (manual mode). \
                         Promote with `aethyme broker promote --entry {}`.",
                        outcome.entry.id, outcome.entry.id,
                    );
                }
            }
        }
        "queue" => {
            let mut broker = open_broker()?;
            let entries = broker.store().merge_queue()?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("Merge queue is empty.");
            } else {
                println!("{:<4} {:<4} {:<11} HEAD", "ID", "SID", "STATUS");
                for entry in entries {
                    println!(
                        "{:<4} {:<4} {:<11} {}",
                        entry.id,
                        entry.session_id,
                        entry.status.as_str(),
                        &entry.head_commit[..12.min(entry.head_commit.len())]
                    );
                }
            }
        }
        "promote" => {
            let entry = parsed
                .entry
                .ok_or(UsageError::Message("promote requires --entry <id>".into()))?;
            let mut broker = open_broker()?;
            broker.promote(entry)?;
            if parsed.json {
                println!("{{\"promoted\":{entry}}}");
            } else {
                println!("Promoted entry {entry} to the local integration branch.");
            }
        }
        "status" => {
            let mut broker = open_broker()?;
            let status = broker.status(now_ms())?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "Integration: {} @ {}",
                    status.integration_branch,
                    &status.integration_head[..12.min(status.integration_head.len())]
                );
                println!();
                if status.agents.is_empty() {
                    println!("No live sessions.");
                } else {
                    println!(
                        "{:<4} {:<8} {:<8} {:<24} TASK",
                        "ID", "STATUS", "ORIGIN", "BRANCH"
                    );
                    for view in &status.agents {
                        println!(
                            "{:<4} {:<8} {:<8} {:<24} {}",
                            view.session.id,
                            view.derived_status.as_str(),
                            view.session.origin.as_str(),
                            view.session.branch,
                            view.session.task.as_deref().unwrap_or("-"),
                        );
                    }
                }
                if !status.queue.is_empty() {
                    println!();
                    println!("{:<4} {:<4} {:<11} HEAD", "QID", "SID", "QSTATUS");
                    for entry in &status.queue {
                        println!(
                            "{:<4} {:<4} {:<11} {}",
                            entry.id,
                            entry.session_id,
                            entry.status.as_str(),
                            &entry.head_commit[..12.min(entry.head_commit.len())]
                        );
                    }
                }
                print_overlap_warnings(&status.overlaps);
            }
        }
        "events" => {
            if parsed.positional.first().map(String::as_str) == Some("prune") {
                let keep_days = parsed.keep_days.ok_or(UsageError::Message(
                    "events prune requires --keep-days <n>".into(),
                ))?;
                let mut broker = open_broker()?;
                let cutoff = now_ms() - keep_days * 24 * 60 * 60 * 1000;
                let removed = broker.store().prune_events_before(cutoff)?;
                if parsed.json {
                    println!("{{\"pruned\":{removed}}}");
                } else {
                    println!("Pruned {removed} event(s) older than {keep_days} day(s).");
                }
                return Ok(());
            }
            let mut broker = open_broker()?;
            let mut cursor = parsed.since.unwrap_or(0);
            // --follow survives transient read errors (e.g. a checkpoint
            // or a busy writer) with bounded retries instead of dying.
            let mut consecutive_errors = 0u32;
            loop {
                let events =
                    match broker
                        .store()
                        .events_after_filtered(cursor, 1000, parsed.kind.as_deref())
                    {
                        Ok(events) => {
                            consecutive_errors = 0;
                            events
                        }
                        Err(err) if parsed.follow && consecutive_errors < 5 => {
                            consecutive_errors += 1;
                            eprintln!("events: transient read error ({err}); retrying");
                            std::thread::sleep(std::time::Duration::from_millis(700));
                            continue;
                        }
                        Err(err) => return Err(err.into()),
                    };
                for event in &events {
                    cursor = event.id;
                    if parsed.json {
                        println!("{}", serde_json::to_string(event)?);
                    } else {
                        println!(
                            "{:<6} {} {:<28} sid={} {}",
                            event.id,
                            event.ts,
                            event.kind,
                            event
                                .session_id
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "-".into()),
                            event.payload_json.as_deref().unwrap_or(""),
                        );
                    }
                }
                if !parsed.follow {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        "metrics" => {
            let mut broker = open_broker()?;
            // Gate executions (pass/fail) vs cache hits with saved time.
            let executed = broker.store().gate_execution_totals()?;
            let cached = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("gate.cached"))?;
            let saved_ms: i64 = cached
                .iter()
                .filter_map(|e| e.payload_json.as_deref())
                .filter_map(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .filter_map(|v| v.get("saved_ms").and_then(|s| s.as_i64()))
                .sum();
            let conflicts = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("merge.conflict"))?
                .len();
            let overlaps = broker
                .store()
                .events_after_filtered(0, i64::MAX, Some("lease.overlap"))?
                .len();

            // Command latency from the safe telemetry file.
            let mut commands: std::collections::BTreeMap<String, (i64, i64)> =
                std::collections::BTreeMap::new();
            let metrics_path = broker
                .main_root()
                .join(".aethyme/logs/command-metrics.jsonl");
            if let Ok(text) = std::fs::read_to_string(&metrics_path) {
                for line in text.lines() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        let name = v
                            .get("command")
                            .and_then(|c| c.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let ms = v.get("duration_ms").and_then(|d| d.as_i64()).unwrap_or(0);
                        let entry = commands.entry(name).or_insert((0, 0));
                        entry.0 += 1;
                        entry.1 += ms;
                    }
                }
            }

            if parsed.json {
                let out = serde_json::json!({
                    "gates_executed": executed.iter().map(|(g, n, ms)| serde_json::json!({
                        "gate": g, "runs": n, "total_ms": ms,
                    })).collect::<Vec<_>>(),
                    "gate_cache_hits": cached.len(),
                    "gate_time_saved_ms": saved_ms,
                    "conflicts_caught_pre_gate": conflicts,
                    "overlaps_warned": overlaps,
                    "commands": commands.iter().map(|(name, (count, ms))| serde_json::json!({
                        "command": name, "count": count, "total_ms": ms,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Gate executions:");
                for (gate, runs, ms) in &executed {
                    println!("  {gate:<20} {runs} run(s), {ms}ms total");
                }
                println!(
                    "Cache hits: {} (≈{}s of checks skipped)",
                    cached.len(),
                    saved_ms / 1000
                );
                println!("Conflicts caught before any gate ran: {conflicts}");
                println!("Overlap warnings: {overlaps}");
                println!("Broker command overhead:");
                for (name, (count, ms)) in &commands {
                    println!(
                        "  {name:<20} {count} call(s), {ms}ms total, {}ms avg",
                        ms / count.max(&1)
                    );
                }
            }
        }
        "doctor" => {
            let mut broker = open_broker()?;
            let report = broker.doctor()?;
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("integrity: {}", report.integrity);
                if report.missing_worktrees.is_empty() {
                    println!("worktrees: all live session worktrees exist");
                } else {
                    for id in &report.missing_worktrees {
                        println!("worktrees: session {id} worktree is missing (adopt gone stale?)");
                    }
                }
                if report.orphaned_pidfiles.is_empty() {
                    println!("gate runs: no orphaned pidfiles");
                } else {
                    for name in &report.orphaned_pidfiles {
                        println!("gate runs: orphaned pidfile removed: {name}");
                    }
                }
                if report.healthy() {
                    println!("doctor: healthy");
                } else {
                    return Err(UsageError::Message("doctor found problems".into()));
                }
            }
        }
        "certify" | "scaffold" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let report = if subcommand == "certify" {
                crate::init::certify(&cwd)?
            } else {
                crate::init::scaffold(&cwd)?
            };
            if parsed.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for check in &report.checks {
                    let tag = match check.status {
                        crate::init::CheckStatus::Pass => "pass",
                        crate::init::CheckStatus::Created => "created",
                        crate::init::CheckStatus::Warn => "warn",
                        crate::init::CheckStatus::Fail => "FAIL",
                        crate::init::CheckStatus::Skipped => "skip",
                    };
                    println!("{tag:<8} {:<28} {}", check.id, check.detail);
                }
                println!();
                if report.certified() {
                    if subcommand == "certify" {
                        println!("Certified (read-only — nothing written).");
                    } else {
                        println!(
                            "Scaffolding done — review the drafts, then run `aethyme certify`."
                        );
                    }
                } else {
                    return Err(UsageError::Message("FAIL items above must be fixed".into()));
                }
            }
            if !report.certified() {
                return Err(UsageError::Message("certification failed".into()));
            }
        }
        "close" => {
            let session = parsed
                .session
                .ok_or(UsageError::Message("close requires --session <id>".into()))?;
            let mut broker = open_broker()?;
            broker.close(session)?;
            if parsed.json {
                println!("{}", serde_json::json!({ "closed": session }));
            } else {
                println!(
                    "Session {session} closed (state only — worktree untouched). \
                     Follow-up task on the same worktree: `aethyme broker adopt --reuse --task \"...\"`."
                );
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
