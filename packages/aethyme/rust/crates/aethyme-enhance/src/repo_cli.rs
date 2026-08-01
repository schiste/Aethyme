//! Native `aethyme repo` UX subcommands (python-retirement Phase 3 flip).
//!
//! Handles the deployment/onboarding/telemetry/hygiene slice of the repo
//! group: `deploy-skills`, `compile-skills`,
//! `init-onboarding-overrides`, `validate-onboarding-overrides`,
//! `init-agents-overrides`, `validate-agents-overrides`,
//! `record-wrapper-invocation` (hidden), `experience-telemetry`,
//! `experience-status`, `commit-message-template`,
//! `lint-commit-message`. The engine-facing basics (`ingest`, `inspect`,
//! `clear-cache`, `warm`, `engine-info`) stay in
//! `aethyme_engine::repo_cli`; anything else returns
//! [`Outcome::Delegate`] so the router can produce its unified
//! unknown-subcommand error.
//!
//! Stdout is byte-compatible with the deleted Click commands
//! (`src/cli.py` repo group before the Phase 3 flip). Error surfaces
//! follow the Phase 2 precedent: Click's `Error: {message}` line without
//! the usage block, exit 2 for usage errors, exit 1 for
//! ClickException-style runtime errors.
//!
//! Fire-and-forget hardening (plan Phase 3 risk): the hidden
//! `record-wrapper-invocation` hook command logs argument-parse failures
//! into the repo-local telemetry ledger (event type
//! `wrapper.invocation-error`) when a usable repo directory is
//! identifiable, instead of dying silently behind the hook's `|| true`.

use std::path::{Path, PathBuf};

use crate::pyjson::{self, Value, py_bool};
use crate::util::resolve_path;
use crate::{AGENTS_OVERRIDE_PATH, agents, hygiene, onboarding, skills, telemetry};

/// Result of dispatching a repo subcommand.
pub enum Outcome {
    /// Command ran natively; carries the process exit code.
    Handled(u8),
    /// Not one of the Phase 3 subcommands.
    Delegate,
}

/// Run `repo <subcommand> ...` with `args` excluding the leading `repo`.
/// `resolve_root` supplies the Aethyme package root lazily — only
/// `deploy-skills` and `compile-skills` need it (template substitution
/// and navigation recipes); the telemetry/hygiene commands must work in
/// repos with no Aethyme checkout reachable (deployed-hook path).
pub fn run(args: &[String], resolve_root: &dyn Fn() -> Option<PathBuf>) -> Outcome {
    let Some(subcommand) = args.first() else {
        return Outcome::Delegate;
    };
    let rest = &args[1..];
    let code = match subcommand.as_str() {
        "deploy-skills" => run_deploy_skills(rest, resolve_root),
        "compile-skills" => run_compile_skills(rest, resolve_root),
        "init-onboarding-overrides" => run_init_onboarding_overrides(rest),
        "validate-onboarding-overrides" => run_validate_onboarding_overrides(rest),
        "init-agents-overrides" => run_init_agents_overrides(rest),
        "validate-agents-overrides" => run_validate_agents_overrides(rest),
        "record-wrapper-invocation" => run_record_wrapper_invocation(rest),
        "hook-envelope" => run_hook_envelope(rest),
        "experience-telemetry" => run_experience_telemetry(rest),
        "experience-status" => run_experience_status(rest),
        "commit-message-template" => run_commit_message_template(rest),
        "lint-commit-message" => run_lint_commit_message(rest),
        _ => return Outcome::Delegate,
    };
    Outcome::Handled(code)
}

// ── argument helpers ────────────────────────────────────────────────────────

fn usage_error(message: &str) -> u8 {
    eprintln!("Error: {message}");
    2
}

fn runtime_error(message: &str) -> u8 {
    eprintln!("Error: {message}");
    1
}

struct ParsedArgs {
    positionals: Vec<String>,
    flags: Vec<String>,
    options: Vec<(String, String)>,
}

