//! `env up | down | destroy | sweep` (#1211), `park | wake` (#1212),
//! `exec | touch | pin | unpin` (#1213).

use std::time::Duration;

use serde_json::{Value, json};

use super::ENVIRONMENTS_FILE;
use super::cli::{
    DOWN_SCHEMA, Failure, Options, Output, PARK_SCHEMA, PIN_SCHEMA, SWEEP_SCHEMA, TOUCH_SCHEMA,
    UP_SCHEMA, WAKE_SCHEMA, check, compile_context, environment_failure, workspace,
};
use super::compile::compile;
use super::lifecycle::{Lifecycle, LifecycleError, RESERVATION_GRANT, UpOptions, read_grant};
use super::logs::environments_directory;
use super::store::{EnvironmentRecord, EnvironmentStore};
use super::supervisor;
use crate::exit_status;

fn lifecycle_failure(error: LifecycleError) -> Failure {
    let code = error.code();
    let next_action = match &error {
        LifecycleError::ReconfigureRequired { .. } => "rerun with --reconfigure".to_string(),
        LifecycleError::WrongPhase { next, .. } => next.clone(),
        LifecycleError::Resources(_) if code == "capacity_timeout" => {
            "raise --wait, park or destroy another environment in the pool, or raise the pool limit"
                .into()
        }
        _ => "run `aethyme env status <id>` and `aethyme env logs <id>` for the failing component"
            .into(),
    };
    Failure {
        code,
        message: error.to_string(),
        next_action,
        exit: match code {
            "not_found" | "wrong_phase" | "reconfigure_required" | "capacity_timeout" => {
                exit_status::REFUSED
            }
            _ => exit_status::FAILED,
        },
    }
}

/// Open the lifecycle on the default host paths. `holder_pid` is the
/// supervisor's when one could be started: it renews the leases, so they
/// are not reclaimed when this short-lived command exits.
fn open(actor: &str, ensure_supervisor: bool) -> Result<(Lifecycle, Option<String>), Failure> {
    let root = environments_directory()
        .ok_or_else(|| environment_failure("host state directory is unavailable".into()))?;
    let mut warning = None;
    let holder_pid = if ensure_supervisor {
        match std::env::current_exe().and_then(|exe| supervisor::ensure_running(&root, &exe)) {
            Ok(status) => status.pid.map(|pid| pid as u32),
            Err(error) => {
                warning = Some(format!(
                    "no supervisor ({error}); leases are not renewed in the background, so the reservation expires after an hour unless a later env command renews it"
                ));
                None
            }
        }
    } else {
        None
    };
    let store =
        EnvironmentStore::open_default().map_err(|error| environment_failure(error.to_string()))?;
    let coordinator = crate::resources::HostResourceCoordinator::open_default()
        .map_err(|error| environment_failure(error.to_string()))?;
    Ok((
        Lifecycle {
            store,
            coordinator,
            root,
            holder_pid,
            hook_timeout: super::exec::DEFAULT_HOOK_TIMEOUT,
            actor: actor.into(),
        },
        warning,
    ))
}

fn describe(lifecycle: &Lifecycle, record: &EnvironmentRecord) -> (Value, String) {
    let grant = read_grant(&lifecycle.root, &record.env_id, RESERVATION_GRANT);
    let mut ports = serde_json::Map::new();
    let mut variables = serde_json::Map::new();
    if let Some(grant) = &grant {
        for allocation in &grant.lease.allocations {
            if let Some(component) = allocation.key.strip_prefix("port_") {
                ports.insert(component.into(), json!(allocation.value));
                let suffix = super::validate::env_suffix(component);
                variables.insert(
                    format!("AETHYME_ENV_PORT_{suffix}"),
                    json!(allocation.value),
                );
                variables.insert(
                    format!("AETHYME_ENV_URL_{suffix}"),
                    json!(format!("http://127.0.0.1:{}", allocation.value)),
                );
            }
        }
    }
    variables.insert("AETHYME_ENV_ID".into(), json!(record.env_id));
    if let Some(namespace) = &record.namespace {
        variables.insert("AETHYME_ENV_NAMESPACE".into(), json!(namespace));
    }
    let mut text = format!(
        "environment {} ({}) is {}\n",
        record.env_id,
        record.profile,
        record.phase.as_str()
    );
    for (component, port) in &ports {
        text.push_str(&format!(
            "  {component}: http://127.0.0.1:{}\n",
            port.as_str().unwrap_or_default()
        ));
    }
    let value = json!({
        "env_id": record.env_id,
        "profile": record.profile,
        "phase": record.phase,
        "namespace": record.namespace,
        "parked_at": record.parked_at,
        "units_to_wake": record.snapshot.units,
        "pool": record.snapshot.pool.as_ref().map(|pool| json!({"name": pool.name, "limit": pool.limit})),
        "ports": ports,
        "variables": variables,
        "last_error": record.last_error,
    });
    (value, text)
}

