//! The managed-server commands: `manage`, `intercept`, `repair`, `bypass`,
//! and the `managed` section of `status --json`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use super::bypass;
use super::config::{self, Loaded, Scope};
use super::intercept;
use super::managed::{MatchSet, RepairMode};
use super::repair;
use crate::exit_status;

pub const MANAGE_SCHEMA: &str = "aethyme.mcp-manage/v1";
pub const INTERCEPT_SCHEMA: &str = "aethyme.mcp-intercept/v1";
pub const REPAIR_SCHEMA: &str = "aethyme.mcp-repair/v1";
pub const BYPASS_SCHEMA: &str = "aethyme.mcp-bypass/v1";

/// Appended to `aethyme mcp --help`.
pub const USAGE: &str = "
Managed servers (host file only: managed = true, match_packages,
match_executables, repair = auto|report):
  aethyme mcp manage list [--json]
  aethyme mcp manage add <name> --package <pkg> [--package ...]
                         [--executable <name> ...] [--repair auto|report] [--json]
  aethyme mcp manage remove <name> [--json]
      List, mark or unmark managed entries in <host state>/mcp/services.toml
      (edited in place, formatting kept). A new entry starts as
      `npx -y <first package>`. Refreshes the shims when interception is on.
  aethyme mcp intercept install [--dir <path>] [--json]
  aethyme mcp intercept uninstall [--json]
  aethyme mcp intercept status [--json]
      Shims named after npx, npm, pnpm, pnpx, bunx, yarn and each managed
      executable, pointing at aethyme: a matching call becomes `aethyme mcp
      connect <name> --fallback -- <real launcher> <args>`, anything else
      runs the real tool untouched. Prints the PATH line to add for agent
      hosts; never edits a shell profile.
  aethyme mcp repair [--check|--apply] [--auto] [--agent codex|claude]
                     [--codex-config <path>] [--claude-config <path>] [--json]
      Find agent configuration entries (Codex config.toml, ~/.claude.json,
      a repository's .mcp.json) that start a managed server without
      connect; --apply rewrites only those (backup, compare-and-swap,
      formatting kept); --auto only entries with repair = \"auto\". A
      tracked .mcp.json is only reported. Prints names, never values.
  aethyme mcp bypass [--json]
      Running processes that match a managed entry but did not start
      through connect, counted per entry (1 s budget; inconclusive when the
      process table cannot be read).
";

fn json_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--json")
}

fn report_error(json: bool, code: &str, message: &str, exit: u8) -> u8 {
    if json {
        println!(
            "{}",
            json!({"schema": super::cli::ERROR_SCHEMA, "code": code, "message": message})
        );
    } else {
        eprintln!("aethyme mcp: {message}");
    }
    exit
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Parsed options: flags, repeated values, positionals.
#[derive(Default)]
struct Parsed {
    json: bool,
    flags: Vec<String>,
    values: Vec<(String, String)>,
    positionals: Vec<String>,
}

impl Parsed {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    fn one(&self, name: &str) -> Option<String> {
        self.all(name).pop()
    }
}

fn parse(
    args: &[String],
    flags: &[&str],
    valued: &[&str],
    max_positionals: usize,
) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while index < args.len() {
        let word = args[index].as_str();
        if word == "--json" {
            parsed.json = true;
        } else if flags.contains(&word) {
            parsed.flags.push(word.to_string());
        } else if valued.contains(&word) {
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| format!("{word} needs a value"))?;
            parsed.values.push((word.to_string(), value.clone()));
        } else if word.starts_with('-') {
            return Err(format!("unknown option {word}"));
        } else if parsed.positionals.len() < max_positionals {
            parsed.positionals.push(word.to_string());
        } else {
            return Err(format!("unexpected argument {word}"));
        }
        index += 1;
    }
    Ok(parsed)
}

fn host_or_error(json: bool) -> Result<PathBuf, u8> {
    super::host_directory().ok_or_else(|| {
        report_error(
            json,
            "host_state_unavailable",
            "the host state directory cannot be resolved (set HOME or AETHYME_HOST_STATE_DIR)",
            exit_status::ENVIRONMENT,
        )
    })
}

