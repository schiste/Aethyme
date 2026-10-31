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
pub const STATUS_SCHEMA: &str = "aethyme.env-status/v1";
pub const LIST_SCHEMA: &str = "aethyme.env-list/v1";
pub const HISTORY_SCHEMA: &str = "aethyme.env-history/v1";
pub const LOGS_SCHEMA: &str = "aethyme.env-logs/v1";
pub const SUPERVISE_SCHEMA: &str = "aethyme.env-supervise/v1";
pub const UP_SCHEMA: &str = "aethyme.env-up/v1";
pub const DOWN_SCHEMA: &str = "aethyme.env-down/v1";
pub const SWEEP_SCHEMA: &str = "aethyme.env-sweep/v1";
pub const PARK_SCHEMA: &str = "aethyme.env-park/v1";
pub const WAKE_SCHEMA: &str = "aethyme.env-wake/v1";
pub const TOUCH_SCHEMA: &str = "aethyme.env-touch/v1";
pub const PIN_SCHEMA: &str = "aethyme.env-pin/v1";
pub const ROUTES_SCHEMA: &str = "aethyme.env-routes/v1";
pub const INFER_SCHEMA: &str = "aethyme.env-infer/v1";
pub const SHARED_SCHEMA: &str = "aethyme.env-shared/v1";
pub const CALIBRATE_SCHEMA: &str = "aethyme.env-calibrate/v1";
pub const CACHE_SCHEMA: &str = "aethyme.env-cache/v1";

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
  aethyme env status [<id>] [--json]
      This worktree's environments (or one, by id or 8+ character prefix):
      phase, components with their identities, last error, recent
      transitions, and whether the host supervisor is running. Read-only.
  aethyme env list [--all] [--json]
      Environments of this repository (--all: every repository on this host).
  aethyme env history <id> [--limit N] [--json]
      The environment's recorded phase transitions, oldest first.
  aethyme env logs <id> [<component>] [--tail N] [--json]
      The last N (default 50) lines of each component log, or of one.
  aethyme env infer [--range <rev-range>] [--json]
      The minimum profile this worktree's changes need: [[rules]] globs over
      the diff against its base (committed, uncommitted and untracked, both
      sides of renames), the rules that fired with their paths, and the
      paths no rule matched. --range evaluates every commit in a range and
      prints how many would need each profile, for tuning rules.
  aethyme env up [--profile <name> [--reason <text>]] [--wait <seconds>] [--reconfigure] [--json]
      Make sure this worktree's environment is up: create it, or return it
      if already active. Reserves its namespace and ports, waits up to
      --wait (default 300) for pool capacity, provisions, starts and
      health-checks every component in dependency order, and prints its
      ports, URLs and variables. Without --profile it uses the inferred
      minimum (`env infer`); a lower profile is refused, a higher one needs
      --reason and is recorded. An environment of this worktree below the
      minimum is destroyed and replaced. It prints its
      ports, URLs and the AETHYME_ENV_* variables. A failure rolls back and
      leaves the environment `failed` with its error. A changed profile
      needs --reconfigure (destroy, then recreate).
  aethyme env down <id> [--json]
      Destroy the environment (reverse dependency order) and release its
      leases only after every destroy hook succeeded. A failing destroy
      leaves it `destroy_failed` with its leases held.
  aethyme env destroy <id> --retry | --abandon --reason <text> [--json]
      --retry runs destroy again. --abandon releases the leases without
      running destroy and records what may have leaked; use it only for
      resources that are already gone.
  aethyme env park <id> [--json]
      Stop every component and return the environment's capacity units to
      its pool, keeping its namespace, ports and provisioned data. Only an
      active environment parks; parking a parked one does nothing.
  aethyme env wake <id> [--wait <seconds>] [--json]
      Acquire capacity (waiting up to --wait, default 300), start every
      component and wait for health. Without capacity it stays parked with
      capacity_timeout; a failed start rolls back to parked. `env up` on a
      parked environment wakes it.
  aethyme env exec <id> -- <command> [args...]
      Run a command against the environment: it wakes a parked environment,
      sets the AETHYME_ENV_* variables (namespace, ports, URLs, receipts),
      and marks the environment busy for the command's whole life, so it is
      never parked under a running test. Exits with the command's status.
  aethyme env touch <id> [--json]
      Record activity now (an mtime update; no database lock).
  aethyme env pin <id> --for <seconds> --reason <text> [--json]
      Keep the environment awake (up to the host cap, at most 24 h). A host
      cap of 0 disables new pins. `env unpin <id>` ends an existing pin.
      Pins are recorded in the environment's history.
  aethyme env shared up|redeploy|destroy <name> [--force] [--json]
  aethyme env shared list [--json]
      Host-scoped [[shared]] environments, owned by a ref instead of a
      session: never removed by session or worktree cleanup. `up` creates or
      wakes one at its ref's commit (an export, not a worktree); `redeploy`
      moves it to the ref's current commit by hold-and-swap (park, swap,
      wake: same ports and URLs, requests held meanwhile), rolling back if
      the new revision fails; the supervisor does this when the ref moves,
      at most every min_redeploy_interval_seconds (--force ignores it).
      Profiles with uses_shared get AETHYME_SHARED_<NAME>_URL/_PORT_<C>.
  aethyme env calibrate [--ram-fraction <0..1>] [--json]
      Propose `units` per profile and `limit` per pool from measured
      footprints (the supervisor samples active environments every 60 s:
      process-group RSS or container stats), so a full pool's p95 stays
      within --ram-fraction (default 0.5) of RAM minus any container-engine
      VM reservation. Prints a diff of environments.toml; never writes it.
      Profiles with fewer than 10 samples are reported as insufficient.
  aethyme env cache list|prune [--all] [--json]
      Content-addressed build outputs (build + cache_inputs, output in
      build_output or serve_dir) shared across worktrees, and database
      templates (template + template_inputs, AETHYME_ENV_TEMPLATE_ID to
      provision). `prune` evicts unused entries, least recently used first,
      down to AETHYME_ENV_CACHE_MAX_BYTES (default 8 GiB), or all unused ones
      with --all; templates go only through their template_destroy.
  aethyme env routes [--json]
      The route table the built-in proxy serves on 127.0.0.1:4180
      (AETHYME_ENV_PROXY_PORT): <component>.<namespace>.<route_domain> and
      <namespace>.<route_domain> per routed component, with the upstream port
      while active. A request to a parked environment's host wakes it and is
      held until it is healthy. Reach a host with your browser
      (*.localhost) or with `curl -H 'Host: <host>' http://127.0.0.1:4180/`.
  aethyme env sweep [--json]
      Destroy environments whose worktree no longer exists (from a
      temporary export of their recorded commit) and retry failed destroys
      (at most 5 times). The supervisor runs this on its own.
  aethyme env supervise [--ensure] [--json]
      Run the host supervisor in the foreground (one per host user; a second
      exits at once). --ensure instead starts one in the background if none
      runs and returns when it is up. It exits by itself after
      AETHYME_ENV_SUPERVISOR_LINGER_SECONDS (default 300) with nothing to
      supervise.
