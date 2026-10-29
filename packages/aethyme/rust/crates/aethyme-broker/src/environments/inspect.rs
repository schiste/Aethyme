//! `env status | list | history | logs | supervise` (#1222).
//!
//! Inspection never creates host state: a host where no environment was ever
//! recorded reads as empty, and the supervisor is reported, not started.

use serde_json::{Value, json};

use super::cli::{
    Failure, HISTORY_SCHEMA, LIST_SCHEMA, LOGS_SCHEMA, Options, Output, STATUS_SCHEMA,
    SUPERVISE_SCHEMA, environment_failure, workspace,
};
use super::logs::{environments_directory, log_path, tail};
use super::store::{EnvironmentRecord, EnvironmentStore, StoreError, default_store_path};
use super::supervisor::{self, StoreWatch, SupervisorConfig, SupervisorStatus};
use crate::exit_status;

fn store_failure(error: StoreError) -> Failure {
    match error {
        StoreError::NotFound(id) => Failure {
            code: "not_found",
            message: format!("no environment {id}"),
            next_action: "run `aethyme env list --all` to see recorded environments".into(),
            exit: exit_status::REFUSED,
        },
        StoreError::Ambiguous(prefix) => Failure {
            code: "ambiguous_id",
            message: format!("{prefix:?} matches more than one environment"),
            next_action: "pass more characters of the id".into(),
            exit: exit_status::USAGE,
        },
        StoreError::SchemaTooNew { .. } => Failure {
            code: "schema_too_new",
            message: error.to_string(),
            next_action: "upgrade aethyme (`aethyme self-update`)".into(),
            exit: exit_status::ENVIRONMENT,
        },
        other => environment_failure(other.to_string()),
    }
}

fn root() -> Result<std::path::PathBuf, Failure> {
    environments_directory().ok_or_else(|| Failure {
        code: "host_state_unavailable",
        message: "host state directory is unavailable".into(),
        next_action: "set AETHYME_HOST_STATE_DIR or HOME".into(),
        exit: exit_status::ENVIRONMENT,
    })
}

fn read_store() -> Result<Option<EnvironmentStore>, Failure> {
    EnvironmentStore::open_default_read_only().map_err(store_failure)
}

fn supervisor_status() -> Result<SupervisorStatus, Failure> {
    Ok(supervisor::status(&root()?))
}

fn summary(record: &EnvironmentRecord) -> Value {
    json!({
        "env_id": record.env_id,
        "profile": record.profile,
        "phase": record.phase,
        "phase_generation": record.phase_generation,
        "owner": record.owner,
        "namespace": record.namespace,
        "session_id": record.session_id,
        "snapshot_digest": record.profile_digest,
        "updated_at": record.updated_at,
        "last_activity_at": record.last_activity_at,
        "parked_at": record.parked_at,
        "pinned_until": record.pinned_until,
        "last_error": record.last_error,
    })
}

fn summary_line(record: &EnvironmentRecord) -> String {
    format!(
        "  {}  {:<15} profile {}{}\n",
        record.env_id,
        record.phase.as_str(),
        record.profile,
        record
            .namespace
            .as_ref()
            .map(|ns| format!(", namespace {ns}"))
            .unwrap_or_default()
    )
}

fn supervisor_line(status: &SupervisorStatus) -> String {
    match status.pid {
        Some(pid) => format!("supervisor: {} (pid {pid})\n", status.state),
        None => format!(
            "supervisor: {} (idle parking and wake on request are not running)\n",
            status.state
        ),
    }
}

pub(super) fn run_status(options: &Options) -> Result<Output, Failure> {
    let supervisor = supervisor_status()?;
    let store = read_store()?;
    if let Some(id) = options.positional.first() {
        let Some(store) = store else {
            return Err(store_failure(StoreError::NotFound(id.clone())));
        };
        let record = store.resolve(id).map_err(store_failure)?;
        let components = store.components(&record.env_id).map_err(store_failure)?;
        let transitions = store
            .transitions(&record.env_id, 10)
            .map_err(store_failure)?;
        let mut text = summary_line(&record);
        for component in &components {
            text.push_str(&format!(
                "    {:<12} {:<10} {} (attempt {})\n",
                component.component,
                component.runtime,
                component.phase.as_str(),
                component.attempt
            ));
        }
        if let Some(error) = &record.last_error {
            text.push_str(&format!("  last error: {error}\n"));
        }
        text.push_str(&supervisor_line(&supervisor));
        let value = json!({
            "schema": STATUS_SCHEMA,
            "environment": summary(&record),
            "components": components,
            "recent_transitions": transitions,
            "supervisor": supervisor,
        });
        return Ok((value, text, exit_status::SUCCESS));
    }
    let workspace = workspace(options)?;
    let fingerprint = crate::console::worktree_fingerprint(&workspace.worktree_root);
    let records: Vec<EnvironmentRecord> = match &store {
        None => Vec::new(),
        Some(store) => store
            .list(None)
            .map_err(store_failure)?
            .into_iter()
            .filter(|record| record.worktree_fingerprint == fingerprint)
            .collect(),
    };
    let mut text = if records.is_empty() {
        "no environments for this worktree\n".to_string()
    } else {
        format!("{} environment(s) for this worktree:\n", records.len())
    };
    for record in &records {
        text.push_str(&summary_line(record));
    }
    text.push_str(&supervisor_line(&supervisor));
    let value = json!({
        "schema": STATUS_SCHEMA,
        "environments": records.iter().map(summary).collect::<Vec<_>>(),
        "supervisor": supervisor,
    });
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_list(options: &Options) -> Result<Output, Failure> {
    let store = read_store()?;
    let repository = if options.all {
        None
    } else {
        let workspace = workspace(options)?;
        Some(crate::gates::git_origin_fingerprint(&workspace.repo))
    };
    let records = match &store {
        None => Vec::new(),
        Some(store) => store.list(repository.as_deref()).map_err(store_failure)?,
    };
    let mut text = if records.is_empty() {
        "no environments recorded\n".to_string()
    } else {
        String::new()
    };
    for record in &records {
        text.push_str(&summary_line(record));
    }
    let value = json!({
        "schema": LIST_SCHEMA,
        "scope": if options.all { "host" } else { "repository" },
        "environments": records.iter().map(summary).collect::<Vec<_>>(),
    });
    Ok((value, text, exit_status::SUCCESS))
}

fn required_id(options: &Options) -> Result<&str, Failure> {
    options
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| Failure {
            code: "usage",
            message: "an environment id is required".into(),
            next_action: "pass an id from `aethyme env list`".into(),
            exit: exit_status::USAGE,
        })
}