pub(super) fn run_up(options: &Options) -> Result<Output, Failure> {
    let workspace = workspace(options)?;
    let checked = check(&workspace)?;
    let file = match (&checked.file, checked.diagnostics.is_empty()) {
        (Some(file), true) => file,
        _ => {
            return Err(Failure {
                code: "invalid_environments_file",
                message: format!(
                    "{ENVIRONMENTS_FILE} has {} validation problem(s)",
                    checked.diagnostics.len()
                ),
                next_action: "run `aethyme env validate` and fix every problem".into(),
                exit: exit_status::VERIFICATION_FAILED,
            });
        }
    };
    let (base, changed) =
        super::infer::changed_paths(&workspace.worktree_root).map_err(environment_failure)?;
    let inference = super::infer::infer(file, &changed).map_err(environment_failure)?;
    let profile = choose_profile(file, &inference, options)?;
    let context = compile_context(&workspace, &checked.digest);
    let snapshot =
        compile(file, &checked.host, &profile, ENVIRONMENTS_FILE, &context).map_err(|error| {
            Failure {
                code: "unknown_profile",
                message: error.to_string(),
                next_action: "pass --profile with a declared profile".into(),
                exit: exit_status::USAGE,
            }
        })?;
    let (mut lifecycle, warning) = open("env up", true)?;
    let replaced =
        replace_below_minimum(&mut lifecycle, &workspace.worktree_root, file, &inference)?;
    let main_root = workspace.repo.main_root().ok();
    let record = lifecycle
        .up(
            &snapshot,
            &workspace.worktree_root,
            main_root.as_deref(),
            None,
            &UpOptions {
                wait: Duration::from_secs(options.wait_seconds),
                reconfigure: options.reconfigure,
            },
        )
        .map_err(lifecycle_failure)?;
    let (mut value, mut text) = describe(&lifecycle, &record);
    if let Some(warning) = &warning {
        value["warning"] = json!(warning);
        text.push_str(&format!("warning: {warning}\n"));
    }
    if let Some(reason) = options.reason.as_deref()
        && super::infer::profile_rank(file, &record.profile).is_some_and(|r| r > inference.rank)
    {
        lifecycle
            .store
            .note(
                &record.env_id,
                "profile above inferred minimum",
                "env up",
                Some(&json!({
                    "profile": record.profile,
                    "inferred_minimum": inference.minimum,
                    "reason": reason,
                })),
            )
            .map_err(|error| environment_failure(error.to_string()))?;
    }
    value["inference"] = json!({
        "base": base,
        "minimum": inference.minimum,
        "fired": inference.fired,
    });
    if !replaced.is_empty() {
        value["replaced"] = json!(replaced);
        text.push_str(&format!(
            "replaced below-minimum environment(s): {}\n",
            replaced.join(", ")
        ));
    }
    value["schema"] = json!(UP_SCHEMA);
    Ok((value, text, exit_status::SUCCESS))
}

fn required_id(options: &Options) -> Result<String, Failure> {
    options.positional.first().cloned().ok_or_else(|| Failure {
        code: "usage",
        message: "an environment id is required".into(),
        next_action: "pass an id from `aethyme env list`".into(),
        exit: exit_status::USAGE,
    })
}

