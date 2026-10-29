//! `aethyme env`: environment profiles (#1209).
//!
//! `validate` and `plan` are read-only: neither spawns a process nor writes
//! host state. `plan` asks the host-resource coordinator for the same
//! advisory plan `broker advanced resources plan` prints, so there is exactly
//! one allocator.
//!
//! `--json` prints one object whose `schema` names its shape
//! (`aethyme.env-<command>/v1`); a refusal is `aethyme.env-error/v1` with
//! `code` and `next_action`.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::compile::{
    CompileContext, CompiledEnvironment, EnvironmentOwner, compile, default_profile_name,
};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::validate::{Diagnostic, validate};
use super::{ENVIRONMENTS_FILE, load_host_file, load_repository_file};
use crate::exit_status;

pub const VALIDATE_SCHEMA: &str = "aethyme.env-validate/v1";
pub const PLAN_SCHEMA: &str = "aethyme.env-plan/v1";
pub const ERROR_SCHEMA: &str = "aethyme.env-error/v1";

pub const USAGE: &str = "\
aethyme env — declared runnable environments per worktree

Environments are declared in .aethyme/environments.toml: profiles made of
components (process, compose, container, static, hooks), each with its own
commands, health check and ports. Host pool limits live in
<host state>/environments.toml.
Every subcommand takes --json (one versioned object) and --repo <path>.

Usage:
  aethyme env validate [--json]
      Check the environments file statically: schema, unknown keys, runtime
      fields, depends_on cycles, health checks for routed components, pool
      limits and rule globs. Spawns nothing and writes nothing.
  aethyme env plan [--profile <name>] [--json]
      Compile a profile (default: default_profile, else the first) and print
      its component order, the pinned snapshot digest, and the reservation
      and capacity requests with the host-resource coordinator's advisory
      plan for each. Read-only; a later acquire may race and is authoritative.
";

struct Failure {
    code: &'static str,
    message: String,
    next_action: String,
    exit: u8,
}

struct Options {
    json: bool,
    repo: Option<PathBuf>,
    profile: Option<String>,
}

pub fn run(args: &[String]) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return exit_status::USAGE;
    };
    // Help anywhere answers help and does nothing else, so the router can
    // pass any `env ... --help` line through unchanged.
    if command == "help" || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return exit_status::SUCCESS;
    }
    let options = match parse_options(command, rest) {
        Ok(options) => options,
        Err(failure) => return report(false, failure),
    };
    let json = options.json;
    let result = match command.as_str() {
        "validate" => run_validate(&options),
        "plan" => run_plan(&options),
        other => Err(Failure {
            code: "unknown_subcommand",
            message: format!("unknown env subcommand {other:?}"),
            next_action: "run `aethyme env --help`".into(),
            exit: exit_status::USAGE,
        }),
    };
    match result {
        Ok((value, text, exit)) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            } else {
                print!("{text}");
            }
            exit
        }
        Err(failure) => report(json, failure),
    }
}

fn parse_options(command: &str, rest: &[String]) -> Result<Options, Failure> {
    let usage = |message: String| Failure {
        code: "usage",
        message,
        next_action: "run `aethyme env --help`".into(),
        exit: exit_status::USAGE,
    };
    let mut options = Options {
        json: false,
        repo: None,
        profile: None,
    };
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--repo" => {
                options.repo = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| usage("--repo needs a path".into()))?,
                ))
            }
            "--profile" if command == "plan" => {
                options.profile = Some(
                    args.next()
                        .ok_or_else(|| usage("--profile needs a name".into()))?
                        .clone(),
                )
            }
            other => return Err(usage(format!("unexpected argument {other:?} for env {command}"))),
        }
    }
    Ok(options)
}

fn report(json: bool, failure: Failure) -> u8 {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": ERROR_SCHEMA,
                "code": failure.code,
                "message": failure.message,
                "next_action": failure.next_action,
            }))
            .unwrap_or_default()
        );
    } else {
        eprintln!("Error: {} ({})", failure.message, failure.code);
        eprintln!("next: {}", failure.next_action);
    }
    failure.exit
}

struct Workspace {
    worktree_root: PathBuf,
    repo: crate::GitRepo,
}

fn workspace(options: &Options) -> Result<Workspace, Failure> {
    let start = match &options.repo {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|error| environment_failure(error.to_string()))?,
    };
    let repo = crate::GitRepo::discover(&start).map_err(|error| Failure {
        code: "not_a_checkout",
        message: format!("env requires a git checkout: {error}"),
        next_action: "run inside a repository checkout or pass --repo <path>".into(),
        exit: exit_status::ENVIRONMENT,
    })?;
    Ok(Workspace {
        worktree_root: repo.root().to_path_buf(),
        repo,
    })
}

fn environment_failure(message: String) -> Failure {
    Failure {
        code: "io_error",
        message,
        next_action: "check file permissions and retry".into(),
        exit: exit_status::ENVIRONMENT,
    }
}

/// Everything `validate` and `plan` share: both files parsed and checked.
struct Checked {
    file: Option<EnvironmentsFile>,
    host: HostEnvironmentsFile,
    host_label: String,
    digest: String,
    diagnostics: Vec<Diagnostic>,
}

