//! `aethyme transcripts`: the command group.
//!
//! `--json` prints one object whose `schema` names its shape
//! (`aethyme.transcripts-<command>/v1`); a refusal is
//! `aethyme.transcripts-error/v1` with `code` and `message`.

use serde_json::{Value, json};

use super::agents::SystemAgents;
use super::config::{self, SettingsChange};
use super::open_files::{OpenFiles, SystemProbe};
use super::providers::Provider;
use super::run::{self, OPEN_LISTING_MAX_AGE, Options, Probes};
use super::{Environment, TranscriptsConfig};
use crate::exit_status;

pub const ERROR_SCHEMA: &str = "aethyme.transcripts-error/v1";
pub const LIST_SCHEMA: &str = "aethyme.transcripts-list/v1";
pub const RESTORE_SCHEMA: &str = "aethyme.transcripts-restore/v1";
pub const SETTINGS_SCHEMA: &str = "aethyme.transcripts-settings/v1";

pub const USAGE: &str = "\
aethyme transcripts — archive closed agent transcripts (on by default)

A transcript inactive for more than archive_after_inactive_days (default 3;
for Claude Code archive_after_inactive_days_claude, default 14) is
archived. Its last activity is the newest of its modification time, the
timestamp of its last record, and the last activity of any broker session
linked to it. It must also be open in no process, have every linked broker
session closed with its checkout gone, and (Claude Code) have no running
claude process for its project or session. It is compressed with zstd,
verified by SHA-256, and moved. Codex transcripts go to
<codex home>/archived_sessions/<name>.jsonl.zst, which `codex unarchive`
and `codex resume` read natively. Claude Code transcripts go under
<host state>/transcripts/claude/; restore them with `transcripts restore`.
Settings: <host state>/transcripts/settings.toml, [transcripts] archive,
archive_after_inactive_days, archive_after_inactive_days_claude (the older
archive_after_days names are still read), providers (\"codex\",
\"claude\"; default both), codex_home, claude_home, repositories,
bytes_per_second, free_space_floor_bytes. Without the file, or without
archive in it, archiving is on; the background worker then waits 24 hours
after `aethyme broker status` first shows the notice
transcripts.archiving-default-on (enable or a manual archive --apply skips
the wait). Nothing is listed or moved while archive = false.

Usage:
  aethyme transcripts status [--json]
      Settings and where `enabled` came from (enabled_source), the first-run
      wait, archived files and bytes before/after compression, bytes an
      archive pass could reclaim now, lifetime counters, the last run.
  aethyme transcripts list [--json]
      Archived transcripts, and the ones a pass would archive or skip (with
      the reason for each skip).
  aethyme transcripts archive --dry-run|--apply [--json]
      Print what would move, or archive it now (largest first, paced). The
      background worker runs the same pass while archiving is on.
  aethyme transcripts restore <session-id|path> [--json]
      Decompress an archived transcript back to where it came from,
      verifying its SHA-256, then remove the archive. When a newer transcript
      exists at that path, the result is the archive followed by it.
  aethyme transcripts enable [--after-inactive-days <n>] [--providers codex,claude] [--json]
  aethyme transcripts disable [--json]
      Turn archiving on or off explicitly (--after-days is an alias of
      --after-inactive-days). Writes only the settings file above.
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
        eprintln!("aethyme transcripts: {message}");
    }
    exit
}

fn print_json(value: &impl serde::Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("aethyme transcripts: cannot render JSON: {error}"),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The command group's entry point.
pub fn run(args: &[String]) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return exit_status::USAGE;
    };
    // Help anywhere answers help and does nothing else.
    if command == "help" || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return exit_status::SUCCESS;
    }
    let json = json_requested(rest);
    let Some(environment) = Environment::from_process() else {
        return report_error(
            json,
            "host_state_unavailable",
            "the host state directory cannot be resolved (HOME is unset)",
            exit_status::ENVIRONMENT,
        );
    };
    match command.as_str() {
        "status" => run_status(&environment, rest, json),
        "list" => run_list(&environment, rest, json),
        "archive" => run_archive(&environment, rest, json),
        "restore" => run_restore(&environment, rest, json),
        "enable" => run_settings(&environment, rest, json, true),
        "disable" => run_settings(&environment, rest, json, false),
        other => report_error(
            json,
            "usage",
            &format!("unknown subcommand {other:?}; see `aethyme transcripts --help`"),
            exit_status::USAGE,
        ),
    }
}