pub(super) fn run_down(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (mut lifecycle, _) = open("env down", false)?;
    let record = lifecycle.down(&id, None).map_err(lifecycle_failure)?;
    let (mut value, text) = describe(&lifecycle, &record);
    value["schema"] = json!(DOWN_SCHEMA);
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_destroy(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let abandon = match (options.retry, options.abandon, options.reason.as_deref()) {
        (true, false, None) => None,
        (false, true, Some(reason)) if !reason.trim().is_empty() => Some(reason.to_string()),
        _ => {
            return Err(Failure {
                code: "usage",
                message: "env destroy needs exactly one of --retry, or --abandon --reason <text>".into(),
                next_action: "pass --retry to run destroy again, or --abandon --reason \"why the resources are already gone\"".into(),
                exit: exit_status::USAGE,
            });
        }
    };
    let (mut lifecycle, _) = open("env destroy", false)?;
    let record = lifecycle
        .down(&id, abandon.as_deref())
        .map_err(lifecycle_failure)?;
    let (mut value, text) = describe(&lifecycle, &record);
    value["schema"] = json!(DOWN_SCHEMA);
    value["abandoned"] = json!(abandon.is_some());
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_park(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (mut lifecycle, _) = open("env park", false)?;
    let record = lifecycle.park(&id, "park").map_err(lifecycle_failure)?;
    let (mut value, text) = describe(&lifecycle, &record);
    value["schema"] = json!(PARK_SCHEMA);
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_wake(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (mut lifecycle, warning) = open("env wake", true)?;
    let record = lifecycle
        .wake(&id, Duration::from_secs(options.wait_seconds))
        .map_err(lifecycle_failure)?;
    let (mut value, mut text) = describe(&lifecycle, &record);
    if let Some(warning) = &warning {
        value["warning"] = json!(warning);
        text.push_str(&format!("warning: {warning}\n"));
    }
    value["schema"] = json!(WAKE_SCHEMA);
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_touch(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (lifecycle, _) = open("env touch", false)?;
    let record = lifecycle
        .store
        .resolve(&id)
        .map_err(|error| lifecycle_failure(error.into()))?;
    super::activity::touch(&lifecycle.root, &record.env_id)
        .map_err(|error| environment_failure(format!("touch {}: {error}", record.env_id)))?;
    let at = super::activity::now_ms();
    Ok((
        json!({ "schema": TOUCH_SCHEMA, "env_id": record.env_id, "at": at }),
        format!("touched {}\n", record.env_id),
        exit_status::SUCCESS,
    ))
}

pub(super) fn run_pin(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (Some(seconds), Some(reason)) = (options.pin_seconds, options.reason.as_deref()) else {
        return Err(Failure {
            code: "usage",
            message: "env pin needs --for <seconds> and --reason <text>".into(),
            next_action: "e.g. aethyme env pin <id> --for 3600 --reason \"demo\"".into(),
            exit: exit_status::USAGE,
        });
    };
    let (mut lifecycle, _) = open("env pin", false)?;
    let record = lifecycle
        .pin(&id, Duration::from_secs(seconds), reason)
        .map_err(lifecycle_failure)?;
    Ok((
        json!({
            "schema": PIN_SCHEMA,
            "env_id": record.env_id,
            "pinned_until": record.pinned_until,
            "reason": record.pin_reason,
        }),
        format!("{} pinned until {:?}\n", record.env_id, record.pinned_until),
        exit_status::SUCCESS,
    ))
}

pub(super) fn run_unpin(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let (mut lifecycle, _) = open("env unpin", false)?;
    let record = lifecycle.unpin(&id).map_err(lifecycle_failure)?;
    Ok((
        json!({ "schema": PIN_SCHEMA, "env_id": record.env_id, "pinned_until": null }),
        format!("{} unpinned\n", record.env_id),
        exit_status::SUCCESS,
    ))
}

fn refuse(failure: &Failure) -> u8 {
    eprintln!(
        "{}",
        json!({
            "schema": super::cli::ERROR_SCHEMA,
            "code": failure.code,
            "message": failure.message,
            "next_action": failure.next_action,
        })
    );
    failure.exit
}

/// `env exec <id> -- <command...>`: run an agent's command against the
/// environment. Its stdio is passed through and the exit status is the
/// command's. A refusal is one `aethyme.env-error/v1` line on stderr, so it
/// never mixes with the command's own stdout.
pub(super) fn run_exec(rest: &[String]) -> u8 {
    let usage = Failure {
        code: "usage",
        message: "env exec needs exactly one id, then `--`, then a command".into(),
        next_action: "e.g. aethyme env exec <id> -- cargo test".into(),
        exit: exit_status::USAGE,
    };
    let Some(split) = rest.iter().position(|arg| arg == "--") else {
        return refuse(&usage);
    };
    let (head, command) = (&rest[..split], &rest[split + 1..]);
    let (Some(id), Some((program, arguments))) = (head.first(), command.split_first()) else {
        return refuse(&usage);
    };
    if head.len() != 1 {
        return refuse(&usage);
    }
    let (mut lifecycle, _) = match open("env exec", true) {
        Ok(opened) => opened,
        Err(failure) => return refuse(&failure),
    };
    let record = match lifecycle.store.resolve(id) {
        Ok(record) if record.phase == super::store::EnvironmentPhase::Parked => {
            match lifecycle.wake(&record.env_id, Duration::from_secs(300)) {
                Ok(record) => record,
                Err(error) => return refuse(&lifecycle_failure(error)),
            }
        }
        Ok(record) if record.phase == super::store::EnvironmentPhase::Active => record,
        Ok(record) => {
            return refuse(&Failure {
                code: "wrong_phase",
                message: format!("environment {} is {}", record.env_id, record.phase.as_str()),
                next_action: "run `aethyme env up` first".into(),
                exit: exit_status::REFUSED,
            });
        }
        Err(error) => return refuse(&lifecycle_failure(error.into())),
    };
    let (described, _) = describe(&lifecycle, &record);
    let mut child = std::process::Command::new(program);
    child.args(arguments);
    if let Some(variables) = described["variables"].as_object() {
        for (key, value) in variables {
            if let Some(value) = value.as_str() {
                child.env(key, value);
            }
        }
    }
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(error) => {
            return refuse(&Failure {
                code: "spawn_failed",
                message: format!("{program}: {error}"),
                next_action: "check the command and PATH".into(),
                exit: exit_status::ENVIRONMENT,
            });
        }
    };
    // Busy for the command's whole life: never parked under a running test.
    let marker =
        super::activity::BusyMarker::register(&lifecycle.root, &record.env_id, child.id() as i32);
    if let Err(error) = &marker {
        eprintln!(
            "warning: aethyme could not mark {} busy: {error}",
            record.env_id
        );
    }
    crate::warn_unrecorded(
        "record activity",
        super::activity::touch(&lifecycle.root, &record.env_id),
    );
    let status = child.wait();
    crate::warn_unrecorded(
        "record activity",
        super::activity::touch(&lifecycle.root, &record.env_id),
    );
    drop(marker);
    match status {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(exit_status::FAILED),
        Err(error) => refuse(&Failure {
            code: "wait_failed",
            message: error.to_string(),
            next_action: "rerun the command".into(),
            exit: exit_status::ENVIRONMENT,
        }),
    }
}

pub(super) fn run_routes(_: &Options) -> Result<Output, Failure> {
    let root = environments_directory()
        .ok_or_else(|| environment_failure("host state directory is unavailable".into()))?;
    let routes = match EnvironmentStore::open_default_read_only()
        .map_err(|error| environment_failure(error.to_string()))?
    {
        Some(store) => super::routes::route_table(&store, &root)
            .map_err(|error| environment_failure(error.to_string()))?,
        None => Vec::new(),
    };
    let port = super::proxy::proxy_port();
    let mut text = format!("proxy: http://127.0.0.1:{port} (Host header selects the route)\n");
    if routes.is_empty() {
        text.push_str("no routes\n");
    }
    for route in &routes {
        text.push_str(&format!(
            "  {:<48} {} {} {}\n",
            route.host,
            route.env_id,
            route.state,
            route
                .upstream
                .map(|p| format!("-> 127.0.0.1:{p}"))
                .or_else(|| route
                    .static_dir
                    .as_ref()
                    .map(|d| format!("static {}", d.display())))
                .unwrap_or_else(|| "(wakes on request)".into())
        ));
    }
    Ok((
        json!({
            "schema": super::cli::ROUTES_SCHEMA,
            "proxy": format!("http://127.0.0.1:{port}"),
            "token_file": root.join(super::proxy::TOKEN_FILE),
            "etag": super::routes::etag(&routes),
            "routes": routes,
        }),
        text,
        exit_status::SUCCESS,
    ))
}

/// The profile `env up` brings up: the inferred minimum unless `--profile`
/// asks for one. Lower than the minimum is refused with the firing rules;
/// higher needs `--reason`.
fn choose_profile(
    file: &super::spec::EnvironmentsFile,
    inference: &super::infer::Inference,
    options: &Options,
) -> Result<String, Failure> {
    let Some(requested) = options.profile.as_deref() else {
        return Ok(inference.minimum.clone());
    };
    let rank = super::infer::profile_rank(file, requested).ok_or_else(|| Failure {
        code: "unknown_profile",
        message: format!("profile {requested:?} is not declared"),
        next_action: "pass --profile with a declared profile".into(),
        exit: exit_status::USAGE,
    })?;
    if rank < inference.rank {
        let rules: Vec<String> = inference
            .fired
            .iter()
            .map(|f| format!("{} -> {} ({})", f.rule, f.min_profile, f.paths.join(", ")))
            .collect();
        return Err(Failure {
            code: "below_inferred_minimum",
            message: format!(
                "profile {requested} is below the inferred minimum {}; rules fired: {}",
                inference.minimum,
                rules.join("; ")
            ),
            next_action: format!(
                "use --profile {} (or higher); if a rule is wrong, change [[rules]] in environments.toml",
                inference.minimum
            ),
            exit: exit_status::REFUSED,
        });
    }
    if rank > inference.rank
        && options
            .reason
            .as_deref()
            .is_none_or(|r| r.trim().is_empty())
    {
        return Err(Failure {
            code: "reason_required",
            message: format!(
                "profile {requested} is above the inferred minimum {}; say why",
                inference.minimum
            ),
            next_action: format!(
                "rerun with --reason \"...\", or use --profile {}",
                inference.minimum
            ),
            exit: exit_status::USAGE,
        });
    }
    Ok(requested.to_string())
}

/// Destroy this worktree's live environments whose profile ranks below the
/// inferred minimum, so a raised minimum replaces them instead of leaving a
/// too-cheap environment running beside the new one.
fn replace_below_minimum(
    lifecycle: &mut Lifecycle,
    worktree: &std::path::Path,
    file: &super::spec::EnvironmentsFile,
    inference: &super::infer::Inference,
) -> Result<Vec<String>, Failure> {
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    let below: Vec<String> = lifecycle
        .store
        .list(None)
        .map_err(|error| environment_failure(error.to_string()))?
        .into_iter()
        .filter(|r| r.worktree_fingerprint == fingerprint && r.phase.retains_worktree())
        .filter(|r| {
            super::infer::profile_rank(file, &r.profile).is_some_and(|rank| rank < inference.rank)
        })
        .map(|r| r.env_id)
        .collect();
    for env_id in &below {
        lifecycle.down(env_id, None).map_err(lifecycle_failure)?;
    }
    Ok(below)
}

pub(super) fn run_infer(options: &Options) -> Result<Output, Failure> {
    let workspace = workspace(options)?;
    let checked = check(&workspace)?;
    let file = match (&checked.file, checked.diagnostics.is_empty()) {
        (Some(file), true) => file,
        _ => {
            return Err(Failure {
                code: "invalid_environments_file",
                message: format!("{ENVIRONMENTS_FILE} has validation problems"),
                next_action: "run `aethyme env validate` and fix every problem".into(),
                exit: exit_status::VERIFICATION_FAILED,
            });
        }
    };
    if let Some(range) = &options.range {
        let report = super::infer::infer_range(&workspace.worktree_root, file, range)
            .map_err(environment_failure)?;
        let mut text = format!("{} commit(s) in {range}\n", report.commits);
        for (profile, count) in &report.profiles {
            text.push_str(&format!(
                "  {profile:<16} {count:>6}{}\n",
                report
                    .top_rule
                    .get(profile)
                    .map(|rule| format!("  (most often: {rule})"))
                    .unwrap_or_default()
            ));
        }
        return Ok((
            json!({ "schema": super::cli::INFER_SCHEMA, "range": range, "report": report }),
            text,
            exit_status::SUCCESS,
        ));
    }
    let (base, changed) =
        super::infer::changed_paths(&workspace.worktree_root).map_err(environment_failure)?;
    let inference = super::infer::infer(file, &changed).map_err(environment_failure)?;
    let mut text = format!(
        "minimum profile: {} ({} changed path(s))\n",
        inference.minimum, inference.changed
    );
    for fired in &inference.fired {
        text.push_str(&format!(
            "  rule {} -> {}: {}\n",
            fired.rule,
            fired.min_profile,
            fired.paths.join(", ")
        ));
    }
    if !inference.unmatched.is_empty() {
        text.push_str(&format!(
            "  unmatched: {}\n",
            inference.unmatched.join(", ")
        ));
    }
    Ok((
        json!({ "schema": super::cli::INFER_SCHEMA, "base": base, "inference": inference }),
        text,
        exit_status::SUCCESS,
    ))
}

pub(super) fn run_sweep(_: &Options) -> Result<Output, Failure> {
    let (mut lifecycle, _) = open("env sweep", false)?;
    lifecycle.renew_leases().map_err(lifecycle_failure)?;
    let acted = lifecycle.sweep_orphans().map_err(lifecycle_failure)?;
    let text = if acted.is_empty() {
        "nothing to sweep\n".to_string()
    } else {
        format!("swept: {}\n", acted.join(", "))
    };
    Ok((
        json!({ "schema": SWEEP_SCHEMA, "acted_on": acted }),
        text,
        exit_status::SUCCESS,
    ))
}

/// Supervisor work for the lifecycle: renew leases, sweep orphans, and stay
/// up while anything needs supervision.
pub struct LifecycleWork {
    pub watch: super::supervisor::StoreWatch,
    /// The proxy starts on the first tick, i.e. only in the supervisor that
    /// won the singleton lock.
    pub proxy_started: bool,
}

/// Start the wake-on-request proxy on loopback in this (supervisor)
/// process. A port already in use is logged, not fatal: everything except
/// wake on request keeps working.
fn start_proxy() {
    let (Some(root), Ok(store_path), Ok(host_db)) = (
        environments_directory(),
        super::store::default_store_path(),
        crate::resources::default_host_resource_db_path(),
    ) else {
        eprintln!("warning: aethyme env proxy not started: host state unavailable");
        return;
    };
    let token = match super::proxy::ensure_token(&root) {
        Ok(token) => token,
        Err(error) => {
            eprintln!("warning: aethyme env proxy not started: token: {error}");
            return;
        }
    };
    let port = super::proxy::proxy_port();
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("warning: aethyme env proxy not started on 127.0.0.1:{port}: {error}");
            return;
        }
    };
    let config = std::sync::Arc::new(super::proxy::ProxyConfig {
        root,
        store_path,
        host_db,
        token,
        holder_pid: Some(std::process::id()),
    });
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::spawn(move || {
        crate::warn_unrecorded(
            "serve the environment proxy",
            super::proxy::serve(listener, config, stop),
        );
    });
}

impl super::supervisor::SupervisorWork for LifecycleWork {
    fn tick(&mut self) -> super::supervisor::TickOutcome {
        if !self.proxy_started {
            self.proxy_started = true;
            start_proxy();
        }
        if let Ok((mut lifecycle, _)) = open("supervisor", false) {
            crate::warn_unrecorded("renew environment leases", lifecycle.renew_leases());
            crate::warn_unrecorded("sweep orphaned environments", lifecycle.sweep_orphans());
            crate::warn_unrecorded(
                "park idle environments",
                lifecycle.park_idle(super::activity::now_ms()),
            );
            // Live environments need renewal even when nothing is moving.
            let live = lifecycle
                .store
                .list(None)
                .map(|records| records.iter().any(|r| r.phase.retains_worktree()))
                .unwrap_or(true);
            if live {
                return super::supervisor::TickOutcome {
                    needs_supervisor: true,
                };
            }
        }
        self.watch.tick()
    }
}