/// Click-style parse: `flag_names` are boolean switches, `option_names`
/// take a value (`--opt value` or `--opt=value`), anything else starting
/// with `--` is unknown. Options may appear before or after positionals.
fn parse_args(
    rest: &[String],
    flag_names: &[&str],
    option_names: &[&str],
) -> Result<ParsedArgs, String> {
    let mut parsed = ParsedArgs {
        positionals: Vec::new(),
        flags: Vec::new(),
        options: Vec::new(),
    };
    let mut i = 0usize;
    while i < rest.len() {
        let arg = &rest[i];
        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline_value) = match body.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (body, None),
            };
            let dashed = format!("--{name}");
            if flag_names.contains(&dashed.as_str()) {
                if inline_value.is_some() {
                    return Err(format!("Option '{dashed}' does not take a value."));
                }
                parsed.flags.push(dashed);
                i += 1;
            } else if option_names.contains(&dashed.as_str()) {
                let value = match inline_value {
                    Some(value) => value,
                    None => {
                        let Some(value) = rest.get(i + 1) else {
                            return Err(format!("Option '{dashed}' requires an argument."));
                        };
                        i += 1;
                        value.clone()
                    }
                };
                parsed.options.push((dashed, value));
                i += 1;
            } else {
                return Err(format!("No such option: {arg}"));
            }
        } else {
            parsed.positionals.push(arg.clone());
            i += 1;
        }
    }
    Ok(parsed)
}

impl ParsedArgs {
    fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    fn option_values(&self, name: &str) -> Vec<&str> {
        self.options
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn last_option(&self, name: &str) -> Option<&str> {
        self.option_values(name).last().copied()
    }
}

/// Validate a `click.Path(exists=True, file_okay=False)` positional.
fn require_repo_dir(parsed: &ParsedArgs) -> Result<PathBuf, String> {
    let Some(raw) = parsed.positionals.first() else {
        return Err("Missing argument 'REPO_PATH'.".to_string());
    };
    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(format!(
            "Invalid value for 'REPO_PATH': Directory '{raw}' does not exist."
        ));
    }
    if path.is_file() {
        return Err(format!(
            "Invalid value for 'REPO_PATH': Directory '{raw}' is a file."
        ));
    }
    Ok(path)
}

/// `str(pathlib.Path(raw))` — the shape Click echoes back for
/// `path_type=Path` arguments: no resolution, but pure-path
/// normalization (collapse `//`, drop `.` components and trailing `/`).
fn py_path_str(raw: &str) -> String {
    let (root, remainder) = if let Some(stripped) = raw.strip_prefix('/') {
        // POSIX: exactly two leading slashes are preserved by pathlib.
        if raw.starts_with("//") && !raw.starts_with("///") {
            ("//", raw.trim_start_matches('/'))
        } else {
            ("/", stripped)
        }
    } else {
        ("", raw)
    };
    let parts: Vec<&str> = remainder
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() {
        if root.is_empty() {
            ".".to_string()
        } else {
            root.to_string()
        }
    } else {
        format!("{root}{}", parts.join("/"))
    }
}

fn append_event_or_error(repo: &Path, event_type: &str, payload: Value) -> Result<(), u8> {
    telemetry::append_event(repo, event_type, payload).map_err(|message| runtime_error(&message))
}

// ── skills ──────────────────────────────────────────────────────────────────

fn resolved_root(
    subcommand: &str,
    resolve_root: &dyn Fn() -> Option<PathBuf>,
) -> Result<String, u8> {
    match resolve_root() {
        Some(root) => Ok(resolve_path(&root).display().to_string()),
        None => {
            eprintln!(
                "aethyme: cannot locate the Aethyme package root for the 'repo {subcommand}' command."
            );
            eprintln!("  - run from inside an Aethyme checkout (auto-discovered), or");
            eprintln!(
                "  - `aethyme root set /path/to/Aethyme` (writes ~/.config/aethyme/root), or"
            );
            eprintln!("  - export AETHYME_ROOT=/path/to/Aethyme/packages/aethyme");
            Err(2)
        }
    }
}

