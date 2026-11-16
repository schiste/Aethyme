//! `aethyme mcp connect <name>`: become the server.
//!
//! The resolved argv replaces this process (`exec`), so the agent's stdio is
//! the server's stdio and no extra process stays alive: the server keeps the
//! pid the agent spawned. Everything `connect` prints goes to stderr; stdout
//! belongs to the protocol.
//!
//! When anything fails before the exec (a usage error, a missing, refused or
//! untrusted entry, a failed resolution or pre-flight check, even a panic),
//! `connect` prints why and exits non-zero, unless `--fallback -- <command...>`
//! was given: then it execs that command instead, with the caller's
//! environment, so an agent is never blocked by `aethyme mcp`. What can be
//! checked before the exec (the program and its interpreter exist and are
//! executable, a kept shell shim's `node` is on the server's `PATH`) is
//! checked, because after the exec no fallback is possible.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::checkouts::GitCheckouts;
use super::config::{self, Entry, Scope, Sources, Transport};
use super::key::{self, Caller, CopyKind, Plan};
use super::resolve::{is_executable_file, is_shell, read_shebang, search_path};
use super::telemetry::{self, Event};
use crate::exit_status;

/// Parsed `connect` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectArgs {
    pub name: String,
    pub from_worktree: bool,
    pub fallback: Option<Fallback>,
}

/// The command to run when `connect` cannot start the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Find `--fallback ... -- <command...>` without parsing anything else, so a
/// usage error elsewhere on the line still runs the fallback.
pub fn scan_fallback(args: &[String]) -> Option<Fallback> {
    let separator = args.iter().position(|arg| arg == "--")?;
    let options = &args[..separator];
    if !options.iter().any(|arg| arg == "--fallback") {
        return None;
    }
    let command = args[separator + 1..].to_vec();
    if command.is_empty() {
        return None;
    }
    let cwd = options
        .iter()
        .position(|arg| arg == "--fallback-cwd")
        .and_then(|index| options.get(index + 1))
        .map(PathBuf::from);
    Some(Fallback { command, cwd })
}

fn entry_name(args: &[String]) -> String {
    let mut words = args.iter().take_while(|arg| *arg != "--");
    let mut name = String::new();
    while let Some(word) = words.next() {
        if word == "--fallback-cwd" {
            words.next();
        } else if !word.starts_with('-') {
            name = word.clone();
            break;
        }
    }
    name
}

/// `connect <name> [--from-worktree] [--fallback-cwd <dir>] [--fallback -- <command...>]`.
pub fn parse_args(args: &[String]) -> Result<ConnectArgs, String> {
    let mut name = None;
    let mut from_worktree = false;
    let mut fallback_requested = false;
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        match word {
            "--from-worktree" => from_worktree = true,
            "--fallback" => fallback_requested = true,
            "--fallback-cwd" => {
                index += 1;
                if args.get(index).is_none() {
                    return Err("--fallback-cwd needs a directory".into());
                }
            }
            "--" => {
                if !fallback_requested {
                    return Err("`--` is only accepted after --fallback".into());
                }
                if args.len() == index + 1 {
                    return Err("--fallback -- needs a command".into());
                }
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
    let fallback = scan_fallback(args);
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
    })
}

/// The command that starts the planned server.
pub fn server_command(plan: &Plan) -> Command {
    let argv = &plan.resolution.argv;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&plan.cwd)
        .env_clear()
        .envs(plan.environment.0.iter().cloned());
    command
}

/// Checks that can fail now, while the fallback is still possible.
pub fn preflight(plan: &Plan) -> Result<(), String> {
    let program = Path::new(&plan.resolution.argv[0]);
    if !is_executable_file(program) {
        return Err(format!("{} is not an executable file", program.display()));
    }
    if let Some(interpreter) = plan
        .resolution
        .identity
        .as_ref()
        .and_then(|identity| identity.interpreter.as_ref())
        && !is_executable_file(&interpreter.path)
    {
        return Err(format!(
            "the interpreter {} is not executable",
            interpreter.path.display()
        ));
    }
    // A shell shim that was kept runs `node` from the server's PATH.
    let shell_script = read_shebang(program)
        .is_some_and(|line| line.split_whitespace().next().is_some_and(is_shell));
    if shell_script
        && std::fs::read_to_string(program).is_ok_and(|text| text.contains("exec node "))
    {
        let local = program.parent().map(|dir| dir.join("node"));
        let path = plan
            .environment
            .0
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone());
        let found = local.as_deref().is_some_and(is_executable_file)
            || search_path("node", path.as_deref()).is_some();
        if !found {
            return Err(format!(
                "{} runs `node`, which is not on the server's PATH (env_inherit = \"none\" \
                 without PATH in env_allow?)",
                program.display()
            ));
        }
    }
    Ok(())
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
    shadowed: bool,
}

