//! `aethyme mcp connect <name>`: become the server.
//!
//! The resolved argv replaces this process (`exec`), so the agent's stdio is
//! the server's stdio and no extra process stays alive: the server keeps the
//! pid the agent spawned. Everything `connect` prints goes to stderr; stdout
//! belongs to the protocol.
//!
//! When the entry is missing or invalid, or resolution fails, `connect`
//! prints why and exits non-zero, unless `--fallback -- <command...>` was
//! given: then it execs that command instead, with the caller's environment,
//! so an agent is never blocked by `aethyme mcp`.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use super::checkouts::GitCheckouts;
use super::config::{self, Entry, Scope, Sources, Transport};
use super::key::{self, Caller, CopyKind, Plan};
use super::telemetry::{self, Event};
use crate::exit_status;

/// Parsed `connect` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectArgs {
    pub name: String,
    pub from_worktree: bool,
    pub fallback: Option<Vec<String>>,
    pub fallback_cwd: Option<PathBuf>,
}

/// `connect <name> [--from-worktree] [--fallback-cwd <dir>] [--fallback -- <command...>]`.
pub fn parse_args(args: &[String]) -> Result<ConnectArgs, String> {
    let mut name = None;
    let mut from_worktree = false;
    let mut fallback_requested = false;
    let mut fallback_cwd = None;
    let mut fallback = None;
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        match word {
            "--from-worktree" => from_worktree = true,
            "--fallback" => fallback_requested = true,
            "--fallback-cwd" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--fallback-cwd needs a directory".into());
                };
                fallback_cwd = Some(PathBuf::from(value));
            }
            "--" => {
                if !fallback_requested {
                    return Err("`--` is only accepted after --fallback".into());
                }
                let command = args[index + 1..].to_vec();
                if command.is_empty() {
                    return Err("--fallback -- needs a command".into());
                }
                fallback = Some(command);
                break;
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown option {option}"));
            }
            value => {
                if name.is_some() {
                    return Err(format!("unexpected argument {value}"));
                }
                name = Some(value.to_string());
            }
        }
        index += 1;
    }
    if fallback_requested && fallback.is_none() {
        return Err("--fallback needs `-- <command...>`".into());
    }
    let Some(name) = name else {
        return Err("connect needs an entry name".into());
    };
    Ok(ConnectArgs {
        name,
        from_worktree,
        fallback,
        fallback_cwd,
    })
}

/// The server's environment: the allowed names from the caller, then the env
/// file's assignments (which win).
pub fn server_environment(
    plan: &Plan,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = Vec::new();
    let mut set = |name: &str, value: OsString| {
        environment.retain(|(existing, _)| existing != name);
        environment.push((OsString::from(name), value));
    };
    for name in &plan.env_allow {
        if let Some(value) = lookup(name) {
            set(name, value);
        }
    }
    for (name, value) in &plan.env_file_values.0 {
        set(name, OsString::from(value));
    }
    environment
}

/// The command that starts the planned server.
pub fn server_command(plan: &Plan) -> Command {
    let argv = &plan.resolution.argv;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&plan.cwd)
        .env_clear()
        .envs(server_environment(plan, &caller_variable));
    command
}

fn caller_variable(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The stem under which an entry's stable resolution is remembered.
pub fn record_name(entry: &Entry) -> String {
    let safe = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    match (&entry.scope, &entry.repository) {
        (Scope::Repository, Some(anchor)) => {
            format!("repository-{}-{}", safe(&anchor.identity), entry.name)
        }
        _ => format!("host-{}", entry.name),
    }
}

struct Recorder {
    name: String,
    phases: std::collections::BTreeMap<String, u64>,
    started: Instant,
}

impl Recorder {
    fn record(&self, outcome: &str, reason: Option<&str>, plan: Option<&Plan>) {
        let Some(directory) = super::host_directory() else {
            return;
        };
        let mut phases = self.phases.clone();
        if let Some(plan) = plan {
            phases.extend(
                plan.phases_ms
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), *value)),
            );
        }
        phases.insert(
            "connect_total".into(),
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let event = Event {
            at_ms: telemetry::now_ms(),
            command: "connect".into(),
            name: self.name.clone(),
            outcome: outcome.into(),
            reason: reason.map(str::to_string),
            copy: plan.map(|plan| plan.copy.kind.as_str().to_string()),
            key: plan.map(|plan| plan.key_short.clone()),
            phases_ms: phases,
        };
        let _ = telemetry::append(&directory, &event);
    }
}