fn run_deploy_skills(rest: &[String], resolve_root: &dyn Fn() -> Option<PathBuf>) -> u8 {
    let parsed = match parse_args(rest, &["--force", "--remove"], &[]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    if parsed.has_flag("--remove") {
        match skills::remove_skills(&repo) {
            Ok(removed) if !removed.is_empty() => {
                println!("Removed skills: {}", removed.join(", "));
            }
            Ok(_) => println!("No Aethyme skills found to remove."),
            Err(message) => return runtime_error(&message),
        }
        return 0;
    }
    let root = match resolved_root("deploy-skills", resolve_root) {
        Ok(root) => root,
        Err(code) => return code,
    };
    match skills::deploy_skills(&repo, &root, parsed.has_flag("--force")) {
        Ok(deployed) if !deployed.is_empty() => {
            println!("Deployed skills: {}", deployed.join(", "));
        }
        Ok(_) => println!("All skills already present (use --force to overwrite)."),
        Err(message) => return runtime_error(&message),
    }
    println!(
        "Note: `repo deploy-skills` is a compatibility path. Prefer `aethyme enhance deploy --repo <path>`."
    );
    0
}

fn run_compile_skills(rest: &[String], resolve_root: &dyn Fn() -> Option<PathBuf>) -> u8 {
    let parsed = match parse_args(rest, &[], &["--skill"]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    let skill_names: Vec<&str> = parsed.option_values("--skill");
    for skill in &skill_names {
        if *skill != "repo-onboarding" {
            return usage_error(&format!(
                "Invalid value for '--skill': '{skill}' is not one of 'repo-onboarding'."
            ));
        }
    }
    let selected: Vec<&str> = if skill_names.is_empty() {
        vec!["repo-onboarding"]
    } else {
        skill_names
    };
    let root = match resolved_root("compile-skills", resolve_root) {
        Ok(root) => root,
        Err(code) => return code,
    };

    let mut written: Vec<String> = Vec::new();
    if selected.contains(&"repo-onboarding") {
        let files = match onboarding::expected_onboarding_files(&repo, Path::new(&root)) {
            Ok(files) => files,
            Err(message) => return runtime_error(&message),
        };
        for (relative_path, content) in files {
            let dest = repo.join(&relative_path);
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return runtime_error(&format!("{}: {e}", parent.display()));
                }
            }
            if let Err(e) = std::fs::write(&dest, content) {
                return runtime_error(&format!("{}: {e}", dest.display()));
            }
            written.push(relative_path);
        }
    }

    for relative_path in &written {
        println!("  compiled   {relative_path}");
    }
    let mut payload = Value::object();
    payload.set(
        "selected_skills",
        Value::Array(selected.iter().map(|s| Value::str(*s)).collect()),
    );
    payload.set(
        "written_paths",
        Value::Array(written.iter().map(|p| Value::str(p.clone())).collect()),
    );
    match telemetry::event_payload_from_generated_artifacts(&repo) {
        Ok(Value::Object(entries)) => {
            for (key, value) in entries {
                payload.set(&key, value);
            }
        }
        Ok(_) => {}
        Err(message) => return runtime_error(&message),
    }
    if let Err(code) = append_event_or_error(&repo, "repo.compile-skills", payload) {
        return code;
    }
    println!(
        "Compiled repo skills for: {}",
        py_path_str(&parsed.positionals[0])
    );
    0
}

// ── overrides ───────────────────────────────────────────────────────────────

fn run_init_override(
    rest: &[String],
    override_path: &str,
    template: Value,
    event_type: &str,
) -> u8 {
    let parsed = match parse_args(rest, &["--force"], &[]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    let force = parsed.has_flag("--force");
    let dest = repo.join(override_path);
    if dest.exists() && !force {
        return runtime_error(&format!(
            "{override_path} already exists. Use --force to overwrite."
        ));
    }
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return runtime_error(&format!("{}: {e}", parent.display()));
        }
    }
    if let Err(e) = std::fs::write(&dest, format!("{}\n", pyjson::dumps_indent2(&template))) {
        return runtime_error(&format!("{}: {e}", dest.display()));
    }
    let mut payload = Value::object();
    payload.set("force", Value::Bool(force));
    payload.set("path", Value::str(override_path));
    if let Err(code) = append_event_or_error(&repo, event_type, payload) {
        return code;
    }
    println!("Wrote {override_path}");
    0
}

fn run_init_onboarding_overrides(rest: &[String]) -> u8 {
    run_init_override(
        rest,
        onboarding::ONBOARDING_OVERRIDE_PATH,
        onboarding::override_template(),
        "repo.init-onboarding-overrides",
    )
}

fn run_init_agents_overrides(rest: &[String]) -> u8 {
    run_init_override(
        rest,
        AGENTS_OVERRIDE_PATH,
        agents::agents_override_template(),
        "repo.init-agents-overrides",
    )
}