";

pub(super) struct Failure {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) next_action: String,
    pub(super) exit: u8,
}

pub(super) struct Options {
    pub(super) json: bool,
    pub(super) repo: Option<PathBuf>,
    pub(super) profile: Option<String>,
    pub(super) positional: Vec<String>,
    pub(super) all: bool,
    pub(super) ensure: bool,
    pub(super) tail: usize,
    pub(super) limit: usize,
    pub(super) wait_seconds: u64,
    pub(super) reconfigure: bool,
    pub(super) retry: bool,
    pub(super) abandon: bool,
    pub(super) reason: Option<String>,
    pub(super) pin_seconds: Option<u64>,
    pub(super) range: Option<String>,
    pub(super) force: bool,
    pub(super) ram_fraction: f64,
}

pub fn run(args: &[String]) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return exit_status::USAGE;
    };
    // Help anywhere answers help and does nothing else, so the router can
    // pass any `env ... --help` line through unchanged.
    if command == "help"
        || args
            .iter()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "-h" || arg == "--help")
    {
        print!("{USAGE}");
        return exit_status::SUCCESS;
    }
    if command == "exec" {
        return super::operate::run_exec(rest);
    }
    let options = match parse_options(command, rest) {
        Ok(options) => options,
        Err(failure) => return report(false, failure),
    };
    let json = options.json;
    let result = match command.as_str() {
        "validate" => run_validate(&options),
        "plan" => run_plan(&options),
        "status" => super::inspect::run_status(&options),
        "list" => super::inspect::run_list(&options),
        "history" => super::inspect::run_history(&options),
        "logs" => super::inspect::run_logs(&options),
        "supervise" => super::inspect::run_supervise(&options),
        "up" => super::operate::run_up(&options),
        "down" => super::operate::run_down(&options),
        "destroy" => super::operate::run_destroy(&options),
        "sweep" => super::operate::run_sweep(&options),
        "park" => super::operate::run_park(&options),
        "wake" => super::operate::run_wake(&options),
        "touch" => super::operate::run_touch(&options),
        "pin" => super::operate::run_pin(&options),
        "unpin" => super::operate::run_unpin(&options),
        "routes" => super::operate::run_routes(&options),
        "infer" => super::operate::run_infer(&options),
        "shared" => super::operate::run_shared(&options),
        "calibrate" => super::operate::run_calibrate(&options),
        "cache" => super::operate::run_cache(&options),
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
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
        positional: Vec::new(),
        all: false,
        ensure: false,
        tail: 50,
        limit: 200,
        wait_seconds: 300,
        reconfigure: false,
        retry: false,
        abandon: false,
        reason: None,
        pin_seconds: None,
        range: None,
        force: false,
        ram_fraction: super::metrics::DEFAULT_RAM_FRACTION,
    };
    let number = |value: Option<&String>, flag: &str| -> Result<usize, Failure> {
        value
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| (1..=10_000).contains(n))
            .ok_or_else(|| usage(format!("{flag} needs a number from 1 to 10000")))
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
            "--wait" if matches!(command, "up" | "wake") => {
                options.wait_seconds = number(args.next(), "--wait")? as u64
            }
            "--reconfigure" if command == "up" => options.reconfigure = true,
            "--retry" if command == "destroy" => options.retry = true,
            "--abandon" if command == "destroy" => options.abandon = true,
            "--for" if command == "pin" => {
                options.pin_seconds = Some(
                    args.next()
                        .and_then(|v| v.parse::<u64>().ok())
                        .filter(|n| (60..=86_400).contains(n))
                        .ok_or_else(|| usage("--for needs seconds from 60 to 86400".into()))?,
                )
            }
            "--force" if command == "shared" => options.force = true,
            "--all" if command == "cache" => options.all = true,
            "--ram-fraction" if command == "calibrate" => {
                options.ram_fraction = args
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|f| *f > 0.0 && *f <= 1.0)
                    .ok_or_else(|| usage("--ram-fraction needs a number in (0, 1]".into()))?
            }
            "--range" if command == "infer" => {
                options.range = Some(
                    args.next()
                        .ok_or_else(|| usage("--range needs a revision range".into()))?
                        .clone(),
                )
            }
            "--reason" if matches!(command, "destroy" | "pin" | "up") => {
                options.reason = Some(
                    args.next()
                        .ok_or_else(|| usage("--reason needs text".into()))?
                        .clone(),
                )
            }
            "--profile" if matches!(command, "plan" | "up") => {
                options.profile = Some(
                    args.next()
                        .ok_or_else(|| usage("--profile needs a name".into()))?
                        .clone(),
                )
            }
            "--all" if command == "list" => options.all = true,
            "--ensure" if command == "supervise" => options.ensure = true,
            "--tail" if command == "logs" => options.tail = number(args.next(), "--tail")?,
            "--limit" if command == "history" => options.limit = number(args.next(), "--limit")?,
            positional
                if !positional.starts_with('-')
                    && matches!(
                        command,
                        "status"
                            | "history"
                            | "logs"
                            | "down"
                            | "destroy"
                            | "park"
                            | "wake"
                            | "touch"
                            | "pin"
                            | "unpin"
                            | "shared"
                            | "cache"
                    )
                    && options.positional.len()
                        < if matches!(command, "logs" | "shared") {
                            2
                        } else {
                            1
                        } =>
            {
                options.positional.push(positional.to_string())
            }
            other => {
                return Err(usage(format!(
                    "unexpected argument {other:?} for env {command}"
                )));
            }
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