/// Refuse flags a subcommand does not take.
fn check_flags(rest: &[String], allowed: &[&str], json: bool) -> Result<Vec<String>, u8> {
    let mut positional = Vec::new();
    for arg in rest {
        if arg.starts_with("--") {
            if !allowed.contains(&arg.as_str()) {
                return Err(report_error(
                    json,
                    "usage",
                    &format!("unknown flag {arg}; see `aethyme transcripts --help`"),
                    exit_status::USAGE,
                ));
            }
        } else {
            positional.push(arg.clone());
        }
    }
    Ok(positional)
}

fn load_config(environment: &Environment, json: bool) -> Result<TranscriptsConfig, u8> {
    TranscriptsConfig::load(&environment.host_dir)
        .map_err(|error| report_error(json, "settings_invalid", &error, exit_status::REFUSED))
}

fn run_status(environment: &Environment, rest: &[String], json: bool) -> u8 {
    if let Err(code) = check_flags(rest, &["--json"], json) {
        return code;
    }
    let config = match load_config(environment, json) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    let status = match run::status(environment, &config, &mut probes) {
        Ok(status) => status,
        Err(error) => return report_error(json, "status_failed", &error, exit_status::ENVIRONMENT),
    };
    if json {
        print_json(&status);
        return exit_status::SUCCESS;
    }
    println!(
        "transcript archiving: {} ({})",
        if status.enabled {
            "enabled"
        } else {
            "disabled"
        },
        match status.enabled_source {
            super::EnabledSource::Default => "default",
            super::EnabledSource::Settings => "settings file",
        }
    );
    println!("  settings: {}", status.settings_file.display());
    if status.enabled {
        let providers: Vec<&str> = config.providers.iter().map(|p| p.label()).collect();
        println!(
            "  providers: {}; archive after {} inactive days (Claude Code: {} inactive days)",
            providers.join(", "),
            config.archive_after_inactive_days,
            config.archive_after_inactive_days_claude
        );
        for line in super::first_run::status_lines(&status.first_run) {
            println!("{line}");
        }
    }
    println!(
        "  archived: {} files, {} before compression, {} now",
        status.archived_files,
        human_bytes(status.archived_bytes_before),
        human_bytes(status.archived_bytes_after)
    );
    if status.enabled {
        println!(
            "  reclaimable now: {} in {} files",
            human_bytes(status.reclaimable_now_bytes),
            status.eligible_now
        );
        for (reason, count) in &status.skipped_now {
            println!("  skipped now ({reason}): {count}");
        }
    } else {
        println!("  next: aethyme transcripts enable");
    }
    for warning in &status.warnings {
        println!("  Warning: {warning}");
    }
    exit_status::SUCCESS
}