fn check(workspace: &Workspace) -> Result<Checked, Failure> {
    let loaded = load_repository_file(&workspace.worktree_root)
        .map_err(|error| environment_failure(format!("{ENVIRONMENTS_FILE}: {error}")))?
        .ok_or_else(|| Failure {
            code: "not_configured",
            message: format!("{ENVIRONMENTS_FILE} does not exist in this checkout"),
            next_action: format!(
                "create {ENVIRONMENTS_FILE} with schema = 1 and at least one [[profiles]] table"
            ),
            exit: exit_status::REFUSED,
        })?;
    let (host_label, host_loaded) =
        load_host_file().map_err(|error| environment_failure(format!("host settings: {error}")))?;
    let mut diagnostics = Vec::new();
    let host = match host_loaded.value {
        Ok(host) => host,
        Err(diagnostic) => {
            diagnostics.push(*diagnostic);
            HostEnvironmentsFile::default()
        }
    };
    let file = match loaded.value {
        Ok(file) => {
            diagnostics.extend(validate(&file, &host, ENVIRONMENTS_FILE, &host_label));
            Some(file)
        }
        Err(diagnostic) => {
            diagnostics.push(*diagnostic);
            None
        }
    };
    Ok(Checked {
        file,
        host,
        host_label,
        digest: loaded.digest,
        diagnostics,
    })
}

type Output = (Value, String, u8);

fn run_validate(options: &Options) -> Result<Output, Failure> {
    let workspace = workspace(options)?;
    let checked = check(&workspace)?;
    let valid = checked.diagnostics.is_empty();
    let profiles: Vec<&str> = checked
        .file
        .iter()
        .flat_map(|file| file.profiles.iter().map(|p| p.name.as_str()))
        .collect();
    let value = json!({
        "schema": VALIDATE_SCHEMA,
        "file": ENVIRONMENTS_FILE,
        "host_file": checked.host_label,
        "file_digest": checked.digest,
        "valid": valid,
        "profiles": profiles,
        "diagnostics": checked.diagnostics,
    });
    let mut text = String::new();
    if valid {
        text.push_str(&format!(
            "{ENVIRONMENTS_FILE}: valid ({} profile{}: {})\n",
            profiles.len(),
            if profiles.len() == 1 { "" } else { "s" },
            profiles.join(", ")
        ));
    } else {
        for diagnostic in &checked.diagnostics {
            text.push_str(&diagnostic.render());
            text.push('\n');
        }
        text.push_str(&format!(
            "{} problem{} found\n",
            checked.diagnostics.len(),
            if checked.diagnostics.len() == 1 { "" } else { "s" }
        ));
    }
    let exit = if valid {
        exit_status::SUCCESS
    } else {
        exit_status::VERIFICATION_FAILED
    };
    Ok((value, text, exit))
}

fn run_plan(options: &Options) -> Result<Output, Failure> {
    let workspace = workspace(options)?;
    let checked = check(&workspace)?;
    let file = match (&checked.file, checked.diagnostics.is_empty()) {
        (Some(file), true) => file,
        _ => {
            let first = checked
                .diagnostics
                .first()
                .map(Diagnostic::render)
                .unwrap_or_default();
            return Err(Failure {
                code: "invalid_environments_file",
                message: format!(
                    "{} validation problem(s); first: {first}",
                    checked.diagnostics.len()
                ),
                next_action: "run `aethyme env validate` and fix every problem".into(),
                exit: exit_status::VERIFICATION_FAILED,
            });
        }
    };
    let profile = options
        .profile
        .as_deref()
        .or_else(|| default_profile_name(file))
        .unwrap_or_default()
        .to_string();
    let context = compile_context(&workspace, &checked.digest);
    let compiled = compile(
        file,
        &checked.host,
        &profile,
        ENVIRONMENTS_FILE,
        &context,
    )
    .map_err(|error| Failure {
        code: "unknown_profile",
        message: error.to_string(),
        next_action: format!(
            "pass --profile with one of: {}",
            file.profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        exit: exit_status::USAGE,
    })?;
    let plans = advisory_plans(&compiled)?;
    Ok(render_plan(&compiled, plans))
}

fn compile_context(workspace_root: &Workspace, file_digest: &str) -> CompileContext {
    let repo = &workspace_root.repo;
    let fingerprint = crate::console::worktree_fingerprint(&workspace_root.worktree_root);
    let commit = repo.head_commit().ok();
    let committed = commit
        .as_deref()
        .and_then(|commit| repo.blob_at(commit, ENVIRONMENTS_FILE));
    let dirty = match committed {
        Some(text) => format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes()))
            != file_digest,
        None => true,
    };
    CompileContext {
        repository: crate::gates::git_origin_fingerprint(repo),
        owner: EnvironmentOwner::Worktree {
            fingerprint: fingerprint.clone(),
        },
        worktree_fingerprint: fingerprint,
        source_commit: commit,
        source_dirty: dirty,
        source_digest: file_digest.to_string(),
    }
}