pub(super) struct Workspace {
    pub(super) worktree_root: PathBuf,
    pub(super) repo: crate::GitRepo,
}

pub(super) fn workspace(options: &Options) -> Result<Workspace, Failure> {
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

pub(super) fn environment_failure(message: String) -> Failure {
    Failure {
        code: "io_error",
        message,
        next_action: "check file permissions and retry".into(),
        exit: exit_status::ENVIRONMENT,
    }
}

/// Everything `validate` and `plan` share: both files parsed and checked.
pub(super) struct Checked {
    pub(super) file: Option<EnvironmentsFile>,
    pub(super) host: HostEnvironmentsFile,
    pub(super) host_label: String,
    pub(super) digest: String,
    pub(super) diagnostics: Vec<Diagnostic>,
}

pub(super) fn check(workspace: &Workspace) -> Result<Checked, Failure> {
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

pub(super) type Output = (Value, String, u8);

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
            if checked.diagnostics.len() == 1 {
                ""
            } else {
                "s"
            }
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
    let compiled =
        compile(file, &checked.host, &profile, ENVIRONMENTS_FILE, &context).map_err(|error| {
            Failure {
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
            }
        })?;
    let plans = advisory_plans(&compiled)?;
    Ok(render_plan(&compiled, plans))
}

pub(super) fn compile_context(workspace_root: &Workspace, file_digest: &str) -> CompileContext {
    let repo = &workspace_root.repo;
    let fingerprint = crate::console::worktree_fingerprint(&workspace_root.worktree_root);
    let commit = repo.head_commit().ok();
    let committed = commit
        .as_deref()
        .and_then(|commit| repo.blob_at(commit, ENVIRONMENTS_FILE));
    let dirty = match committed {
        Some(text) => {
            format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
            ) != file_digest
        }
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
    let reservation = compiled.reservation_request(None, 0);
    let capacity = compiled.capacity_request(None, 0);
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
    let reservation = compiled.reservation_request(None, 0);
    let capacity = compiled.capacity_request(None, 0);
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