fn run_list(environment: &Environment, rest: &[String], json: bool) -> u8 {
    if let Err(code) = check_flags(rest, &["--json"], json) {
        return code;
    }
    let config = match load_config(environment, json) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let archived = match run::archived_rows(environment, &config) {
        Ok(rows) => rows,
        Err(error) => {
            return report_error(json, "index_unreadable", &error, exit_status::ENVIRONMENT);
        }
    };
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    let pass = match run::run_pass(
        environment,
        &config,
        Options {
            apply: false,
            budget: None,
        },
        &mut probes,
        &mut super::archive::NoHooks,
        super::now_ms(),
    ) {
        Ok(pass) => pass,
        Err(error) => return report_error(json, "list_failed", &error, exit_status::ENVIRONMENT),
    };
    if json {
        print_json(&json!({
            "schema": LIST_SCHEMA,
            "enabled": config.archive,
            "archived": archived,
            "archived_bytes_before": archived.iter().map(|row| row.bytes_in).sum::<u64>(),
            "archived_bytes_after": archived.iter().map(|row| row.bytes_out).sum::<u64>(),
            "eligible": pass.eligible,
            "eligible_bytes": pass.eligible_bytes,
            "skipped": pass.skipped,
            "skipped_examples": pass.skipped_examples,
            "warnings": pass.warnings,
        }));
        return exit_status::SUCCESS;
    }
    println!("archived ({}):", archived.len());
    for row in &archived {
        println!(
            "  {} -> {} ({} -> {})",
            row.original_path.display(),
            row.archive_path.display(),
            human_bytes(row.bytes_in),
            human_bytes(row.bytes_out)
        );
    }
    if !config.archive {
        println!("archiving is disabled; nothing else is listed (aethyme transcripts enable)");
        return exit_status::SUCCESS;
    }
    print_pass(&pass);
    exit_status::SUCCESS
}

fn print_pass(pass: &run::RunReport) {
    println!(
        "eligible ({}, {}):",
        pass.eligible.len(),
        human_bytes(pass.eligible_bytes)
    );
    for transcript in &pass.eligible {
        println!(
            "  {} ({}) -> {}",
            transcript.path.display(),
            human_bytes(transcript.bytes),
            transcript.destination.display()
        );
    }
    for (reason, count) in &pass.skipped {
        println!("skipped ({reason}): {count}");
    }
    for example in &pass.skipped_examples {
        println!(
            "  {}: {}{}",
            example.reason.label(),
            example.path.display(),
            if example.detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", example.detail)
            }
        );
    }
    for warning in &pass.warnings {
        println!("Warning: {warning}");
    }
}

fn run_archive(environment: &Environment, rest: &[String], json: bool) -> u8 {
    if let Err(code) = check_flags(rest, &["--json", "--dry-run", "--apply"], json) {
        return code;
    }
    let apply = rest.iter().any(|arg| arg == "--apply");
    let dry_run = rest.iter().any(|arg| arg == "--dry-run");
    if apply == dry_run {
        return report_error(
            json,
            "usage",
            "pass exactly one of --dry-run or --apply",
            exit_status::USAGE,
        );
    }
    let config = match load_config(environment, json) {
        Ok(config) => config,
        Err(code) => return code,
    };
    if !config.archive {
        return report_error(
            json,
            "disabled",
            "transcript archiving is disabled on this host; next: aethyme transcripts enable",
            exit_status::REFUSED,
        );
    }
    if apply {
        // Archiving by hand lifts the first-run wait for the worker too.
        if let Some(warning) = run::acknowledge_manual_apply(environment, super::now_ms()) {
            eprintln!("aethyme transcripts: Warning: {warning}");
        }
    }
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    let pass = match run::run_pass(
        environment,
        &config,
        Options {
            apply,
            budget: None,
        },
        &mut probes,
        &mut super::archive::NoHooks,
        super::now_ms(),
    ) {
        Ok(pass) => pass,
        Err(error) => {
            return report_error(json, "archive_failed", &error, exit_status::ENVIRONMENT);
        }
    };
    if pass.lock == "busy" {
        return report_error(
            json,
            "busy",
            "another archive run holds the lock; retry when it finishes",
            exit_status::ENVIRONMENT,
        );
    }
    if json {
        print_json(&pass);
        return exit_status::SUCCESS;
    }
    if apply {
        println!(
            "archived {} transcripts: {} -> {}{}",
            pass.archived.len(),
            human_bytes(pass.bytes_in),
            human_bytes(pass.bytes_out),
            if pass.resumed > 0 {
                format!(" ({} finished from an interrupted run)", pass.resumed)
            } else {
                String::new()
            }
        );
    } else {
        println!("dry run: nothing was moved");
    }
    print_pass(&pass);
    exit_status::SUCCESS
}

