//! Command telemetry: the allowlisted command label and the outcome and timing records.

/// Safe-by-construction command telemetry: the label is built ONLY from
/// an allowlist of known subcommand words, so positional values (paths,
/// session ids, task text) can never leak into the metrics file. Best
/// effort — any failure is silently ignored.
pub(super) const KNOWN_COMMAND_WORDS: &[&str] = &[
    "adopt",
    "start",
    "start-agent",
    "worktree-root",
    "exec",
    "git",
    "gh",
    "operations",
    "stats",
    "blockers",
    "unblock",
    "advisories",
    "exposures",
    "note",
    "send",
    "list",
    "ack",
    "reconcile",
    "agents",
    "leases",
    "export",
    "resources",
    "console",
    "prepare",
    "claim",
    "release",
    "gates",
    "draft",
    "validate",
    "manifest",
    "scope",
    "affected",
    "semantic",
    "run",
    "pre-push",
    "hooks",
    "install",
    "uninstall",
    "pre-commit",
    "post-commit",
    "pr",
    "check",
    "submit",
    "repair",
    "checkpoint",
    "promotion-record",
    "representation",
    "scan",
    "record",
    "main",
    "apply",
    "queue",
    "promote",
    "ship",
    "plan",
    "execute",
    "sync-main",
    "sync-integration",
    "no-cache",
    "integration",
    "status",
    "events",
    "prune",
    "metrics",
    "doctor",
    "quick-test",
    "trust",
    "verify-loop",
    "e2e",
    "finish",
    "handoff",
    "report",
    "external-events",
    "deliveries",
    "subscribe",
    "complete",
    "review",
    "register",
    "request",
    "unlock",
    "reassign",
    "abandon",
    "ingest",
    "capture",
    "cleanup",
    "gc",
    "storage",
    "certify",
    "readiness",
    "scaffold",
    "init",
];

pub(super) fn safe_command_surface(args: &[String]) -> Option<String> {
    let first = args.first()?.as_str();
    if !KNOWN_COMMAND_WORDS.contains(&first) {
        return None;
    }
    let mut words = vec![first];
    if let Some(second) = args.get(1).map(String::as_str)
        && KNOWN_COMMAND_WORDS.contains(&second)
    {
        words.push(second);
    }
    Some(words.join("."))
}

pub(super) fn record_command_outcome(args: &[String], exit: u8) {
    if !command_records_metric(args) {
        return;
    }
    let Some(surface) = safe_command_surface(args) else {
        return;
    };
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    // `open_current_in_repo`, not `open_in_repo`: this is a metric, and a
    // metric may not create a repository's broker state or migrate it. The
    // repository here came from the process working directory, which for a
    // spawned test binary is a checkout nobody asked this command to touch
    // (#163).
    let Ok(Some(mut store)) = crate::BrokerStore::open_current_in_repo(&main_root) else {
        return;
    };
    let explicit_session = args
        .windows(2)
        .find(|pair| pair[0] == "--session")
        .and_then(|pair| pair[1].parse::<i64>().ok());
    let session_id = explicit_session.or_else(|| {
        store
            .session_for_worktree(repo.root().to_string_lossy().as_ref())
            .ok()
            .flatten()
            .map(|session| session.id)
    });
    let command_surface = format!("broker.{surface}");
    let failure_class = (exit != 0).then_some(match args.first().map(String::as_str) {
        Some("submit") => "submission_failed",
        Some("repair") => "recovery_failed",
        Some("git" | "gh") => "coordinated_operation_failed",
        _ => "command_failed",
    });
    let payload = crate::events::broker_command_outcome_payload(
        &command_surface,
        exit,
        failure_class,
        None,
        None,
    );
    let kind = if exit == 0 {
        crate::events::BROKER_COMMAND_SUCCEEDED
    } else {
        crate::events::BROKER_COMMAND_FAILED
    };
    crate::warn_unrecorded(
        "record the command outcome event",
        store.append_event(kind, session_id, Some(&payload)),
    );
}