fn host_file(host: &Path) -> PathBuf {
    host.join("services.toml")
}

/// The host file's managed entries, with its refusals.
fn load_host(host: &Path) -> Loaded {
    config::load(
        &config::Sources {
            host_file: Some(host_file(host)),
            repository: None,
            trusted_digests: Vec::new(),
        },
        &super::checkouts::GitCheckouts,
        None,
    )
}

// ---------------------------------------------------------------------------
// manage

pub fn run_manage(args: &[String]) -> u8 {
    let json = json_requested(args);
    match args.split_first() {
        Some((sub, rest)) if sub == "list" => manage_list(rest),
        Some((sub, rest)) if sub == "add" => manage_add(rest),
        Some((sub, rest)) if sub == "remove" => manage_remove(rest),
        _ => report_error(
            json,
            "usage",
            "use `aethyme mcp manage list|add|remove`; see `aethyme mcp --help`",
            exit_status::USAGE,
        ),
    }
}

fn entries_json(set: &MatchSet) -> Vec<Value> {
    set.entries
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "match_packages": entry.packages,
                "match_executables": entry.executables,
                "repair": entry.repair,
            })
        })
        .collect()
}

fn manage_list(args: &[String]) -> u8 {
    let parsed = match parse(args, &[], &[], 0) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let loaded = load_host(&host);
    let set = MatchSet::from_loaded(&loaded);
    let refused: Vec<String> = loaded.errors.iter().map(ToString::to_string).collect();
    if parsed.json {
        print_json(&json!({
            "schema": MANAGE_SCHEMA,
            "action": "list",
            "host_file": host_file(&host),
            "entries": entries_json(&set),
            "errors": loaded.errors,
        }));
        return exit_status::SUCCESS;
    }
    if set.is_empty() {
        println!("No managed MCP servers ({}).", host_file(&host).display());
    }
    for entry in &set.entries {
        println!(
            "{}: packages [{}], executables [{}], repair {}",
            entry.name,
            entry.packages.join(", "),
            entry.executables.join(", "),
            entry.repair.as_str()
        );
    }
    for error in refused {
        println!("refused: {error}");
    }
    exit_status::SUCCESS
}

fn servers_mut(document: &mut DocumentMut) -> Result<&mut ArrayOfTables, String> {
    document
        .entry("servers")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| "`servers` in the host file is not an array of [[servers]] tables".into())
}

fn string_array(items: &[String]) -> Item {
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    value(array)
}

/// Read, edit and validate the host file, then replace it atomically.
fn edit_host_file(
    host: &Path,
    name: &str,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    let path = host_file(host);
    let (text, mode) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|metadata| metadata.permissions().mode() & 0o7777)
                .unwrap_or(0o600);
            (text, mode)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (format!("schema = {}\n", config::SCHEMA), 0o600)
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let mut document: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        let line = error
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
        format!(
            "{} is not valid TOML{}; fix it first",
            path.display(),
            line.map(|line| format!(" (line {line})"))
                .unwrap_or_default()
        )
    })?;
    edit(&mut document)?;
    let new_text = document.to_string();
    let (_, errors) = config::parse_and_validate(
        &new_text,
        &path,
        Scope::Host,
        None,
        &super::checkouts::GitCheckouts,
    );
    if let Some(error) = errors
        .iter()
        .find(|error| error.entry.as_deref().is_none_or(|entry| entry == name))
    {
        return Err(format!(
            "the edit would be refused, so nothing was written: {error}"
        ));
    }
    write_atomically(host, &path, new_text.as_bytes(), mode)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn write_atomically(host: &Path, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(host)?;
    let mut temporary = tempfile::NamedTempFile::new_in(host)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(mode))?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn find_server<'a>(servers: &'a mut ArrayOfTables, name: &str) -> Option<&'a mut Table> {
    servers
        .iter_mut()
        .find(|table| table.get("name").and_then(Item::as_str) == Some(name))
}