impl Recorder {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            phases: Default::default(),
            started: Instant::now(),
            shadowed: false,
        }
    }

    fn record(&self, outcome: &str, reason: Option<&str>, plan: Option<&Plan>, fallback: bool) {
        let Some(directory) = super::host_directory() else {
            return;
        };
        let mut phases = self.phases.clone();
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
            shadowed: self.shadowed,
            fallback,
        };
        crate::warn_unrecorded(
            "record the MCP connect event",
            telemetry::append(&directory, &event),
        );
    }
}

/// Run `connect`. Returns only when no process image replaced this one. A
/// panic anywhere before the exec still runs the fallback.
pub fn run(args: &[String]) -> u8 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_inner(args))) {
        Ok(code) => code,
        Err(_) => {
            let name = entry_name(args);
            eprintln!("aethyme mcp: {name}: internal error before the server started");
            match scan_fallback(args) {
                Some(fallback) => {
                    Recorder::new(&name).record("fallback", Some("panic"), None, true);
                    exec_fallback(&name, &fallback)
                }
                None => exit_status::FAILED,
            }
        }
    }
}

/// Test-only seam: panic inside `connect` to prove the fallback still runs.
/// Honoured only in debug builds started by the test harness.
fn injected_panic() -> bool {
    cfg!(debug_assertions)
        && ["CARGO_MANIFEST_DIR", "CARGO_BIN_EXE_aethyme"]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        && std::env::var_os("AETHYME_TEST_MCP_CONNECT_PANIC").is_some()
}

fn run_inner(args: &[String]) -> u8 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("aethyme mcp connect: {message}");
            if let Some(fallback) = scan_fallback(args) {
                let name = entry_name(args);
                Recorder::new(&name).record("fallback", Some("usage"), None, true);
                return exec_fallback(&name, &fallback);
            }
            eprintln!("Run `aethyme mcp --help` for usage.");
            return exit_status::USAGE;
        }
    };
    assert!(!injected_panic(), "injected connect panic (test seam)");
    let mut recorder = Recorder::new(&parsed.name);
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
    // Only the requested entry is validated.
    let loaded = config::load(
        &Sources::discover(&caller.cwd),
        &checkouts,
        Some(parsed.name.as_str()),
    );
    recorder.phases.insert(
        "config_load".into(),
        u64::try_from(phase.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    if loaded.shadowed.contains(&parsed.name) {
        recorder.shadowed = true;
        eprintln!(
            "aethyme mcp: {}: the repository's .aethyme/mcp.toml also defines this name; it is \
             ignored and the host entry is used",
            parsed.name
        );
    }
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
    if !entry.trusted {
        return fail(
            &parsed,
            &recorder,
            "untrusted_repository_entry",
            &format!(
                "{} is not trusted on this host; review it and run `aethyme mcp trust` in that \
                 repository (any change to the file needs trust again)",
                entry.config_path.display()
            ),
            exit_status::REFUSED,
        );
    }
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
    recorder.phases.extend(
        plan.phases_ms
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value)),
    );
    if !plan.resolution.resolved {
        let reason = plan.resolution.reason.unwrap_or("unresolved");
        let message = plan.resolution.message.clone().unwrap_or_default();
        return fail(
            &parsed,
            &recorder,
            reason,
            &message,
            exit_status::ENVIRONMENT,
        );
    }
    if let Err(message) = preflight(&plan) {
        return fail(
            &parsed,
            &recorder,
            "preflight_failed",
            &message,
            exit_status::ENVIRONMENT,
        );
    }
    for note in &plan.notes {
        eprintln!("aethyme mcp: {}: {note}", plan.name);
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
    recorder.record("exec", None, Some(&plan), false);
    let error = server_command(&plan).exec();
    // Only reached when exec failed: one `exec_failed` event corrects the
    // `exec` above (status counts it once, as a failure).
    eprintln!(
        "aethyme mcp: {}: could not start {}: {error}",
        parsed.name, plan.resolution.argv[0]
    );
    recorder.record(
        "exec_failed",
        Some("exec_failed"),
        Some(&plan),
        parsed.fallback.is_some(),
    );
    match &parsed.fallback {
        Some(fallback) => exec_fallback(&parsed.name, fallback),
        None => exit_status::ENVIRONMENT,
    }
}

/// Report a failure; run the fallback when one was given.
fn fail(parsed: &ConnectArgs, recorder: &Recorder, reason: &str, message: &str, code: u8) -> u8 {
    eprintln!("aethyme mcp: {}: {message}", parsed.name);
    let Some(fallback) = &parsed.fallback else {
        recorder.record("error", Some(reason), None, false);
        return code;
    };
    recorder.record("fallback", Some(reason), None, true);
    exec_fallback(&parsed.name, fallback)
}

/// Exec the fallback command with the caller's environment.
fn exec_fallback(name: &str, fallback: &Fallback) -> u8 {
    eprintln!("aethyme mcp: {name}: running the fallback command");
    let mut command = Command::new(&fallback.command[0]);
    command.args(&fallback.command[1..]);
    if let Some(directory) = &fallback.cwd {
        command.current_dir(directory);
    }
    let error = command.exec();
    eprintln!(
        "aethyme mcp: {name}: the fallback command {} could not start: {error}",
        fallback.command[0]
    );
    exit_status::ENVIRONMENT
}