/// Record one command's cost. `output_bytes` counts stdout emitted through
/// `out!`; stderr is excluded because it carries diagnostics rather than the
/// payload an agent pays to read.
pub(super) fn record_command_metric(args: &[String], exit: u8, duration_ms: i64) {
    let output_bytes = crate::cli_output::emitted();
    // Inspection commands are contractually side-effect free: the CLI documents
    // "never writes broker state or command telemetry" and `external_events_cli`
    // asserts the metrics file is byte-identical across them. That invariant
    // also hides the commands that dominate agent token cost, because reads are
    // the frequent, expensive ones. Measuring them is therefore opt-in: unset,
    // nothing changes; set, the operator has accepted that inspection now
    // writes one telemetry line.
    if !command_records_metric(args) && !output_measurement_opted_in() {
        return;
    }
    let Some(label) = safe_command_surface(args) else {
        return;
    };
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    // Opt-in latency telemetry, not broker state: a command must never fail
    // or grow stderr noise because its own timing line could not be written.
    let dir = main_root.join(".aethyme/logs");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let line = format!(
        "{{\"ts\":{ts},\"command\":\"{}\",\"duration_ms\":{duration_ms},\"exit\":{exit},\"output_bytes\":{output_bytes}}}\n",
        label
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

/// Whether this invocation should contribute command-latency telemetry.
/// Report-only commands stay telemetry-free; variants that mutate broker,
/// repository, or installation state remain observable.
/// Whether the operator opted into measuring read-only command output.
///
/// Off by default so inspection stays side-effect free. `AETHYME_MEASURE_OUTPUT`
/// is read per invocation rather than cached, so enabling it needs no restart of
/// anything and a wrapper can scope it to a single command.
pub(super) fn output_measurement_opted_in() -> bool {
    std::env::var_os("AETHYME_MEASURE_OUTPUT")
        .map(|value| {
            let value = value.to_string_lossy().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        })
        .unwrap_or(false)
}

pub(super) fn command_records_metric(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("certify" | "readiness" | "queue" | "metrics" | "handoff" | "worktree-root") => false,
        Some("advisories") => matches!(args.get(1).map(String::as_str), Some("ack" | "suppress")),
        Some("exposures") => args.get(1).map(String::as_str) == Some("apply"),
        Some("report") => args.get(1).map(String::as_str) == Some("file"),
        Some("quality-report") => args.get(1).map(String::as_str) != Some("plan"),
        Some("external-events") => matches!(
            args.get(1).map(String::as_str),
            Some("ingest" | "reconcile")
        ),
        Some("review") => !matches!(args.get(1).map(String::as_str), Some("show")),
        Some("ship") => args.get(1).map(String::as_str) != Some("plan"),
        Some("checkpoint") => args.get(1).map(String::as_str) == Some("apply"),
        Some("gc") => args.get(1).map(String::as_str) == Some("apply"),
        Some("storage") => args.get(1).map(String::as_str) == Some("apply"),
        Some("representation") => args.get(1).map(String::as_str) == Some("record"),
        Some("operations") => args.get(1).map(String::as_str) == Some("reconcile"),
        Some("git" | "gh") => {
            let command = args
                .iter()
                .position(|arg| arg == "--")
                .map(|index| &args[index + 1..])
                .unwrap_or(&[]);
            let effect = if args.first().map(String::as_str) == Some("git") {
                crate::classify_git(command)
            } else {
                crate::classify_gh(command)
            };
            effect != Some(crate::OperationEffect::Read)
        }
        Some("hooks") => !matches!(args.get(1).map(String::as_str), Some("status" | "snippet")),
        Some("leases") => !matches!(args.get(1).map(String::as_str), Some("plan" | "export")),
        Some("console") => args.get(1).map(String::as_str) == Some("run"),
        Some("resources") => !matches!(
            args.get(1).map(String::as_str),
            Some("plan" | "explain" | "list")
        ),
        Some("events") => args.get(1).map(String::as_str) == Some("prune"),
        Some("gates") => match args.get(1).map(String::as_str) {
            Some("validate" | "manifest" | "scope" | "affected" | "semantic") => false,
            Some("doctor") => args.iter().any(|arg| arg == "--probe"),
            _ => true,
        },
        Some("doctor") => args.iter().any(|arg| arg == "--fix-version"),
        Some("trust") => args.get(1).map(String::as_str) != Some("status"),
        Some("blockers") => false,
        _ => true,
    }
}