/// After a change to the managed list: refresh interception when installed.
fn refresh_interception(host: &Path) -> (MatchSet, Value) {
    let set = MatchSet::from_loaded(&load_host(host));
    let refreshed = match intercept::regenerate_if_installed(host, &set) {
        Ok(Some(report)) => json!({"state": "refreshed", "report": report}),
        Ok(None) => json!({"state": "not_installed"}),
        Err(message) => json!({"state": "failed", "message": message}),
    };
    (set, refreshed)
}

fn manage_add(args: &[String]) -> u8 {
    let started = Instant::now();
    let parsed = match parse(args, &[], &["--package", "--executable", "--repair"], 1) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let Some(name) = parsed.positionals.first().cloned() else {
        return report_error(
            parsed.json,
            "usage",
            "manage add needs an entry name",
            exit_status::USAGE,
        );
    };
    let packages = parsed.all("--package");
    let executables = parsed.all("--executable");
    if packages.is_empty() && executables.is_empty() {
        return report_error(
            parsed.json,
            "usage",
            "manage add needs at least one --package or --executable",
            exit_status::USAGE,
        );
    }
    let repair = match parsed.one("--repair").as_deref().map(RepairMode::parse) {
        None => None,
        Some(Some(mode)) => Some(mode),
        Some(None) => {
            return report_error(
                parsed.json,
                "usage",
                "--repair takes auto or report",
                exit_status::USAGE,
            );
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let mut created: Option<Vec<String>> = None;
    let result = edit_host_file(&host, &name, |document| {
        if find_server(servers_mut(document)?, &name).is_none() {
            let mut table = Table::new();
            // Comments at the end of the file stay where they were: above
            // the appended table rather than after it.
            let trailing = document.trailing().clone();
            document.set_trailing("");
            table.decor_mut().set_prefix(trailing);
            table.insert("name", value(name.as_str()));
            let argv = match packages.first() {
                Some(package) => vec!["npx".to_string(), "-y".to_string(), package.clone()],
                None => vec![executables[0].clone()],
            };
            table.insert("argv", string_array(&argv));
            servers_mut(document)?.push(table);
            created = Some(argv);
        }
        let servers = servers_mut(document)?;
        let table = find_server(servers, &name).ok_or("the entry could not be added")?;
        table.insert("managed", value(true));
        if packages.is_empty() {
            table.remove("match_packages");
        } else {
            table.insert("match_packages", string_array(&packages));
        }
        if executables.is_empty() {
            table.remove("match_executables");
        } else {
            table.insert("match_executables", string_array(&executables));
        }
        if let Some(mode) = repair {
            table.insert("repair", value(mode.as_str()));
        }
        Ok(())
    });
    if let Err(message) = result {
        return report_error(
            parsed.json,
            "manage_refused",
            &message,
            exit_status::REFUSED,
        );
    }
    let (set, refreshed) = refresh_interception(&host);
    if parsed.json {
        print_json(&json!({
            "schema": MANAGE_SCHEMA,
            "action": "add",
            "name": name,
            "created": created.is_some(),
            "host_file": host_file(&host),
            "entries": entries_json(&set),
            "interception": refreshed,
            "phases_ms": {"total": elapsed_ms(started)},
        }));
        return exit_status::SUCCESS;
    }
    println!("{name} is managed ({}).", host_file(&host).display());
    if let Some(argv) = &created {
        println!(
            "  new entry: argv = {}; edit argv in the host file if the server starts differently",
            argv.join(" ")
        );
    }
    print_refresh_text(&refreshed);
    exit_status::SUCCESS
}

fn print_refresh_text(refreshed: &Value) {
    match refreshed["state"].as_str() {
        Some("refreshed") => println!("  interception refreshed (shims and match file)"),
        Some("failed") => println!(
            "  interception NOT refreshed: {}",
            refreshed["message"].as_str().unwrap_or_default()
        ),
        _ => println!("  interception is not installed (`aethyme mcp intercept install`)"),
    }
}

fn manage_remove(args: &[String]) -> u8 {
    let started = Instant::now();
    let parsed = match parse(args, &[], &[], 1) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let Some(name) = parsed.positionals.first().cloned() else {
        return report_error(
            parsed.json,
            "usage",
            "manage remove needs an entry name",
            exit_status::USAGE,
        );
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let result = edit_host_file(&host, &name, |document| {
        let servers = servers_mut(document)?;
        let table =
            find_server(servers, &name).ok_or_else(|| format!("no host entry named {name:?}"))?;
        let mut removed = false;
        for key in ["managed", "match_packages", "match_executables", "repair"] {
            removed |= table.remove(key).is_some();
        }
        if removed {
            Ok(())
        } else {
            Err(format!("{name:?} is not managed"))
        }
    });
    if let Err(message) = result {
        return report_error(
            parsed.json,
            "manage_refused",
            &message,
            exit_status::REFUSED,
        );
    }
    let (set, refreshed) = refresh_interception(&host);
    if parsed.json {
        print_json(&json!({
            "schema": MANAGE_SCHEMA,
            "action": "remove",
            "name": name,
            "host_file": host_file(&host),
            "entries": entries_json(&set),
            "interception": refreshed,
            "phases_ms": {"total": elapsed_ms(started)},
        }));
        return exit_status::SUCCESS;
    }
    println!("{name} is no longer managed; the entry itself is kept.");
    print_refresh_text(&refreshed);
    exit_status::SUCCESS
}

// ---------------------------------------------------------------------------
// intercept

pub fn run_intercept(args: &[String]) -> u8 {
    let json = json_requested(args);
    match args.split_first() {
        Some((sub, rest)) if sub == "install" => intercept_install(rest),
        Some((sub, rest)) if sub == "uninstall" => intercept_uninstall(rest),
        Some((sub, rest)) if sub == "status" => intercept_status(rest),
        _ => report_error(
            json,
            "usage",
            "use `aethyme mcp intercept install|uninstall|status`",
            exit_status::USAGE,
        ),
    }
}

fn intercept_install(args: &[String]) -> u8 {
    let parsed = match parse(args, &[], &["--dir"], 0) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let set = MatchSet::from_loaded(&load_host(&host));
    let target = match intercept::aethyme_target() {
        Ok(target) => target,
        Err(message) => {
            return report_error(
                parsed.json,
                "aethyme_not_found",
                &message,
                exit_status::ENVIRONMENT,
            );
        }
    };
    let dir = parsed.one("--dir").map(PathBuf::from);
    let report = match intercept::install(&host, dir.as_deref(), &set, &target) {
        Ok(report) => report,
        Err(message) => {
            return report_error(
                parsed.json,
                "install_refused",
                &message,
                exit_status::REFUSED,
            );
        }
    };
    let status = intercept::status(&host, &set, std::env::var_os("PATH").as_deref());
    if parsed.json {
        print_json(&json!({
            "schema": INTERCEPT_SCHEMA,
            "action": "install",
            "report": report,
            "status": status,
        }));
        return exit_status::SUCCESS;
    }
    println!(
        "Installed {} shims in {} -> {}",
        report.shims.len(),
        report.shim_dir.display(),
        report.target.display()
    );
    println!("  managed entries: {}", report.managed_entries);
    println!("Add this line where agent hosts get their environment, so the shims come first:");
    println!("  {}", report.path_line);
    println!(
        "Aethyme never edits a shell profile. Only hosts started with this directory first in \
         PATH are intercepted."
    );
    if status.ahead_of_real_tools {
        println!("This shell already has it first in PATH.");
    }
    exit_status::SUCCESS
}

fn intercept_uninstall(args: &[String]) -> u8 {
    let parsed = match parse(args, &[], &[], 0) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let report = match intercept::uninstall(&host) {
        Ok(report) => report,
        Err(message) => {
            return report_error(
                parsed.json,
                "uninstall_failed",
                &message,
                exit_status::FAILED,
            );
        }
    };
    if parsed.json {
        print_json(&json!({"schema": INTERCEPT_SCHEMA, "action": "uninstall", "report": report}));
        return exit_status::SUCCESS;
    }
    println!(
        "Removed {} shims{}{}.",
        report.removed.len(),
        if report.directory_removed {
            " and the shim directory"
        } else {
            ""
        },
        if report.match_file_removed {
            " and the match file"
        } else {
            ""
        }
    );
    println!("Remove the PATH line you added for agent hosts, if any.");
    exit_status::SUCCESS
}

fn intercept_status(args: &[String]) -> u8 {
    let parsed = match parse(args, &[], &[], 0) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let set = MatchSet::from_loaded(&load_host(&host));
    let status = intercept::status(&host, &set, std::env::var_os("PATH").as_deref());
    if parsed.json {
        print_json(&json!({"schema": INTERCEPT_SCHEMA, "action": "status", "status": status}));
        return exit_status::SUCCESS;
    }
    print_intercept_text(&status);
    exit_status::SUCCESS
}

fn print_intercept_text(status: &intercept::InterceptStatus) {
    if !status.installed {
        println!("interception: not installed (`aethyme mcp intercept install`)");
        return;
    }
    println!(
        "interception: {} ({}), match file {}",
        if status.complete {
            "complete"
        } else {
            "incomplete"
        },
        status.shim_dir.display(),
        status.match_file_state
    );
    if !status.missing_shims.is_empty() {
        println!("  missing shims: {}", status.missing_shims.join(", "));
    }
    if !status.broken_shims.is_empty() {
        println!("  broken shims: {}", status.broken_shims.join(", "));
    }
    if status.ahead_of_real_tools {
        println!("  on PATH ahead of the real launchers in this shell");
    } else if status.on_path {
        println!("  on PATH but behind a real launcher in this shell");
    } else {
        println!("  not on this shell's PATH; add: {}", status.path_line);
    }
}

// ---------------------------------------------------------------------------
// repair

pub fn run_repair(args: &[String]) -> u8 {
    let started = Instant::now();
    let parsed = match parse(
        args,
        &["--check", "--apply", "--auto"],
        &["--agent", "--codex-config", "--claude-config"],
        0,
    ) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    if parsed.flag("--check") && parsed.flag("--apply") {
        return report_error(
            parsed.json,
            "usage",
            "use --check or --apply, not both",
            exit_status::USAGE,
        );
    }
    let agent = parsed.one("--agent");
    if agent
        .as_deref()
        .is_some_and(|agent| agent != "codex" && agent != "claude")
    {
        return report_error(
            parsed.json,
            "usage",
            "--agent takes codex or claude",
            exit_status::USAGE,
        );
    }
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let phase = Instant::now();
    let set = MatchSet::from_loaded(&load_host(&host));
    let config_ms = elapsed_ms(phase);
    let cwd = std::env::current_dir().ok();
    let locations = repair::Locations::discover(
        cwd.as_deref(),
        parsed.one("--codex-config").map(PathBuf::from),
        parsed.one("--claude-config").map(PathBuf::from),
    );
    let targets = locations.targets(agent.as_deref());
    let options = repair::Options {
        apply: parsed.flag("--apply"),
        auto: parsed.flag("--auto"),
        deadline: None,
        backup_dir: host.join(repair::BACKUP_DIRECTORY),
    };
    let report = repair::run(&set, &targets, &locations, &options);
    repair::record(&host, &report, elapsed_ms(started));
    let failed = report
        .files
        .iter()
        .any(|file| matches!(file.state, "contended" | "write_failed"));
    if parsed.json {
        print_json(&json!({
            "schema": REPAIR_SCHEMA,
            "report": report,
            "phases_ms": {"config_load": config_ms, "total": elapsed_ms(started)},
        }));
    } else if options.auto {
        // Meant for hooks: silent on a no-op, one line when it repaired.
        if report.repaired > 0 {
            println!(
                "aethyme mcp: repaired {} managed server entr{}; it takes effect at the next \
                 agent start",
                report.repaired,
                if report.repaired == 1 { "y" } else { "ies" }
            );
        }
    } else {
        print_repair_text(&report, set.is_empty());
    }
    if failed {
        exit_status::FAILED
    } else {
        exit_status::SUCCESS
    }
}

fn print_repair_text(report: &repair::Report, no_managed: bool) {
    if no_managed {
        println!("No managed MCP servers are configured (`aethyme mcp manage add`).");
        return;
    }
    for file in &report.files {
        println!(
            "{} {}: {}{}",
            file.agent,
            file.file.display(),
            file.state,
            file.detail
                .as_deref()
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default()
        );
        for finding in &file.findings {
            println!(
                "  {} [{}] -> {} (program {}, {} {}): {}{}",
                finding.server,
                finding.scope,
                finding.entry,
                finding.program,
                finding.matched_kind,
                finding.matched,
                finding.state,
                finding
                    .reason
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            );
        }
        if file.already_managed > 0 {
            println!(
                "  {} entr{} already start through connect",
                file.already_managed,
                if file.already_managed == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
        }
        if let Some(backup) = &file.backup {
            println!("  backup: {}", backup.display());
        }
    }
    if report.mode == "check"
        && report
            .files
            .iter()
            .flat_map(|file| &file.findings)
            .any(|finding| finding.state == "would_repair")
    {
        println!(
            "Run `aethyme mcp repair --apply` to rewrite them (takes effect at the next agent start)."
        );
    }
}

// ---------------------------------------------------------------------------
// bypass

pub fn run_bypass(args: &[String]) -> u8 {
    let parsed = match parse(args, &[], &[], 0) {
        Ok(parsed) => parsed,
        Err(message) => {
            return report_error(json_requested(args), "usage", &message, exit_status::USAGE);
        }
    };
    let host = match host_or_error(parsed.json) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let set = MatchSet::from_loaded(&load_host(&host));
    let report = bypass::scan(&set, Some(&host), bypass::BUDGET);
    bypass::record(&host, &report);
    if parsed.json {
        print_json(&json!({"schema": BYPASS_SCHEMA, "report": report}));
        return exit_status::SUCCESS;
    }
    print_bypass_text(&report, set.is_empty());
    exit_status::SUCCESS
}

fn print_bypass_text(report: &bypass::BypassReport, no_managed: bool) {
    if no_managed {
        println!("bypass: no managed MCP servers are configured");
        return;
    }
    if report.state != "complete" {
        println!(
            "bypass: inconclusive ({})",
            report.reason.as_deref().unwrap_or("unknown")
        );
        return;
    }
    if report.total() == 0 {
        println!("bypass: none among {} processes", report.processes_scanned);
    }
    for entry in &report.entries {
        if entry.processes == 0 {
            continue;
        }
        let matched: Vec<String> = entry
            .matched
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect();
        println!(
            "bypass: {} has {} process{} not started through connect ({})",
            entry.name,
            entry.processes,
            if entry.processes == 1 { "" } else { "es" },
            matched.join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// status

/// The `managed` section of `aethyme mcp status --json`.
pub fn status_value(loaded: &Loaded) -> Value {
    let set = MatchSet::from_loaded(loaded);
    let Some(host) = super::host_directory() else {
        return json!({"entries": entries_json(&set), "state": "host_state_unavailable"});
    };
    let interception = intercept::status(&host, &set, std::env::var_os("PATH").as_deref());
    let bypass = bypass::scan(&set, Some(&host), bypass::BUDGET);
    let repair_on_session_start = match repair::repair_on_session_start(&host) {
        Ok(on) => json!(on),
        Err(message) => json!({"error": message}),
    };
    json!({
        "entries": entries_json(&set),
        "interception": interception,
        "bypass": bypass,
        "repair_on_session_start": repair_on_session_start,
        "settings_file": host.join(repair::SETTINGS_FILE),
    })
}

/// The managed lines of `aethyme mcp status`.
pub fn print_status_text(loaded: &Loaded) {
    let set = MatchSet::from_loaded(loaded);
    if set.is_empty() {
        return;
    }
    let names: Vec<&str> = set
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    println!("managed: {}", names.join(", "));
    let Some(host) = super::host_directory() else {
        return;
    };
    print_intercept_text(&intercept::status(
        &host,
        &set,
        std::env::var_os("PATH").as_deref(),
    ));
    print_bypass_text(&bypass::scan(&set, Some(&host), bypass::BUDGET), false);
}