fn run_validate_override(rest: &[String], result: impl Fn(&Path) -> Value, event_type: &str) -> u8 {
    let parsed = match parse_args(rest, &[], &[]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    let result = result(&repo);
    let mut payload = Value::object();
    for key in ["ok", "exists", "errors"] {
        payload.set(key, result.get(key).cloned().unwrap_or(Value::Null));
    }
    if let Err(code) = append_event_or_error(&repo, event_type, payload) {
        return code;
    }
    let ok = result.get("ok").map(Value::truthy).unwrap_or(false);
    let exists = result.get("exists").map(Value::truthy).unwrap_or(false);
    let path = result.get("path").map(Value::py_str).unwrap_or_default();
    if ok {
        if exists {
            println!("Valid override file: {path}");
        } else {
            println!("No override file present: {path}");
        }
        return 0;
    }
    eprintln!("Invalid override file: {path}");
    if let Some(errors) = result.get("errors").and_then(Value::as_array) {
        for error in errors {
            eprintln!("  - {}", error.py_str());
        }
    }
    1
}

fn run_validate_onboarding_overrides(rest: &[String]) -> u8 {
    run_validate_override(
        rest,
        |repo| onboarding::validate_overrides(repo),
        "repo.validate-onboarding-overrides",
    )
}

fn run_validate_agents_overrides(rest: &[String]) -> u8 {
    run_validate_override(
        rest,
        |repo| agents::validate_agents_overrides(repo),
        "repo.validate-agents-overrides",
    )
}

// ── telemetry ───────────────────────────────────────────────────────────────

/// Best-effort ledger note for hook arg-parse failures: the deployed
/// hooks call this command behind `>/dev/null 2>&1 || true`, so a usage
/// error would otherwise vanish without a trace (plan Phase 3 risk).
fn log_wrapper_arg_failure(rest: &[String], message: &str) {
    let Some(repo) = rest
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        return;
    };
    let mut payload = Value::object();
    payload.set("error", Value::str(message));
    payload.set(
        "argv",
        Value::Array(rest.iter().map(|a| Value::str(a.clone())).collect()),
    );
    let _ = telemetry::append_event(&repo, "wrapper.invocation-error", payload);
}

fn run_record_wrapper_invocation(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &[], &["--wrapper", "--detail"]) {
        Ok(parsed) => parsed,
        Err(message) => {
            log_wrapper_arg_failure(rest, &message);
            return usage_error(&message);
        }
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => {
            log_wrapper_arg_failure(rest, &message);
            return usage_error(&message);
        }
    };
    let Some(wrapper_name) = parsed.last_option("--wrapper") else {
        let message = "Missing option '--wrapper'.";
        log_wrapper_arg_failure(rest, message);
        return usage_error(message);
    };
    let mut details = Value::object();
    for item in parsed.option_values("--detail") {
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        details.set(key, Value::str(value));
    }
    let details = if details.truthy() { Some(details) } else { None };
    match telemetry::record_wrapper_invocation(&repo, wrapper_name, details) {
        Ok(()) => 0,
        Err(message) => runtime_error(&message),
    }
}

fn experience_report_has_attention(report: &Value) -> bool {
    report
        .get("kpis")
        .and_then(|kpis| kpis.get("signals"))
        .and_then(Value::as_array)
        .map(|signals| {
            signals
                .iter()
                .any(|signal| signal.get("status").and_then(Value::as_str) == Some("attention"))
        })
        .unwrap_or(false)
}

