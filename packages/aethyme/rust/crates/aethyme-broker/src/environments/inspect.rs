//! `env status | list | history | logs | supervise` (#1222).
//!
//! Inspection never creates host state: a host where no environment was ever
//! recorded reads as empty, and the supervisor is reported, not started.

use serde_json::{Value, json};

use super::cli::{
    Failure, HISTORY_SCHEMA, LIST_SCHEMA, LOGS_SCHEMA, Options, Output, STATUS_SCHEMA,
    SUPERVISE_SCHEMA, environment_failure, workspace,
};
use super::logs::{environments_directory, tail};
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
    status.describe()
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
        // Each listening component's URL, entry path included (#1292).
        let urls = environments_directory()
            .map(|root| super::lifecycle::component_urls(&root, &record))
            .unwrap_or_default();
        for (component, url) in &urls {
            text.push_str(&format!("    {component}: {url}\n"));
        }
        let value = json!({
            "schema": STATUS_SCHEMA,
            "environment": summary(&record),
            "urls": urls,
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
    // Advisory (#1216): an environment below what this worktree's changes
    // now need. Best effort: no valid file or no git means no advisory.
    let inference = super::load_repository_file(&workspace.worktree_root)
        .ok()
        .flatten()
        .and_then(|loaded| loaded.value.ok())
        .and_then(|file| {
            let (_, changed) = super::infer::changed_paths(&workspace.worktree_root).ok()?;
            let inference = super::infer::infer(&file, &changed).ok()?;
            Some((file, inference))
        });
    let mut below = Vec::new();
    if let Some((file, inference)) = &inference {
        for record in &records {
            if record.phase.retains_worktree()
                && super::infer::profile_rank(file, &record.profile)
                    .is_some_and(|rank| rank < inference.rank)
            {
                below.push(record.env_id.clone());
                text.push_str(&format!(
                    "advisory: {} ({}) is below the inferred minimum {}; `aethyme env up` replaces it\n",
                    record.env_id, record.profile, inference.minimum
                ));
            }
        }
    }
    text.push_str(&supervisor_line(&supervisor));
    let value = json!({
        "schema": STATUS_SCHEMA,
        "environments": records.iter().map(summary).collect::<Vec<_>>(),
        "inferred_minimum": inference.as_ref().map(|(_, i)| i.minimum.clone()),
        "below_inferred_minimum": below,
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
    let memory: Vec<Value> = records
        .iter()
        .map(|record| {
            // Current footprint only: an active environment's recent
            // measurement. Parked or stopped ones use no memory, and an old
            // or unmeasurable sample is not a footprint.
            let now = super::activity::now_ms();
            let stale = 3 * super::metrics::SAMPLE_INTERVAL.as_millis() as i64;
            store
                .as_ref()
                .filter(|_| record.phase == super::store::EnvironmentPhase::Active)
                .and_then(|store| store.latest_sample(&record.env_id).ok().flatten())
                .filter(|sample| sample.method != "none" && now - sample.sampled_at <= stale)
                .map_or(Value::Null, |sample| json!(sample))
        })
        .collect();
    let environments: Vec<Value> = records
        .iter()
        .zip(memory)
        .map(|(record, memory)| {
            let mut value = summary(record);
            value["memory"] = memory;
            value
        })
        .collect();
    let value = json!({
        "schema": LIST_SCHEMA,
        "scope": if options.all { "host" } else { "repository" },
        "environments": environments,
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
        // One phase when asked; otherwise the runtime log (always, even when
        // empty) and every hook phase that has written something (#1522).
        let phases: Vec<&str> = match &options.phase {
            Some(phase) => vec![phase.as_str()],
            None => std::iter::once(super::logs::RUN_PHASE)
                .chain(super::logs::HOOK_PHASES.iter().copied().filter(|phase| {
                    super::logs::phase_log_path(&root, &record.env_id, name, phase)
                        .is_some_and(|path| path.exists())
                }))
                .collect(),
        };
        for phase in phases {
            let path = super::logs::phase_log_path(&root, &record.env_id, name, phase).ok_or_else(
                || {
                    environment_failure(format!(
                        "invalid log path for {}/{name} ({phase})",
                        record.env_id
                    ))
                },
            )?;
            let lines = if options.last_run {
                super::logs::tail_last_run(&path, options.tail)
            } else {
                tail(&path, options.tail)
            }
            .map_err(|error| environment_failure(format!("{}: {error}", path.display())))?;
            text.push_str(&format!("== {name} {phase} ({})\n", path.display()));
            for line in &lines {
                text.push_str(line);
                text.push('\n');
            }
            logs.push(json!({
                "component": name,
                "phase": phase,
                "path": path,
                "lines": lines,
            }));
        }
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
    // Detached (started by `ensure`, stdout on the log): timestamp and
    // throttle everything this process prints (#1521). In a terminal the
    // output stays where the operator is looking.
    #[cfg(unix)]
    let _stamped = (!std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .then(|| super::supervisor_log::stamp_output(&root.join(supervisor::SUPERVISOR_LOG)))
        .transpose()
        .map_err(|error| environment_failure(format!("supervisor log: {error}")))?;
    supervisor::install_signal_handlers()
        .map_err(|error| environment_failure(format!("supervisor signals: {error}")))?;
    let config = SupervisorConfig::new(root);
    let lifecycle = super::operate::LifecycleWork {
        watch: StoreWatch { store_path },
        proxy_started: false,
        last_sample: None,
        shared_sync: None,
        last_budget_outcomes: None,
        budget_outcomes: None,
        last_prebuild: None,
        prebuild: None,
    };
    // The supervisor is the host's one background process: every service
    // that must outlive a command is hosted here.
    let mut services: Vec<Box<dyn supervisor::SupervisorWork>> = vec![Box::new(lifecycle)];
    #[cfg(unix)]
    if let Some(directory) = crate::worktree_watch::directory() {
        services.push(Box::new(crate::worktree_watch::WorktreeWatchService::new(
            directory,
        )));
    }
    let mut work = supervisor::Services(services);
    let reason = supervisor::run(&config, &mut work, &|| false)
        .map_err(|error| environment_failure(format!("supervisor: {error}")))?;
    if reason == supervisor::ExitReason::Replaced {
        // The record is withdrawn and the lock released, so this starts the
        // new build. A failure leaves no supervisor, which the next command
        // that needs one repairs; it is still said in the log.
        if let Some(executable) = &config.executable {
            match supervisor::ensure_running(&config.root, executable) {
                Ok(successor) => eprintln!("aethyme env supervisor: {}", successor.describe()),
                Err(error) => {
                    eprintln!("aethyme env supervisor: successor did not start: {error}")
                }
            }
        }
    }
    let value = json!({ "schema": SUPERVISE_SCHEMA, "exit_reason": reason });
    Ok((
        value,
        format!("supervisor exited: {reason:?}\n"),
        exit_status::SUCCESS,
    ))
}
