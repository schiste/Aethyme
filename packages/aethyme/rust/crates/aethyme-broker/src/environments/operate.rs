//! `env up | down | destroy | sweep` (#1211).

use std::time::Duration;

use serde_json::{Value, json};

use super::ENVIRONMENTS_FILE;
use super::cli::{
    DOWN_SCHEMA, Failure, Options, Output, SWEEP_SCHEMA, UP_SCHEMA, check, compile_context,
    environment_failure, workspace,
};
use super::compile::{compile, default_profile_name};
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
    let profile = options
        .profile
        .as_deref()
        .or_else(|| default_profile_name(file))
        .unwrap_or_default()
        .to_string();
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
}

impl super::supervisor::SupervisorWork for LifecycleWork {
    fn tick(&mut self) -> super::supervisor::TickOutcome {
        if let Ok((mut lifecycle, _)) = open("supervisor", false) {
            crate::warn_unrecorded("renew environment leases", lifecycle.renew_leases());
            crate::warn_unrecorded("sweep orphaned environments", lifecycle.sweep_orphans());
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