/// `sorted(dict.items())` over an ordered pyjson object (key sort;
/// UTF-8 byte order equals code-point order).
fn sorted_entries(value: &Value) -> Vec<(&String, &Value)> {
    let mut entries: Vec<(&String, &Value)> = value
        .as_object()
        .map(|entries| entries.iter().map(|(k, v)| (k, v)).collect())
        .unwrap_or_default();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

fn join_str_list(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(Value::py_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn run_experience_telemetry(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &["--json-output", "--check"], &[]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    let check_signals = parsed.has_flag("--check");
    let report = match telemetry::detailed_report(&repo) {
        Ok(report) => report,
        Err(message) => return runtime_error(&message),
    };
    if parsed.has_flag("--json-output") {
        println!("{}", pyjson::dumps_indent2(&report));
        if check_signals && experience_report_has_attention(&report) {
            return 1;
        }
        return 0;
    }

    let field = |key: &str| report.get(key).cloned().unwrap_or(Value::Null);
    println!("Path: {}", field("path").py_str());
    println!(
        "Exists: {}",
        if field("exists").truthy() { "yes" } else { "no" }
    );
    println!("Events: {}", field("event_count").py_str());
    println!("Last event: {}", {
        let last = field("last_event_type");
        if last.truthy() {
            last.py_str()
        } else {
            "none".to_string()
        }
    });
    let by_type = field("by_type");
    if by_type.truthy() {
        println!("By type:");
        for (event_type, count) in sorted_entries(&by_type) {
            println!("- {event_type}: {}", count.py_str());
        }
    }
    let wrapper_invocations = field("wrapper_invocations");
    if wrapper_invocations.truthy() {
        println!("Wrapper invocations:");
        for (wrapper_name, count) in sorted_entries(&wrapper_invocations) {
            println!("- {wrapper_name}: {}", count.py_str());
        }
    }
    let empty = Value::object();
    let freshness = match report.get("freshness") {
        Some(v) if v.truthy() => v.clone(),
        _ => empty.clone(),
    };
    if report.get("kpis").map(Value::truthy).unwrap_or(false) {
        let kpis = report.get("kpis").expect("checked above");
        let kpi = |key: &str| kpis.get(key).cloned().unwrap_or(Value::Null);
        println!("KPIs:");
        println!("- wrapper_total: {}", kpi("wrapper_total").py_str());
        println!(
            "- onboarding_commands: {}",
            kpi("onboarding_commands").py_str()
        );
        println!("- onboarding_notes: {}", kpi("onboarding_notes").py_str());
        println!("- act_has_fast_test: {}", kpi("act_has_fast_test").py_str());
        println!(
            "- override_regeneration_required: {}",
            kpi("override_regeneration_required").py_str()
        );
        if freshness
            .get("override_exists")
            .map(Value::truthy)
            .unwrap_or(false)
        {
            let joined = join_str_list(kpis.get("stale_targets"));
            println!(
                "- stale_targets: {}",
                if joined.is_empty() { "none".to_string() } else { joined }
            );
        }
        let signals = kpi("signals");
        if signals.truthy() {
            println!("Signals:");
            if let Some(items) = signals.as_array() {
                for signal in items {
                    println!(
                        "- {} {}: {}",
                        signal.get("status").unwrap_or(&Value::Null).py_str(),
                        signal.get("code").unwrap_or(&Value::Null).py_str(),
                        signal.get("message").unwrap_or(&Value::Null).py_str(),
                    );
                }
            }
        }
        let suggestions = kpi("suggestions");
        if suggestions.truthy() {
            println!("Suggestions:");
            if let Some(items) = suggestions.as_array() {
                for suggestion in items {
                    println!(
                        "- {}: {}",
                        suggestion.get("code").unwrap_or(&Value::Null).py_str(),
                        suggestion.get("message").unwrap_or(&Value::Null).py_str(),
                    );
                }
            }
        }
    }
    if freshness
        .get("override_exists")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        println!("Override freshness:");
        println!(
            "- regeneration_required: {}",
            if freshness
                .get("regeneration_required")
                .map(Value::truthy)
                .unwrap_or(false)
            {
                "yes"
            } else {
                "no"
            }
        );
        if freshness
            .get("stale_targets")
            .map(Value::truthy)
            .unwrap_or(false)
        {
            println!(
                "- stale_targets: {}",
                join_str_list(freshness.get("stale_targets"))
            );
        }
    }
    let recent_events = field("recent_events");
    if recent_events.truthy() {
        println!("Recent events:");
        if let Some(items) = recent_events.as_array() {
            let tail_start = items.len().saturating_sub(5);
            for event in &items[tail_start..] {
                println!(
                    "- {} {}",
                    event.get("timestamp").unwrap_or(&Value::Null).py_str(),
                    event.get("event_type").unwrap_or(&Value::Null).py_str(),
                );
            }
        }
    }
    if check_signals && experience_report_has_attention(&report) {
        return 1;
    }
    0
}

fn run_experience_status(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &["--json-output"], &[]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let repo = match require_repo_dir(&parsed) {
        Ok(repo) => repo,
        Err(message) => return usage_error(&message),
    };
    let status = match telemetry::write_status_artifacts(&repo) {
        Ok(status) => status,
        Err(message) => return runtime_error(&message),
    };
    if parsed.has_flag("--json-output") {
        println!("{}", pyjson::dumps_indent2(&status));
        return 0;
    }

    let empty = Value::object();
    let enhancement = status.get("enhancement").unwrap_or(&empty);
    let artifacts = status.get("artifacts").unwrap_or(&empty);
    let recommendation = status.get("recommended_next_action").unwrap_or(&empty);
    let py = |value: &Value, key: &str| -> String {
        match value.get(key) {
            Some(Value::Bool(b)) => py_bool(*b).to_string(),
            Some(other) => other.py_str(),
            None => "None".to_string(),
        }
    };
    println!("Experience status:");
    println!(
        "  Enhancement: installed={}, verified={}",
        py(enhancement, "installed"),
        py(enhancement, "verified"),
    );
    println!(
        "  Artifacts: onboarding={}, act={}, override_exists={}, regeneration_required={}",
        py(artifacts, "onboarding_present"),
        py(artifacts, "act_present"),
        py(artifacts, "override_exists"),
        py(artifacts, "override_regeneration_required"),
    );
    println!(
        "  Recommended next action: {}",
        py(recommendation, "command")
    );
    println!("  Reason: {}", py(recommendation, "reason"));
    println!(
        "  Wrote: .aethyme/generated/experience-status.json, .aethyme/generated/experience-status.md"
    );
    0
}

// ── commit hygiene ──────────────────────────────────────────────────────────

/// Hidden hook helper: wrap stdin in the SessionStart hook envelope.
///
/// python-retirement Phase 6 (2026-08-01). The deployed
/// `.claude/hooks/aethyme-load-context.sh` used a bare `python3`
/// heredoc to JSON-escape the collected AGENTS.md/CLAUDE.md text — the
/// last Python invocation on the product path. This is that heredoc,
/// natively: read the context from stdin, print
/// `{"hookSpecificOutput": {"hookEventName": …, "additionalContext": …}}`.
///
/// Stdin rather than argv on purpose. The heredoc passed the context as
/// `sys.argv[1]`, which put a whole AGENTS.md through the process
/// argument list — a large repo's guidance could hit `E2BIG` and take
/// context injection down with it. A pipe has no such ceiling and
/// preserves the bytes exactly.
///
/// Output is byte-identical to `json.dumps(...)`: `pyjson::dumps_compact`
/// applies Python's default separators and `ensure_ascii` escaping, so
/// deployed hooks emit the same envelope before and after the flip.
fn run_hook_envelope(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &[], &["--event"]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    if let Some(extra) = parsed.positionals.first() {
        return usage_error(&format!("Got unexpected extra argument ({extra})"));
    }
    let event = parsed.last_option("--event").unwrap_or("SessionStart");

    let mut context = String::new();
    if let Err(error) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut context) {
        return runtime_error(&format!("could not read hook context from stdin: {error}"));
    }
    // Nothing to inject: emit nothing and succeed. The hook protocol
    // treats empty stdout as "no context", so this keeps the shell's
    // `exit 0` short-circuit and the empty-input case identical.
    if context.is_empty() {
        return 0;
    }

    let mut inner = Value::object();
    inner.set("hookEventName", Value::str(event));
    inner.set("additionalContext", Value::str(context));
    let mut envelope = Value::object();
    envelope.set("hookSpecificOutput", inner);
    println!("{}", pyjson::dumps_compact(&envelope));
    0
}

fn run_commit_message_template(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &[], &["--type", "--scope"]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    if let Some(extra) = parsed.positionals.first() {
        return usage_error(&format!("Got unexpected extra argument ({extra})"));
    }
    let commit_type = parsed.last_option("--type").unwrap_or("fix");
    if !hygiene::ALLOWED_TYPES.contains(&commit_type) {
        let quoted: Vec<String> = hygiene::ALLOWED_TYPES
            .iter()
            .map(|t| format!("'{t}'"))
            .collect();
        return usage_error(&format!(
            "Invalid value for '--type': '{commit_type}' is not one of {}.",
            quoted.join(", ")
        ));
    }
    let scope = parsed.last_option("--scope").unwrap_or("scope");
    // Python: click.echo(template, nl=False) — template carries its own
    // trailing newline.
    print!("{}", hygiene::default_template(commit_type, scope));
    0
}

fn run_lint_commit_message(rest: &[String]) -> u8 {
    let parsed = match parse_args(rest, &["--json-output"], &["--message"]) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(&message),
    };
    let message_path = parsed.positionals.first();
    let inline_message = parsed.last_option("--message");
    if let Some(raw) = message_path {
        let path = PathBuf::from(raw);
        if !path.exists() {
            return usage_error(&format!(
                "Invalid value for 'MESSAGE_PATH': File '{raw}' does not exist."
            ));
        }
        if path.is_dir() {
            return usage_error(&format!(
                "Invalid value for 'MESSAGE_PATH': File '{raw}' is a directory."
            ));
        }
    }
    if message_path.is_some() && inline_message.is_some() {
        return runtime_error("Provide either MESSAGE_PATH or --message, not both.");
    }
    let message = if let Some(raw) = message_path {
        match std::fs::read_to_string(raw) {
            Ok(text) => text,
            Err(e) => return runtime_error(&format!("{raw}: {e}")),
        }
    } else if let Some(inline) = inline_message {
        inline.to_string()
    } else {
        use std::io::Read;
        let mut buffer = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buffer) {
            return runtime_error(&format!("stdin: {e}"));
        }
        buffer
    };

    let result = hygiene::lint_commit_message(&message);
    let ok = result.get("ok").map(Value::truthy).unwrap_or(false);
    if parsed.has_flag("--json-output") {
        println!("{}", pyjson::dumps_indent2(&result));
        return if ok { 0 } else { 1 };
    }

    println!("Valid: {}", if ok { "yes" } else { "no" });
    if let Some(subject @ Value::Object(_)) = result.get("subject") {
        println!(
            "Type: {}",
            subject.get("type").unwrap_or(&Value::Null).py_str()
        );
        println!("Scope: {}", {
            let scope = subject.get("scope").cloned().unwrap_or(Value::Null);
            if scope.truthy() {
                scope.py_str()
            } else {
                "none".to_string()
            }
        });
        println!(
            "Summary: {}",
            subject.get("summary").unwrap_or(&Value::Null).py_str()
        );
    }
    println!(
        "Body required: {}",
        if result
            .get("body_required")
            .map(Value::truthy)
            .unwrap_or(false)
        {
            "yes"
        } else {
            "no"
        }
    );
    let recognized = join_str_list(result.get("recognized_sections"));
    println!(
        "Sections: {}",
        if recognized.is_empty() { "none".to_string() } else { recognized }
    );
    if let Some(candidates) = result.get("memory_candidates").and_then(Value::as_array) {
        if !candidates.is_empty() {
            println!("Memory candidates:");
            for candidate in candidates {
                println!(
                    "- {}: {}",
                    candidate.get("type").unwrap_or(&Value::Null).py_str(),
                    candidate.get("summary").unwrap_or(&Value::Null).py_str(),
                );
            }
        }
    }
    if let Some(warnings) = result.get("warnings").and_then(Value::as_array) {
        if !warnings.is_empty() {
            println!("Warnings:");
            for warning in warnings {
                println!("- {}", warning.py_str());
            }
        }
    }
    if let Some(errors) = result.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            println!("Errors:");
            for error in errors {
                println!("- {}", error.py_str());
            }
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_path_str_matches_pathlib_normalization() {
        assert_eq!(py_path_str("/tmp//demo/"), "/tmp/demo");
        assert_eq!(py_path_str("./x"), "x");
        assert_eq!(py_path_str("a/./b"), "a/b");
        assert_eq!(py_path_str("."), ".");
        assert_eq!(py_path_str("/"), "/");
        assert_eq!(py_path_str("x"), "x");
    }

    #[test]
    fn parse_args_handles_inline_values_and_unknown_options() {
        let args: Vec<String> = ["--wrapper=hook", "repo", "--detail", "k=v"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_args(&args, &[], &["--wrapper", "--detail"]).unwrap();
        assert_eq!(parsed.positionals, vec!["repo"]);
        assert_eq!(parsed.last_option("--wrapper"), Some("hook"));
        assert_eq!(parsed.option_values("--detail"), vec!["k=v"]);

        let bad: Vec<String> = vec!["--nope".to_string()];
        assert!(parse_args(&bad, &[], &[]).is_err());
    }
}