struct AdvisoryPlans {
    reservation: Value,
    capacity: Option<Value>,
    host_state: &'static str,
}

/// Plans come from the coordinator itself, never recomputed here. When host
/// state does not exist yet the plan is unknown, not "available".
fn advisory_plans(compiled: &CompiledEnvironment) -> Result<AdvisoryPlans, Failure> {
    let reservation = compiled.reservation_request(None);
    let capacity = compiled.capacity_request(None);
    let path = crate::resources::default_host_resource_db_path().map_err(|error| Failure {
        code: "host_state_unavailable",
        message: error.to_string(),
        next_action: "set AETHYME_HOST_STATE_DIR or HOME".into(),
        exit: exit_status::ENVIRONMENT,
    })?;
    if !path.exists() {
        return Ok(AdvisoryPlans {
            reservation: Value::Null,
            capacity: capacity.as_ref().map(|_| Value::Null),
            host_state: "absent",
        });
    }
    let coordinator =
        crate::resources::HostResourceCoordinator::open_read_only(&path).map_err(|error| {
            Failure {
                code: "host_state_unavailable",
                message: error.to_string(),
                next_action: "inspect with `aethyme broker advanced resources list`".into(),
                exit: exit_status::ENVIRONMENT,
            }
        })?;
    let plan = |request| {
        coordinator
            .plan(request)
            .map(|plan| serde_json::to_value(plan).unwrap_or_default())
            .map_err(|error| Failure {
                code: "invalid_request",
                message: error.to_string(),
                next_action: "report this: a validated profile compiled to an invalid request"
                    .into(),
                exit: exit_status::FAILED,
            })
    };
    Ok(AdvisoryPlans {
        reservation: plan(&reservation)?,
        capacity: capacity.as_ref().map(plan).transpose()?,
        host_state: "present",
    })
}

fn render_plan(compiled: &CompiledEnvironment, plans: AdvisoryPlans) -> Output {
    let components: Vec<Value> = compiled
        .components
        .iter()
        .map(|component| {
            json!({
                "name": component.name,
                "runtime": component.runtime,
                "depends_on": component.depends_on,
                "port_key": component.port_key,
                "routed": component.routed,
                "variables": compiled.variable_names(component),
            })
        })
        .collect();
    let reservation = compiled.reservation_request(None);
    let capacity = compiled.capacity_request(None);
    let value = json!({
        "schema": PLAN_SCHEMA,
        "env_id": compiled.env_id,
        "profile": compiled.profile,
        "rank": compiled.rank,
        "units": compiled.units,
        "pool": compiled.pool,
        "idle_seconds": compiled.idle_seconds,
        "snapshot_digest": compiled.digest,
        "source": compiled.source,
        "start_order": compiled.components.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        "components": components,
        "host_state": plans.host_state,
        "requests": {
            "reservation": reservation,
            "capacity": capacity,
        },
        "plans": {
            "reservation": plans.reservation,
            "capacity": plans.capacity,
        },
    });
    let mut text = format!(
        "environment {} (profile {}, rank {}, {} unit{})\n",
        compiled.env_id,
        compiled.profile,
        compiled.rank,
        compiled.units,
        if compiled.units == 1 { "" } else { "s" }
    );
    if let Some(pool) = &compiled.pool {
        text.push_str(&format!(
            "  pool: {} ({:?} scope, limit {}, lease pool {})\n",
            pool.name, pool.scope, pool.limit, pool.lease_pool
        ));
    }
    text.push_str(&format!("  snapshot: {}\n", compiled.digest));
    text.push_str("  start order:\n");
    for component in &compiled.components {
        text.push_str(&format!(
            "    {} ({}){}{}\n",
            component.name,
            component.runtime.as_str(),
            component
                .port_key
                .as_ref()
                .map(|key| format!(", port {key}"))
                .unwrap_or_default(),
            if component.depends_on.is_empty() {
                String::new()
            } else {
                format!(", after {}", component.depends_on.join(", "))
            }
        ));
    }
    match plans.host_state {
        "absent" => text.push_str(
            "  host plan: unknown (no host resource state yet; nothing has been reserved on this host)\n",
        ),
        _ => {
            text.push_str(&format!("  reservation: {}\n", summarize(&plans.reservation)));
            if let Some(capacity) = &plans.capacity {
                text.push_str(&format!("  capacity: {}\n", summarize(capacity)));
            }
        }
    }
    (value, text, exit_status::SUCCESS)
}

fn summarize(plan: &Value) -> String {
    let available = plan["available"].as_bool().unwrap_or(false);
    let proposed: Vec<String> = plan["proposed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|allocation| {
            format!(
                "{}={}",
                allocation["key"].as_str().unwrap_or("?"),
                allocation["value"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let conflicts: Vec<String> = plan["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|conflict| {
            format!(
                "{} {}: {}",
                conflict["resource_key"].as_str().unwrap_or("?"),
                conflict["code"].as_str().unwrap_or("?"),
                conflict["reason"].as_str().unwrap_or("?")
            )
        })
        .collect();
    if available {
        format!("available ({})", proposed.join(", "))
    } else {
        format!("blocked ({})", conflicts.join("; "))
    }
}