fn run_restore(environment: &Environment, rest: &[String], json: bool) -> u8 {
    let positional = match check_flags(rest, &["--json"], json) {
        Ok(positional) => positional,
        Err(code) => return code,
    };
    let [key] = positional.as_slice() else {
        return report_error(
            json,
            "usage",
            "restore takes one agent session id or path",
            exit_status::USAGE,
        );
    };
    let config = match load_config(environment, json) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    match run::restore(environment, &config, key, &mut probes) {
        Ok(restored) => {
            if json {
                print_json(&json!({"schema": RESTORE_SCHEMA, "restored": restored}));
            } else {
                for item in &restored {
                    println!(
                        "restored {} ({}, SHA-256 {}{})",
                        item.original.display(),
                        human_bytes(item.bytes),
                        item.sha256,
                        if item.hash_checked {
                            ", matches the index"
                        } else {
                            "; zstd checksum verified, no index row"
                        }
                    );
                    if item.already_restored {
                        println!(
                            "  it was already restored by an interrupted run; only the cleanup ran"
                        );
                    }
                    if let Some(backup) = &item.fragment_backup_kept {
                        println!(
                            "  Warning: the newer transcript changed during the restore; its previous copy is kept at {}",
                            backup.display()
                        );
                    }
                    if item.merged_fragment_bytes > 0 {
                        println!(
                            "  a newer transcript at that path ({}) was appended after the archived content",
                            human_bytes(item.merged_fragment_bytes)
                        );
                    }
                }
            }
            exit_status::SUCCESS
        }
        Err(error) => report_error(json, "restore_refused", &error, exit_status::REFUSED),
    }
}

pub(crate) fn parse_settings_flags(
    rest: &[String],
    enable: bool,
) -> Result<SettingsChange, String> {
    let mut change = SettingsChange {
        archive: enable,
        ..SettingsChange::default()
    };
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => {}
            flag @ ("--after-inactive-days" | "--after-days") if enable => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{flag} needs a number of days"))?;
                let days = value
                    .parse::<u32>()
                    .ok()
                    .filter(|days| *days >= 1)
                    .ok_or_else(|| format!("{flag} must be a whole number of at least 1"))?;
                change.archive_after_inactive_days = Some(days);
            }
            "--providers" if enable => {
                let value = iter
                    .next()
                    .ok_or("--providers needs codex, claude or both")?;
                let mut providers = Vec::new();
                for name in value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    let provider = Provider::parse(name)
                        .ok_or_else(|| format!("unknown provider {name:?} (codex or claude)"))?;
                    if !providers.contains(&provider) {
                        providers.push(provider);
                    }
                }
                change.providers = Some(providers);
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(change)
}

fn run_settings(environment: &Environment, rest: &[String], json: bool, enable: bool) -> u8 {
    let change = match parse_settings_flags(rest, enable) {
        Ok(change) => change,
        Err(error) => return report_error(json, "usage", &error, exit_status::USAGE),
    };
    let config = match config::write_settings(&environment.host_dir, &change) {
        Ok(config) => config,
        Err(error) => {
            return report_error(
                json,
                "settings_write_failed",
                &error,
                exit_status::ENVIRONMENT,
            );
        }
    };
    let settings_file = environment.host_dir.join(config::SETTINGS_FILE);
    if json {
        let value: Value = json!({
            "schema": SETTINGS_SCHEMA,
            "settings_file": settings_file,
            "config": config,
        });
        print_json(&value);
    } else {
        println!(
            "transcript archiving {} ({})",
            if config.archive {
                "enabled"
            } else {
                "disabled"
            },
            settings_file.display()
        );
    }
    exit_status::SUCCESS
}
