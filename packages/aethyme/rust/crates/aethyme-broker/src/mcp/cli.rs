//! `aethyme mcp`: the command group.
//!
//! `--json` prints one object whose `schema` names its shape
//! (`aethyme.mcp-<command>/v1`); a refusal is `aethyme.mcp-error/v1` with
//! `code` and `message`. `connect` has no JSON form: its stdout is the
//! server's.

use std::time::Instant;

use serde_json::{Value, json};

use super::checkouts::GitCheckouts;
use super::config::{self, Entry, Loaded, Sources};
use super::connect;
use super::key::{self, Caller, CopyKind};
use super::telemetry::{self, Event};
use crate::exit_status;

pub const RESOLVE_SCHEMA: &str = "aethyme.mcp-resolve/v1";
pub const STATUS_SCHEMA: &str = "aethyme.mcp-status/v1";
pub const ERROR_SCHEMA: &str = "aethyme.mcp-error/v1";

pub const USAGE: &str = "\
aethyme mcp — start MCP servers without launcher chains (opt-in)

Entries live in <host state>/mcp/services.toml and, per repository, in
.aethyme/mcp.toml (schema = 1, one [[servers]] table each): name, mode
(shared|direct), transport (stdio|http), argv, cwd, env_file, env_allow,
source, flavour. Paths must name stable copies: an entry pointing into a
broker session worktree is refused, because sessions are cleaned up.
Nothing is written to any agent configuration; the owner pastes the
snippet `config print` shows.

Usage:
  aethyme mcp resolve <name> [--from-worktree] [--json]
      Resolve the entry's launcher chain (pnpm exec, npm exec, npx) to the
      installed executable and print the direct argv, the executable's
      identity (path, size, mtime, script interpreter), the chosen copy and
      the key. Runs nothing and never downloads: when no installed copy is
      found offline it says so, keeps the declared argv and exits 6. A
      changed executable since the last resolution is reported.
  aethyme mcp connect <name> [--from-worktree] [--fallback-cwd <dir>]
                      [--fallback -- <command...>]
      Replace this process with the resolved server (exec): stdio passes
      straight through and no extra process remains. Inside a broker session
      worktree it uses that worktree's copy when its `source` differs from
      the stable copy, or with --from-worktree; otherwise the stable copy.
      It prints `aethyme mcp: using <stable|worktree> copy, key <short>` on
      stderr. A missing entry or a failed resolution exits non-zero, unless
      --fallback is given: then the fallback command runs instead (in
      --fallback-cwd when given), so an agent is never blocked.
  aethyme mcp status [--json]
      Every entry with its resolution, identity, chosen copy and key,
      refused entries with the reason, and per-entry counters from
      <host state>/mcp/events.jsonl (outcomes, failure reasons, resolve
      time). Writes nothing.
  aethyme mcp config print --agent codex|claude [--name <name>]
      Print the configuration snippet for each entry (or one) in that
      agent's format: `aethyme mcp connect <name> --fallback -- <argv>`.
      Prints only; never writes a file.
";

fn json_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--json")
}

fn report_error(json: bool, code: &str, message: &str, exit: u8) -> u8 {
    if json {
        println!(
            "{}",
            json!({"schema": ERROR_SCHEMA, "code": code, "message": message})
        );
    } else {
        eprintln!("aethyme mcp: {message}");
    }
    exit
}

pub fn run(args: &[String]) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return exit_status::USAGE;
    };
    // Help anywhere before `--` answers help and does nothing else, so the
    // router can pass any `mcp ... --help` line through unchanged.
    if command == "help"
        || args
            .iter()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "-h" || arg == "--help")
    {
        print!("{USAGE}");
        return exit_status::SUCCESS;
    }
    match command.as_str() {
        "connect" => connect::run(rest),
        "resolve" => run_resolve(rest),
        "status" => run_status(rest),
        "config" => match rest.split_first() {
            Some((sub, tail)) if sub == "print" => run_config_print(tail),
            _ => report_error(
                json_requested(rest),
                "usage",
                "use `aethyme mcp config print --agent codex|claude`",
                exit_status::USAGE,
            ),
        },
        other => report_error(
            json_requested(rest),
            "usage",
            &format!("unknown subcommand {other:?}; see `aethyme mcp --help`"),
            exit_status::USAGE,
        ),
    }
}

struct Options {
    json: bool,
    from_worktree: bool,
    name: Option<String>,
    agent: Option<String>,
}

