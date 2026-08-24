//! Native `aethyme enhance` front end. Stdout is byte-compatible with
//! the deleted Click commands it replaced (`enhance_deploy_command` /
//! `enhance_verify_command`, `src/cli.py` before the Phase 2 flip),
//! verified by the `scripts/migration/enhance-golden.sh` stdout
//! comparison. The router dispatches here unconditionally.

use std::path::{Path, PathBuf};

use crate::deploy::{deploy, is_ok, refresh_status, summarize, verify};
use crate::pyjson::{Value, py_bool};
use crate::telemetry::append_event;

/// Run `enhance <subcommand> ...` with `args` excluding the leading
/// `enhance`. Prints its own output/errors; returns the process exit
/// code. Templates and runtime commands are embedded, so this command does
/// not require an Aethyme source checkout.
pub fn run(args: &[String]) -> u8 {
    match args.first().map(String::as_str) {
        Some("deploy") => run_deploy(&args[1..]),
        Some("verify") => run_verify(&args[1..]),
        other => {
            eprintln!(
                "Error: unsupported enhance subcommand: {}",
                other.unwrap_or("<none>")
            );
            2
        }
    }
}

struct RepoArg {
    /// The path exactly as given (echoed in `Enhanced: {repo}`).
    given: PathBuf,
}

fn parse_repo_arg(subcommand: &str, rest: &[String]) -> Result<(RepoArg, bool), (String, u8)> {
    let mut repo: Option<String> = None;
    let mut force = false;
    let mut i = 0usize;
    while i < rest.len() {
        let arg = &rest[i];
        if arg == "--repo" {
            let Some(value) = rest.get(i + 1) else {
                return Err(("Option '--repo' requires an argument.".to_string(), 2));
            };
            repo = Some(value.clone());
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--repo=") {
            repo = Some(value.to_string());
            i += 1;
        } else if arg == "--force" {
            force = true;
            i += 1;
        } else {
            return Err((format!("No such option: {arg}"), 2));
        }
    }
    let Some(repo) = repo else {
        return Err((
            format!("Missing option '--repo' for 'enhance {subcommand}'."),
            2,
        ));
    };
    let path = PathBuf::from(&repo);
    if !path.exists() {
        return Err((
            format!("Invalid value for '--repo': Directory '{repo}' does not exist."),
            2,
        ));
    }
    if !path.is_dir() {
        return Err((
            format!("Invalid value for '--repo': Directory '{repo}' is a file."),
            2,
        ));
    }
    Ok((RepoArg { given: path }, force))
}

fn run_deploy(rest: &[String]) -> u8 {
    let (repo, force) = match parse_repo_arg("deploy", rest) {
        Ok(parsed) => parsed,
        Err((message, code)) => {
            eprintln!("Error: {message}");
            return code;
        }
    };
    let actions = match deploy(&repo.given, force) {
        Ok(actions) => actions,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };
    for action in &actions {
        // Python: f"  {a.action:9}  {a.target.relative_path}"
        println!("  {:<9}  {}", action.action, action.relative_path);
    }
    println!("Enhanced: {}", repo.given.display());
    0
}

fn run_verify(rest: &[String]) -> u8 {
    let (repo, _force) = match parse_repo_arg("verify", rest) {
        Ok(parsed) => parsed,
        Err((message, code)) => {
            eprintln!("Error: {message}");
            return code;
        }
    };
    let results = match verify(&repo.given) {
        Ok(results) => results,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };
    for r in &results {
        let ok = r.exists && !r.placeholder_present;
        let strict = matches!(r.relative_path.as_str(), "AGENTS.md" | "CLAUDE.md");
        let strict_direct_edit = strict && r.exists && !r.matches_canonical;
        let marker = if strict_direct_edit {
            "FAIL"
        } else if ok {
            "OK"
        } else {
            "FAIL"
        };
        let mut notes: Vec<&str> = Vec::new();
        if !r.exists {
            notes.push("missing");
        } else if r.placeholder_present {
            notes.push("placeholder not substituted");
        }
        if r.exists && !r.matches_canonical {
            if strict {
                notes.push("direct edits unsupported; use .aethyme/overrides/agents.json");
            } else {
                notes.push("content drift (allowed)");
            }
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        // Python: f"  [{marker:4}] {path}{suffix}"
        println!("  [{marker:<4}] {}{suffix}", r.relative_path);
    }
    if !is_ok(&results) {
        eprintln!("Verification failed.");
        return 1;
    }

    let summary = match summarize(&repo.given) {
        Ok(summary) => summary,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };
    let field =
        |value: &Value, key: &str| -> String { value.get(key).unwrap_or(&Value::Null).py_str() };
    let bool_field = |value: &Value, key: &str| -> String {
        match value.get(key) {
            Some(Value::Bool(b)) => py_bool(*b).to_string(),
            Some(other) => other.py_str(),
            None => "None".to_string(),
        }
    };

    let mut event_payload = Value::object();
    event_payload.set("ok", Value::Bool(true));
    event_payload.set(
        "recommended_skill",
        summary
            .get("recommended_skill")
            .cloned()
            .unwrap_or(Value::Null),
    );
    event_payload.set(
        "recommended_mode",
        summary
            .get("recommended_mode")
            .cloned()
            .unwrap_or(Value::Null),
    );
    event_payload.set(
        "experience_telemetry_before_write",
        summary
            .get("experience_telemetry")
            .cloned()
            .unwrap_or(Value::Null),
    );
    if let Err(message) = append_event(&repo.given, "enhance.verify", event_payload) {
        eprintln!("Error: {message}");
        return 1;
    }

    let empty = Value::object();
    let onboarding = summary.get("onboarding").unwrap_or(&empty);
    let act = summary.get("act").unwrap_or(&empty);
    let freshness = summary.get("freshness").unwrap_or(&empty);
    let experience = summary.get("experience_telemetry").unwrap_or(&empty);

    println!("Enhancement summary:");
    println!(
        "  Recommendation: load `{}` then run `{}`",
        field(&summary, "recommended_skill"),
        field(&summary, "recommended_mode"),
    );
    println!("  Reason: {}", field(&summary, "reason"));
    println!(
        "  Onboarding: commands={}, areas={}, entrypoints={}, notes={}, overrides={}",
        field(onboarding, "commands"),
        field(onboarding, "areas"),
        field(onboarding, "entrypoints"),
        field(onboarding, "notes"),
        bool_field(onboarding, "overrides_applied"),
    );
    println!(
        "  Act starter: fast_test={}, entrypoints={}, caution_zones={}",
        bool_field(act, "has_fast_test"),
        field(act, "entrypoints"),
        field(act, "caution_zones"),
    );
    if freshness
        .get("override_exists")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        let stale_joined = freshness
            .get("stale_targets")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(Value::py_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        println!(
            "  Override freshness: regeneration_required={}, stale_targets={}",
            bool_field(freshness, "regeneration_required"),
            if stale_joined.is_empty() {
                "none".to_string()
            } else {
                stale_joined
            },
        );
    }
    println!(
        "  Experience telemetry: events={}, last={}",
        field(experience, "event_count"),
        field(experience, "last_event_type"),
    );
    let status = match refresh_status(&repo.given) {
        Ok(status) => status,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };
    let next_command = status
        .get("recommended_next_action")
        .and_then(|r| r.get("command"))
        .unwrap_or(&Value::Null)
        .py_str();
    println!(
        "  Experience status: next=`{next_command}`, artifacts=.aethyme/generated/experience-status.json,.aethyme/generated/experience-status.md"
    );
    println!("All discoverability files present and substituted.");
    0
}