pub(super) fn run_history(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let store = read_store()?.ok_or_else(|| store_failure(StoreError::NotFound(id.into())))?;
    let record = store.resolve(id).map_err(store_failure)?;
    let transitions = store
        .transitions(&record.env_id, options.limit)
        .map_err(store_failure)?;
    let mut text = String::new();
    for t in &transitions {
        text.push_str(&format!(
            "  #{} {} {} -> {} ({}, by {})\n",
            t.seq,
            t.at,
            t.from.map(|p| p.as_str()).unwrap_or("-"),
            t.to.as_str(),
            t.reason,
            t.actor
        ));
    }
    let value = json!({
        "schema": HISTORY_SCHEMA,
        "env_id": record.env_id,
        "transitions": transitions,
    });
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_logs(options: &Options) -> Result<Output, Failure> {
    let id = required_id(options)?;
    let store = read_store()?.ok_or_else(|| store_failure(StoreError::NotFound(id.into())))?;
    let record = store.resolve(id).map_err(store_failure)?;
    let wanted = options.positional.get(1);
    let names: Vec<&str> = record
        .snapshot
        .components
        .iter()
        .map(|c| c.name.as_str())
        .filter(|name| wanted.is_none_or(|w| w == name))
        .collect();
    if let Some(wanted) = wanted
        && names.is_empty()
    {
        return Err(Failure {
            code: "unknown_component",
            message: format!("{} has no component {wanted:?}", record.env_id),
            next_action: format!(
                "pass one of: {}",
                record
                    .snapshot
                    .components
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            exit: exit_status::USAGE,
        });
    }
    let root = root()?;
    let mut text = String::new();
    let mut logs = Vec::new();
    for name in names {
        let path = log_path(&root, &record.env_id, name).ok_or_else(|| {
            environment_failure(format!("invalid log path for {}/{name}", record.env_id))
        })?;
        let lines = tail(&path, options.tail)
            .map_err(|error| environment_failure(format!("{}: {error}", path.display())))?;
        text.push_str(&format!("== {name} ({})\n", path.display()));
        for line in &lines {
            text.push_str(line);
            text.push('\n');
        }
        logs.push(json!({
            "component": name,
            "path": path,
            "lines": lines,
        }));
    }
    let value = json!({
        "schema": LOGS_SCHEMA,
        "env_id": record.env_id,
        "logs": logs,
    });
    Ok((value, text, exit_status::SUCCESS))
}

pub(super) fn run_supervise(options: &Options) -> Result<Output, Failure> {
    let root = root()?;
    if options.ensure {
        let executable = std::env::current_exe()
            .map_err(|error| environment_failure(format!("current executable: {error}")))?;
        let status = supervisor::ensure_running(&root, &executable)
            .map_err(|error| environment_failure(format!("start supervisor: {error}")))?;
        let text = supervisor_line(&status);
        let value = json!({ "schema": SUPERVISE_SCHEMA, "supervisor": status });
        return Ok((value, text, exit_status::SUCCESS));
    }
    let store_path = default_store_path().map_err(store_failure)?;
    let config = SupervisorConfig::new(root);
    let mut work = super::operate::LifecycleWork {
        watch: StoreWatch { store_path },
    };
    let reason = supervisor::run(&config, &mut work, &|| false)
        .map_err(|error| environment_failure(format!("supervisor: {error}")))?;
    let value = json!({ "schema": SUPERVISE_SCHEMA, "exit_reason": reason });
    Ok((
        value,
        format!("supervisor exited: {reason:?}\n"),
        exit_status::SUCCESS,
    ))
}