fn parse_options(args: &[String], allow: &[&str]) -> Result<Options, String> {
    let mut options = Options {
        json: false,
        from_worktree: false,
        name: None,
        agent: None,
    };
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        let take_value = |index: &mut usize| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("{word} needs a value"))
        };
        match word {
            "--json" if allow.contains(&"--json") => options.json = true,
            "--from-worktree" if allow.contains(&"--from-worktree") => options.from_worktree = true,
            "--name" if allow.contains(&"--name") => options.name = Some(take_value(&mut index)?),
            "--agent" if allow.contains(&"--agent") => {
                options.agent = Some(take_value(&mut index)?);
            }
            option if option.starts_with('-') => return Err(format!("unknown option {option}")),
            value if allow.contains(&"<name>") && options.name.is_none() => {
                options.name = Some(value.to_string());
            }
            value => return Err(format!("unexpected argument {value}")),
        }
        index += 1;
    }
    Ok(options)
}

fn load_for(caller: &Caller) -> Loaded {
    config::load(&Sources::discover(&caller.cwd), &GitCheckouts)
}

fn run_resolve(args: &[String]) -> u8 {
    let started = Instant::now();
    let options = match parse_options(args, &["--json", "--from-worktree", "<name>"]) {
        Ok(options) => options,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let Some(name) = options.name.clone() else {
        return report_error(
            options.json,
            "usage",
            "resolve needs an entry name",
            exit_status::USAGE,
        );
    };
    let caller = match Caller::current(options.from_worktree) {
        Ok(caller) => caller,
        Err(error) => {
            return report_error(
                options.json,
                "caller_cwd_unavailable",
                &format!("the current directory is unavailable: {error}"),
                exit_status::ENVIRONMENT,
            );
        }
    };
    let phase = Instant::now();
    let loaded = load_for(&caller);
    let config_ms = elapsed_ms(phase);
    let Some(entry) = loaded.find(&name) else {
        let errors: Vec<String> = loaded.errors_for(&name).map(ToString::to_string).collect();
        let (code, message) = if errors.is_empty() {
            ("unknown_entry", format!("no MCP entry named {name:?}"))
        } else {
            ("invalid_entry", errors.join("; "))
        };
        record_resolve(&name, "error", Some(code), None, config_ms, started);
        return report_error(options.json, code, &message, exit_status::REFUSED);
    };
    let plan = match key::plan(entry, &caller, &GitCheckouts) {
        Ok(plan) => plan,
        Err(failure) => {
            record_resolve(&name, "error", Some(failure.code), None, config_ms, started);
            return report_error(
                options.json,
                failure.code,
                &failure.message,
                exit_status::ENVIRONMENT,
            );
        }
    };
    let change = match (
        &plan.copy.kind,
        &plan.resolution.identity,
        super::host_directory(),
    ) {
        (CopyKind::Stable, Some(identity), Some(directory)) => {
            Some(telemetry::compare_and_remember(
                &directory,
                &connect::record_name(entry),
                &plan.resolution.argv,
                identity,
            ))
        }
        _ => None,
    };
    let outcome = if plan.resolution.resolved {
        "resolved"
    } else {
        "unresolved"
    };
    record_resolve(
        &name,
        outcome,
        plan.resolution.reason,
        Some(&plan),
        config_ms,
        started,
    );
    if options.json {
        let mut value = serde_json::to_value(&plan).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("schema".into(), json!(RESOLVE_SCHEMA));
            map.insert("changed_since_last".into(), json!(change));
            if let Some(Value::Object(phases)) = map.get_mut("phases_ms") {
                phases.insert("config_load".into(), json!(config_ms));
            }
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        print_plan_text(entry, &plan);
        if let Some(change) = &change
            && !change.changes.is_empty()
        {
            println!(
                "  changed since last resolution: {}",
                change.changes.join(", ")
            );
        }
    }
    if plan.resolution.resolved {
        exit_status::SUCCESS
    } else {
        exit_status::ENVIRONMENT
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn record_resolve(
    name: &str,
    outcome: &str,
    reason: Option<&str>,
    plan: Option<&key::Plan>,
    config_ms: u64,
    started: Instant,
) {
    let Some(directory) = super::host_directory() else {
        return;
    };
    let mut phases: std::collections::BTreeMap<String, u64> = plan
        .map(|plan| {
            plan.phases_ms
                .iter()
                .map(|(name, value)| ((*name).to_string(), *value))
                .collect()
        })
        .unwrap_or_default();
    phases.insert("config_load".into(), config_ms);
    phases.insert("command_total".into(), elapsed_ms(started));
    let _ = telemetry::append(
        &directory,
        &Event {
            at_ms: telemetry::now_ms(),
            command: "resolve".into(),
            name: name.to_string(),
            outcome: outcome.into(),
            reason: reason.map(str::to_string),
            copy: plan.map(|plan| plan.copy.kind.as_str().to_string()),
            key: plan.map(|plan| plan.key_short.clone()),
            phases_ms: phases,
        },
    );
}

fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|word| {
            if !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+%".contains(c))
            {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn print_plan_text(entry: &Entry, plan: &key::Plan) {
    let flavour = if plan.flavour.is_empty() {
        String::new()
    } else {
        format!(", flavour {}", plan.flavour)
    };
    println!(
        "{} ({}, {}, {}{flavour})",
        plan.name,
        scope_name(entry),
        serde_name(&plan.mode),
        serde_name(&plan.transport)
    );
    println!(
        "  copy: {} ({}), key {}",
        plan.copy.kind.as_str(),
        plan.copy.reason,
        plan.key_short
    );
    println!("  cwd: {}", plan.cwd.display());
    println!("  declared: {}", shell_words(&plan.resolution.declared));
    if plan.resolution.resolved {
        println!(
            "  resolved: {}  [{}]",
            shell_words(&plan.resolution.argv),
            serde_name(&plan.resolution.method)
        );
        if let Some(identity) = &plan.resolution.identity {
            println!(
                "  executable: {} ({} bytes)",
                identity.file.path.display(),
                identity.file.size
            );
            if let Some(shebang) = &identity.shebang {
                println!("  interpreter: #!{shebang}");
            }
        }
    } else {
        println!(
            "  resolved: no ({}): {}",
            plan.resolution.reason.unwrap_or("unresolved"),
            plan.resolution.message.as_deref().unwrap_or_default()
        );
    }
    for note in &plan.resolution.notes {
        println!("  note: {note}");
    }
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn scope_name(entry: &Entry) -> String {
    serde_name(&entry.scope)
}

fn run_status(args: &[String]) -> u8 {
    let started = Instant::now();
    let options = match parse_options(args, &["--json"]) {
        Ok(options) => options,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let caller = match Caller::current(false) {
        Ok(caller) => caller,
        Err(error) => {
            return report_error(
                options.json,
                "caller_cwd_unavailable",
                &format!("the current directory is unavailable: {error}"),
                exit_status::ENVIRONMENT,
            );
        }
    };
    let phase = Instant::now();
    let loaded = load_for(&caller);
    let config_ms = elapsed_ms(phase);
    let directory = super::host_directory();
    let counters = directory
        .as_deref()
        .map(|directory| telemetry::summarize(&telemetry::read_events(directory)))
        .unwrap_or_default();
    let phase = Instant::now();
    let plans: Vec<(&Entry, Result<key::Plan, Box<key::PlanFailure>>)> = loaded
        .entries
        .iter()
        .map(|entry| (entry, key::plan(entry, &caller, &GitCheckouts)))
        .collect();
    let plan_ms = elapsed_ms(phase);
    if options.json {
        let entries: Vec<Value> = plans
            .iter()
            .map(|(entry, plan)| {
                json!({
                    "name": entry.name,
                    "scope": entry.scope,
                    "mode": entry.mode,
                    "transport": entry.transport,
                    "flavour": entry.flavour,
                    "config_path": entry.config_path,
                    "plan": plan.as_ref().ok(),
                    "plan_error": plan.as_ref().err(),
                    "counters": counters.get(&entry.name),
                    "hub": Value::Null,
                })
            })
            .collect();
        let value = json!({
            "schema": STATUS_SCHEMA,
            "files": loaded.files,
            "errors": loaded.errors,
            "entries": entries,
            "hub": {
                "state": "not_available",
                "detail": "this build has no shared hub; every connect runs its own server",
            },
            "events_file": directory.as_ref().map(|directory| directory.join(telemetry::EVENTS_FILE)),
            "phases_ms": {
                "config_load": config_ms,
                "plans": plan_ms,
                "total": elapsed_ms(started),
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return exit_status::SUCCESS;
    }
    for file in &loaded.files {
        println!(
            "{} file: {} ({})",
            serde_name(&file.scope),
            file.path.display(),
            if file.present { "present" } else { "absent" }
        );
    }
    if loaded.entries.is_empty() && loaded.errors.is_empty() {
        println!("No MCP entries are configured.");
    }
    for (entry, plan) in &plans {
        match plan {
            Ok(plan) => print_plan_text(entry, plan),
            Err(failure) => println!(
                "{} ({}): cannot plan: {} ({})",
                entry.name,
                scope_name(entry),
                failure.message,
                failure.code
            ),
        }
        if let Some(counters) = counters.get(&entry.name) {
            let outcomes: Vec<String> = counters
                .outcomes
                .iter()
                .map(|(outcome, count)| format!("{outcome} {count}"))
                .collect();
            println!("  recorded: {}", outcomes.join(", "));
            if !counters.failure_reasons.is_empty() {
                let reasons: Vec<String> = counters
                    .failure_reasons
                    .iter()
                    .map(|(reason, count)| format!("{reason} {count}"))
                    .collect();
                println!("  failure reasons: {}", reasons.join(", "));
            }
        }
    }
    for error in &loaded.errors {
        println!("refused: {error}");
    }
    println!("hub: not available in this build; every connect runs its own server");
    exit_status::SUCCESS
}

/// The `args` an agent configuration passes to `aethyme`.
pub fn connect_args(entry: &Entry) -> Vec<String> {
    let mut args = vec!["mcp".to_string(), "connect".to_string(), entry.name.clone()];
    if let Some(cwd) = &entry.cwd {
        args.push("--fallback-cwd".into());
        args.push(cwd.to_string_lossy().into_owned());
    }
    args.push("--fallback".into());
    args.push("--".into());
    args.extend(entry.argv.iter().cloned());
    args
}

/// The snippet for `agent`, or why there is none.
pub fn config_snippet(agent: &str, entries: &[&Entry]) -> Result<String, String> {
    match agent {
        "codex" => {
            let mut servers = toml::Table::new();
            for entry in entries {
                let mut server = toml::Table::new();
                server.insert("command".into(), toml::Value::String("aethyme".into()));
                server.insert(
                    "args".into(),
                    toml::Value::Array(
                        connect_args(entry)
                            .into_iter()
                            .map(toml::Value::String)
                            .collect(),
                    ),
                );
                servers.insert(entry.name.clone(), toml::Value::Table(server));
            }
            let mut root = toml::Table::new();
            root.insert("mcp_servers".into(), toml::Value::Table(servers));
            toml::to_string(&root).map_err(|error| error.to_string())
        }
        "claude" => {
            let mut servers = serde_json::Map::new();
            for entry in entries {
                servers.insert(
                    entry.name.clone(),
                    json!({
                        "type": "stdio",
                        "command": "aethyme",
                        "args": connect_args(entry),
                    }),
                );
            }
            serde_json::to_string_pretty(&json!({ "mcpServers": servers }))
                .map(|text| text + "\n")
                .map_err(|error| error.to_string())
        }
        other => Err(format!("unknown agent {other:?}; use codex or claude")),
    }
}

fn run_config_print(args: &[String]) -> u8 {
    let options = match parse_options(args, &["--agent", "--name"]) {
        Ok(options) => options,
        Err(message) => return report_error(false, "usage", &message, exit_status::USAGE),
    };
    let Some(agent) = options.agent.as_deref() else {
        return report_error(
            false,
            "usage",
            "config print needs --agent codex|claude",
            exit_status::USAGE,
        );
    };
    let caller = match Caller::current(false) {
        Ok(caller) => caller,
        Err(error) => {
            return report_error(
                false,
                "caller_cwd_unavailable",
                &format!("the current directory is unavailable: {error}"),
                exit_status::ENVIRONMENT,
            );
        }
    };
    let loaded = load_for(&caller);
    let mut entries: Vec<&Entry> = Vec::new();
    for entry in &loaded.entries {
        let wanted = options
            .name
            .as_deref()
            .is_none_or(|name| name == entry.name);
        // The repository entry shadows a host entry of the same name.
        if wanted
            && loaded
                .find(&entry.name)
                .is_some_and(|found| std::ptr::eq(found, entry))
        {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        let message = match &options.name {
            Some(name) => format!("no valid MCP entry named {name:?}"),
            None => "no valid MCP entries are configured".to_string(),
        };
        for error in &loaded.errors {
            eprintln!("refused: {error}");
        }
        return report_error(false, "unknown_entry", &message, exit_status::REFUSED);
    }
    match config_snippet(agent, &entries) {
        Ok(snippet) => {
            print!("{snippet}");
            eprintln!(
                "aethyme mcp: printed only; paste it into the agent's configuration yourself. \
                 Keep any env settings the server already has there: they reach the fallback."
            );
            for entry in entries.iter().filter(|entry| entry.env_file.is_some()) {
                eprintln!(
                    "aethyme mcp: {}: the fallback runs the declared argv without env_file; \
                     replace it with your current command if that one loads the env file",
                    entry.name
                );
            }
            exit_status::SUCCESS
        }
        Err(message) => report_error(false, "usage", &message, exit_status::USAGE),
    }
}