/// Run `connect`. Returns only when no process image replaced this one.
pub fn run(args: &[String]) -> u8 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("aethyme mcp connect: {message}");
            eprintln!("Run `aethyme mcp --help` for usage.");
            return exit_status::USAGE;
        }
    };
    let mut recorder = Recorder {
        name: parsed.name.clone(),
        phases: Default::default(),
        started: Instant::now(),
    };
    let caller = match Caller::current(parsed.from_worktree) {
        Ok(caller) => caller,
        Err(error) => {
            return fail(
                &parsed,
                &recorder,
                "caller_cwd_unavailable",
                &format!("the current directory is unavailable: {error}"),
                exit_status::ENVIRONMENT,
            );
        }
    };
    let checkouts = GitCheckouts;
    let phase = Instant::now();
    let loaded = config::load(&Sources::discover(&caller.cwd), &checkouts);
    recorder.phases.insert(
        "config_load".into(),
        u64::try_from(phase.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    let Some(entry) = loaded.find(&parsed.name) else {
        let errors: Vec<String> = loaded
            .errors_for(&parsed.name)
            .map(ToString::to_string)
            .collect();
        let (code, message) = if errors.is_empty() {
            let files: Vec<String> = loaded
                .files
                .iter()
                .map(|file| file.path.display().to_string())
                .collect();
            (
                "unknown_entry",
                format!(
                    "no MCP entry named {:?} (looked in: {})",
                    parsed.name,
                    if files.is_empty() {
                        "no configuration file could be located".to_string()
                    } else {
                        files.join(", ")
                    }
                ),
            )
        } else {
            ("invalid_entry", errors.join("; "))
        };
        return fail(&parsed, &recorder, code, &message, exit_status::REFUSED);
    };
    if entry.transport == Transport::Http {
        return fail(
            &parsed,
            &recorder,
            "unsupported_transport",
            "transport = \"http\" needs the shared hub, which this build does not have",
            exit_status::REFUSED,
        );
    }
    let plan = match key::plan(entry, &caller, &checkouts) {
        Ok(plan) => plan,
        Err(failure) => {
            recorder.phases.extend(
                failure
                    .phases_ms
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), *value)),
            );
            return fail(
                &parsed,
                &recorder,
                failure.code,
                &failure.message,
                exit_status::ENVIRONMENT,
            );
        }
    };
    if !plan.resolution.resolved {
        let reason = plan.resolution.reason.unwrap_or("unresolved");
        let message = plan.resolution.message.clone().unwrap_or_default();
        recorder.phases.extend(
            plan.phases_ms
                .iter()
                .map(|(name, value)| ((*name).to_string(), *value)),
        );
        return fail(
            &parsed,
            &recorder,
            reason,
            &message,
            exit_status::ENVIRONMENT,
        );
    }
    eprintln!(
        "aethyme mcp: using {} copy, key {}",
        plan.copy.kind.as_str(),
        plan.key_short
    );
    if plan.copy.kind == CopyKind::Stable
        && let (Some(directory), Some(identity)) =
            (super::host_directory(), &plan.resolution.identity)
    {
        let change = telemetry::compare_and_remember(
            &directory,
            &record_name(entry),
            &plan.resolution.argv,
            identity,
        );
        if !change.changes.is_empty() {
            eprintln!(
                "aethyme mcp: {}: the executable changed since it was last resolved ({})",
                plan.name,
                change.changes.join(", ")
            );
        }
    }
    recorder.record("exec", None, Some(&plan));
    let error = server_command(&plan).exec();
    // Only reached when exec failed.
    fail(
        &parsed,
        &recorder,
        "exec_failed",
        &format!("could not start {}: {error}", plan.resolution.argv[0]),
        exit_status::ENVIRONMENT,
    )
}

/// Report a failure; run the fallback when one was given.
fn fail(parsed: &ConnectArgs, recorder: &Recorder, reason: &str, message: &str, code: u8) -> u8 {
    eprintln!("aethyme mcp: {}: {message}", parsed.name);
    let Some(fallback) = &parsed.fallback else {
        recorder.record("error", Some(reason), None);
        return code;
    };
    eprintln!("aethyme mcp: {}: running the fallback command", parsed.name);
    recorder.record("fallback", Some(reason), None);
    let mut command = Command::new(&fallback[0]);
    command.args(&fallback[1..]);
    if let Some(directory) = &parsed.fallback_cwd {
        command.current_dir(directory);
    }
    let error = command.exec();
    eprintln!(
        "aethyme mcp: {}: the fallback command {} could not start: {error}",
        parsed.name, fallback[0]
    );
    exit_status::ENVIRONMENT
}
