//! `broker resources` and `console`: host resource leases and the shared console.

use super::*;

pub(super) const HOST_RESOURCE_INPUT_MAX_BYTES: u64 = 1024 * 1024;

pub(super) fn read_resource_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
) -> Result<T, UsageError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        UsageError::Message(format!("cannot inspect {}: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(UsageError::Message(format!(
            "resource input must be a regular, non-symlink file: {}",
            path.display()
        )));
    }
    if metadata.len() > HOST_RESOURCE_INPUT_MAX_BYTES {
        return Err(UsageError::Message(format!(
            "resource input exceeds {HOST_RESOURCE_INPUT_MAX_BYTES} bytes"
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| UsageError::Message(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        UsageError::Message(format!("invalid JSON in {}: {error}", path.display()))
    })
}

pub(super) fn parse_resource_duration(value: &str) -> Result<std::time::Duration, UsageError> {
    let (digits, multiplier) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1_u64)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else {
        return Err(UsageError::Message(
            "duration must use ms, s, m, or h (for example 30m)".into(),
        ));
    };
    let amount = digits.parse::<u64>().map_err(|_| {
        UsageError::Message("duration must be a non-negative integer plus ms, s, m, or h".into())
    })?;
    let millis = amount
        .checked_mul(multiplier)
        .ok_or_else(|| UsageError::Message("duration is too large".into()))?;
    Ok(std::time::Duration::from_millis(millis))
}

pub(super) fn write_private_grant(
    path: &std::path::Path,
    grant: &crate::HostResourceGrant,
) -> Result<(), UsageError> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    if path.exists() {
        return Err(UsageError::Message(format!(
            "refusing to overwrite existing grant file {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(UsageError::Message(format!(
            "grant parent directory does not exist: {}",
            parent.display()
        )));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        UsageError::Message(format!(
            "cannot create private grant beside {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| UsageError::Message(format!("cannot protect grant file: {error}")))?;
    serde_json::to_writer_pretty(&mut temporary, grant)?;
    temporary
        .write_all(b"\n")
        .map_err(|error| UsageError::Message(format!("cannot finish private grant: {error}")))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| UsageError::Message(format!("cannot sync private grant: {error}")))?;
    temporary.persist_noclobber(path).map_err(|error| {
        UsageError::Message(format!(
            "cannot publish private grant {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

#[derive(serde::Serialize)]
pub(super) struct ResourceAcquireFailure<'a> {
    code: &'a str,
    request_id: &'a str,
    retryable: bool,
    waited_ms: u128,
    conflicts: &'a [crate::HostResourceConflict],
}

pub(super) fn render_host_lease(lease: &crate::HostResourceLease) {
    out!(
        "{} generation {} — {} until {}",
        lease.lease_id,
        lease.generation,
        lease.state.as_str(),
        lease.expires_at
    );
    for allocation in &lease.allocations {
        out!(
            "  {:<20} {:<14} {}",
            allocation.key,
            allocation.kind,
            allocation.value
        );
    }
}

pub(super) fn render_host_resource_explanation(explanation: &crate::HostResourceExplanation) {
    out!(
        "Request {} — {}",
        explanation.request_id,
        if explanation.available {
            "available"
        } else {
            "blocked"
        }
    );
    out!(
        "  waitable: {} ({})",
        explanation.wait.waitable,
        explanation.wait.reason
    );
    out!("  action: {}", explanation.wait.action);
    for blocker in &explanation.blockers {
        let conflict = &blocker.conflict;
        out!(
            "  blocker {} [{}] — {}",
            conflict.resource_key,
            conflict.kind,
            conflict.reason
        );
        if let Some(bindable) = blocker.os_bindable {
            out!("    OS port available in requested range: {bindable}");
        }
        if blocker.leases.is_empty() {
            out!("    broker leases: none");
        } else {
            for holder in &blocker.holders {
                out!(
                    "    lease {} generation {} run {} pid {} ({})",
                    holder.lease_id,
                    holder.generation,
                    holder.run_id,
                    holder
                        .holder_pid
                        .map_or_else(|| "-".into(), |pid| pid.to_string()),
                    holder
                        .process_alive
                        .map_or("unknown", |alive| { if alive { "alive" } else { "gone" } })
                );
            }
        }
        out!("    recovery: {}", blocker.recovery);
    }
}

/// `console` resolves the repository from the current directory rather than
/// from a session id: an operator starting a dev server is not necessarily in
/// a broker session, and requiring one would put coordination behind exactly
/// the step people skip.
pub(super) fn console_context() -> Result<
    (
        crate::ConsoleConfig,
        String,
        crate::GitRepo,
        PathBuf,
        PathBuf,
    ),
    UsageError,
> {
    let cwd = std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
    let repo = crate::GitRepo::discover(&cwd).map_err(|error| {
        UsageError::Message(format!("console requires a git checkout: {error}"))
    })?;
    let worktree_root = repo.root().to_path_buf();
    let main_root = repo
        .main_root()
        .map_err(|error| UsageError::Message(error.to_string()))?;
    // The same key gates use, so one repository keeps one key across gate
    // leases and console leases alike. The main-checkout anchoring this call
    // site used to do by hand now lives in `git_origin_fingerprint` itself
    // (#170), so every caller gets it.
    let repository = crate::gates::git_origin_fingerprint(&repo);
    let config = crate::ConsoleConfig::load(&main_root);
    Ok((config, repository, repo, main_root, worktree_root))
}

pub(super) fn run_console(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let (config, repository, repo, main_root, worktree_root) = console_context()?;
    let revision = crate::console_revision(&repo)?;
    let identity = crate::console_identity(&config, &repository, &main_root, &worktree_root)
        .with_revision(&revision);
    match action {
        "status" | "list" => {
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let leases = crate::console_leases(&coordinator.list(false)?, &repository);
            let markers = crate::read_console_markers()?;
            let canonical_fingerprint = crate::worktree_fingerprint(&main_root);
            if parsed.json {
                let running: Vec<_> = leases
                    .iter()
                    .map(|lease| {
                        let marker = crate::console_marker_for_lease(&markers, lease);
                        let marker_revision = marker.map(|record| {
                            serde_json::json!({
                                "branch": record.marker.branch,
                                "commit": record.marker.commit,
                                "dirty": record.marker.dirty,
                                "integration_branch": record.marker.integration_branch,
                                "integration_head": record.marker.integration_head,
                                "integration_relation": record.marker.integration_relation,
                                "ahead_commits": record.marker.ahead_commits,
                                "behind_commits": record.marker.behind_commits,
                            })
                        });
                        serde_json::json!({
                            "lease_id": lease.lease_id,
                            "port": crate::console_port(lease),
                            "worktree_fingerprint": lease.worktree_fingerprint,
                            "canonical": marker.map_or(
                                lease.worktree_fingerprint == canonical_fingerprint,
                                |record| record.marker.canonical,
                            ),
                            "parallel": marker.is_some_and(|record| record.marker.parallel),
                            "worktree": marker.map(|record| record.marker.worktree.clone()),
                            "branch": marker.map(|record| record.marker.branch.clone()),
                            "commit": marker.map(|record| record.marker.commit.clone()),
                            "dirty": marker.map(|record| record.marker.dirty),
                            "integration_branch": marker.map(|record| record.marker.integration_branch.clone()),
                            "integration_head": marker.and_then(|record| record.marker.integration_head.clone()),
                            "integration_relation": marker.map(|record| record.marker.integration_relation),
                            "ahead_commits": marker.map(|record| record.marker.ahead_commits),
                            "behind_commits": marker.map(|record| record.marker.behind_commits),
                            "revision": marker_revision,
                            "marker": marker.map(|record| serde_json::json!({
                                "path": record.path,
                                "digest": record.marker.marker_digest,
                            })),
                            "state": lease.state.as_str(),
                            "holder_pid": lease.holder_pid,
                            "expires_at": lease.expires_at,
                        })
                    })
                    .collect();
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "identity": identity,
                        "canonical_checkout": main_root,
                        "this_checkout": worktree_root,
                        "running": running,
                    }))?
                );
            } else {
                out!("Console mode: {}", config.mode.as_str());
                out!("Canonical checkout: {}", main_root.display());
                out!(
                    "This checkout: {} ({})",
                    worktree_root.display(),
                    if identity.canonical {
                        "canonical"
                    } else {
                        "agent worktree — not canonical"
                    }
                );
                out!(
                    "Current revision: {} @ {} ({}, integration: {}{}{})",
                    revision.branch,
                    short_sha(&revision.commit),
                    if revision.dirty { "dirty" } else { "clean" },
                    revision.integration_relation.as_str(),
                    revision
                        .integration_head
                        .as_deref()
                        .map(|head| format!(" @ {}", short_sha(head)))
                        .unwrap_or_default(),
                    if revision.integration_relation
                        == crate::ConsoleIntegrationRelation::Unavailable
                    {
                        " (ref unavailable)"
                    } else {
                        ""
                    }
                );
                if leases.is_empty() {
                    out!("Running consoles: none");
                } else {
                    out!("Running consoles: {}", leases.len());
                    for lease in &leases {
                        let marker = crate::console_marker_for_lease(&markers, lease);
                        let revision_label = marker.map_or_else(
                            || "marker missing".to_string(),
                            |record| {
                                format!(
                                    "{} @ {}{}",
                                    record.marker.branch,
                                    short_sha(&record.marker.commit),
                                    if record.marker.dirty { " (dirty)" } else { "" }
                                )
                            },
                        );
                        out!(
                            "  {:<10} port {:<6} pid {:<8} {}{} — {}{}",
                            lease.state.as_str(),
                            crate::console_port(lease).unwrap_or("-"),
                            lease
                                .holder_pid
                                .map_or_else(|| "-".into(), |pid| pid.to_string()),
                            &lease.worktree_fingerprint[..12.min(lease.worktree_fingerprint.len())],
                            if lease.worktree_fingerprint == canonical_fingerprint {
                                " (canonical)"
                            } else {
                                " (worktree)"
                            },
                            revision_label,
                            if marker.is_some_and(|record| record.marker.parallel) {
                                " [parallel]"
                            } else {
                                ""
                            },
                        );
                    }
                }
            }
        }
        "plan" => {
            let Some(request) = crate::console_request_with_options(
                &config,
                &repository,
                &worktree_root,
                "plan",
                None,
                parsed.allow_parallel,
            ) else {
                return unmanaged_notice(parsed.json, "plan");
            };
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let plan = coordinator.plan(&request)?;
            if parsed.json {
                let mut value = serde_json::to_value(&plan)?;
                value["allow_parallel"] = serde_json::Value::Bool(parsed.allow_parallel);
                out!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                out!(
                    "Console plan ({}) — {} (advisory; run is authoritative)",
                    config.mode.as_str(),
                    if plan.available {
                        "available"
                    } else {
                        "blocked"
                    }
                );
                for allocation in &plan.proposed {
                    out!(
                        "  proposed {:<12} {:<14} {}",
                        allocation.key,
                        allocation.kind,
                        allocation.value
                    );
                }
                for conflict in &plan.conflicts {
                    out!(
                        "  conflict {:<12} {}",
                        conflict.resource_key,
                        conflict.reason
                    );
                }
            }
        }
        "run" => {
            if parsed.exec_command.is_empty() {
                return Err(UsageError::Message(
                    "console run requires -- <command> [args...]".into(),
                ));
            }
            let run_id = format!(
                "pid{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            );
            let Some(request) = crate::console_request_with_options(
                &config,
                &repository,
                &worktree_root,
                &run_id,
                Some(std::process::id()),
                parsed.allow_parallel,
            ) else {
                // Unmanaged reserves nothing, so there is nothing to supervise.
                // Running the command anyway keeps one spelling of "start the
                // console" working in every mode.
                return run_unmanaged_console(&parsed.exec_command, &worktree_root, parsed.json);
            };
            let wait = parsed
                .wait
                .as_deref()
                .map(parse_resource_duration)
                .transpose()?
                .unwrap_or_default();
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let json = parsed.json;
            let mut marker_path = None;
            let marker_repository = repository.clone();
            let marker_worktree = worktree_root.clone();
            let report = coordinator.run_supervised_with_environment(
                &request,
                wait,
                &parsed.exec_command,
                parsed.cleanup_command.as_deref(),
                &worktree_root,
                |grant| {
                    let marker = crate::ConsoleRuntimeMarker::for_grant(
                        grant,
                        &marker_repository,
                        &marker_worktree,
                        config.mode,
                        identity.canonical,
                        parsed.allow_parallel,
                        &crate::console_revision(&repo).map_err(|error| {
                            std::io::Error::other(format!(
                                "cannot identify console revision: {error}"
                            ))
                        })?,
                    )?;
                    let path = crate::write_console_marker(&marker)?;
                    marker_path = Some(path.clone());
                    if json {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "type": "console_marker",
                                "marker": {
                                    "path": path,
                                    "digest": marker.marker_digest,
                                    "repository": marker.repository,
                                    "branch": marker.branch,
                                    "commit": marker.commit,
                                    "dirty": marker.dirty,
                                    "worktree": marker.worktree,
                                    "port": marker.port,
                                    "canonical": marker.canonical,
                                    "parallel": marker.parallel,
                                    "integration_branch": marker.integration_branch,
                                    "integration_head": marker.integration_head,
                                    "integration_relation": marker.integration_relation,
                                    "ahead_commits": marker.ahead_commits,
                                    "behind_commits": marker.behind_commits,
                                },
                            })
                        );
                    } else {
                        eprintln!(
                            "console: source={} branch={} commit={} dirty={} canonical={} integration={} port={} marker={}",
                            marker.worktree,
                            marker.branch,
                            marker.commit,
                            if marker.dirty { "yes" } else { "no" },
                            if marker.canonical { "yes" } else { "no" },
                            marker.integration_relation.as_str(),
                            marker.port,
                            path.display()
                        );
                    }
                    Ok(std::collections::BTreeMap::from([
                        (
                            crate::CONSOLE_MARKER_ENV.to_string(),
                            path.display().to_string(),
                        ),
                        (
                            crate::CONSOLE_MARKER_DIGEST_ENV.to_string(),
                            marker.marker_digest.clone(),
                        ),
                    ]))
                },
                |message| {
                    if json {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "type": "console_run_event",
                                "request_id": request.request_id,
                                "message": message,
                            })
                        );
                    } else {
                        eprintln!("console: {message}");
                    }
                },
            );
            let report = match report {
                Ok(report) => report,
                // Contention here is the feature, not a fault: in `singular`
                // it means a console is already serving this repository. Say
                // which one instead of reporting a bare resource conflict.
                Err(crate::HostResourceRunError::Resource(
                    crate::HostResourceError::Conflict { conflicts, .. },
                )) => {
                    let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
                    let running = crate::console_leases(&coordinator.list(false)?, &repository);
                    let mut message =
                        String::from("a console is already running for this repository");
                    for lease in &running {
                        message.push_str(&format!(
                            "\n  port {} pid {} (aethyme broker console status)",
                            crate::console_port(lease).unwrap_or("-"),
                            lease
                                .holder_pid
                                .map_or_else(|| "-".into(), |pid| pid.to_string()),
                        ));
                    }
                    if running.is_empty() {
                        for conflict in &conflicts {
                            message.push_str(&format!(
                                "\n  {} {}",
                                conflict.resource_key, conflict.reason
                            ));
                        }
                    }
                    if let Some(path) = marker_path.as_deref() {
                        crate::remove_console_marker(path)?;
                    }
                    return Err(UsageError::Message(message));
                }
                Err(error) => {
                    if let Some(path) = marker_path.as_deref() {
                        crate::remove_console_marker(path)?;
                    }
                    return Err(error.into());
                }
            };
            if let Some(path) = marker_path.as_deref() {
                crate::remove_console_marker(path).map_err(|error| {
                    UsageError::Message(format!(
                        "console stopped but could not remove runtime marker {}: {error}",
                        path.display()
                    ))
                })?;
            }
            if json {
                eprintln!("{}", serde_json::to_string(&report)?);
            } else {
                eprintln!(
                    "console: child={} final={}",
                    report.child_exit_code,
                    report.final_lease_state.as_str()
                );
            }
            let exit = if report.child_exit_code != 0 {
                report.child_exit_code
            } else if report.authority_lost
                || report.final_lease_state != crate::HostLeaseState::Released
            {
                70
            } else {
                0
            };
            if exit != 0 {
                return Err(UsageError::SilentExit(exit));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown console action {other:?}; expected status, list, plan, or run"
            )));
        }
    }
    Ok(())
}

pub(super) fn unmanaged_notice(json: bool, action: &str) -> Result<(), UsageError> {
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "unmanaged",
                "action": action,
                "reserved": serde_json::Value::Null,
            }))?
        );
    } else {
        out!("Console mode: unmanaged — nothing is reserved and nothing is coordinated.");
    }
    Ok(())
}

/// Unmanaged still runs the command, so a repository can opt out of
/// coordination without every operator learning a second way to start.
pub(super) fn run_unmanaged_console(
    command: &[String],
    cwd: &Path,
    json: bool,
) -> Result<(), UsageError> {
    if !json {
        eprintln!("console: unmanaged mode — no lease, no port reservation");
    }
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(cwd)
        .status()
        .map_err(|error| UsageError::Message(error.to_string()))?;
    // A signal-killed child reports no code; 70 keeps it distinguishable from
    // a clean exit rather than collapsing to success.
    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(70);
    if code != 0 {
        return Err(UsageError::SilentExit(code));
    }
    Ok(())
}

pub(super) fn run_resources(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
            "resources requires plan, explain, acquire, run, renew, release, list, or reconcile"
                .into(),
        )
        })?;
    match action {
        "plan" | "explain" | "acquire" => {
            let path = parsed.positional.get(1).map(PathBuf::from).ok_or_else(|| {
                UsageError::Message(format!("resources {action} requires <request.json>"))
            })?;
            let request: crate::HostResourceRequest = read_resource_json(&path)?;
            if action == "plan" {
                let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
                let plan = coordinator.plan(&request)?;
                if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&plan)?);
                } else {
                    out!(
                        "Request {} — {} (advisory; acquire is authoritative)",
                        plan.request_id,
                        if plan.available {
                            "available"
                        } else {
                            "blocked"
                        }
                    );
                    for allocation in &plan.proposed {
                        out!(
                            "  proposed {:<20} {:<14} {}",
                            allocation.key,
                            allocation.kind,
                            allocation.value
                        );
                    }
                    for conflict in &plan.conflicts {
                        out!(
                            "  conflict {:<20} {}",
                            conflict.resource_key,
                            conflict.reason
                        );
                    }
                }
            } else if action == "explain" {
                let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
                let explanation = coordinator.explain(&request)?;
                if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&explanation)?);
                } else {
                    render_host_resource_explanation(&explanation);
                }
            } else {
                let mut coordinator = crate::HostResourceCoordinator::open_default()?;
                let wait = parsed
                    .wait
                    .as_deref()
                    .map(parse_resource_duration)
                    .transpose()?
                    .unwrap_or_default();
                let started = std::time::Instant::now();
                let acquired = if wait.is_zero() {
                    coordinator.acquire(&request)
                } else {
                    coordinator.acquire_with_wait(&request, wait, |_| {})
                };
                let grant = match acquired {
                    Ok(grant) => grant,
                    Err(crate::HostResourceError::Conflict {
                        code, conflicts, ..
                    }) if parsed.json => {
                        out!(
                            "{}",
                            serde_json::to_string_pretty(&ResourceAcquireFailure {
                                retryable: code == "resource_contention",
                                code: &code,
                                request_id: &request.request_id,
                                waited_ms: started.elapsed().as_millis(),
                                conflicts: &conflicts,
                            })?
                        );
                        return Err(UsageError::SilentExit(75));
                    }
                    Err(error) => return Err(error.into()),
                };
                if let Some(path) = parsed.grant_out.as_deref() {
                    write_private_grant(path, &grant)?;
                }
                if parsed.json && parsed.grant_out.is_some() {
                    out!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "lease": grant.lease,
                            "grant_path": parsed.grant_out,
                        }))?
                    );
                } else if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&grant)?);
                } else {
                    render_host_lease(&grant.lease);
                    if let Some(path) = parsed.grant_out {
                        out!("Private grant: {}", path.display());
                    } else {
                        out!("Ownership token: {}", grant.ownership_token);
                        out!("Store the complete JSON grant privately for renew/release.");
                    }
                }
            }
        }
        "renew" | "release" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let argument = parsed.positional.get(1).ok_or_else(|| {
                UsageError::Message(format!("resources {action} requires <grant.json>"))
            })?;
            let path = PathBuf::from(argument);
            // A lease id here is a natural mistake, and reporting it as a missing
            // file sends the operator looking for the wrong thing (issue #139).
            if !path.exists()
                && let Some(lease) = coordinator
                    .list(false)?
                    .into_iter()
                    .find(|lease| &lease.lease_id == argument)
            {
                return Err(UsageError::Message(format!(
                    "resources {action} takes the grant JSON written at acquire, not a lease \
                         id; {argument} is a {} lease. The grant carries the ownership token that \
                         authorizes {action}, and a holder that died leaves none to reuse -- \
                         reclaim that lease instead: aethyme broker resources reconcile \
                         {argument} --confirm {}",
                    lease.state.as_str(),
                    lease.generation,
                )));
            }
            let mut grant: crate::HostResourceGrant = read_resource_json(&path)?;
            grant.lease = if action == "renew" {
                let ttl = parsed.ttl_seconds.ok_or_else(|| {
                    UsageError::Message("resources renew requires --ttl <seconds>".into())
                })?;
                let ttl = u64::try_from(ttl)
                    .map_err(|_| UsageError::Message("--ttl must be positive".into()))?;
                coordinator.renew(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                    ttl,
                )?
            } else {
                coordinator.release(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                )?
            };
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&grant)?);
            } else {
                render_host_lease(&grant.lease);
            }
        }
        "run" => {
            let path = parsed.positional.get(1).map(PathBuf::from).ok_or_else(|| {
                UsageError::Message("resources run requires <request.json>".into())
            })?;
            if parsed.exec_command.is_empty() {
                return Err(UsageError::Message(
                    "resources run requires -- <command> [args...]".into(),
                ));
            }
            let request: crate::HostResourceRequest = read_resource_json(&path)?;
            let wait = parsed
                .wait
                .as_deref()
                .map(parse_resource_duration)
                .transpose()?
                .unwrap_or_default();
            let cwd =
                std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let json = parsed.json;
            let report = coordinator.run_supervised(
                &request,
                wait,
                &parsed.exec_command,
                parsed.cleanup_command.as_deref(),
                &cwd,
                |message| {
                    if json {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "type": "resource_run_event",
                                "request_id": request.request_id,
                                "message": message,
                            })
                        );
                    } else {
                        eprintln!("resource run: {message}");
                    }
                },
            );
            let report = match report {
                Ok(report) => report,
                Err(crate::HostResourceRunError::Resource(
                    crate::HostResourceError::Conflict {
                        code, conflicts, ..
                    },
                )) if json => {
                    eprintln!(
                        "{}",
                        serde_json::to_string(&ResourceAcquireFailure {
                            retryable: code == "resource_contention",
                            code: &code,
                            request_id: &request.request_id,
                            waited_ms: wait.as_millis(),
                            conflicts: &conflicts,
                        })?
                    );
                    return Err(UsageError::SilentExit(75));
                }
                Err(error) => return Err(error.into()),
            };
            if json {
                eprintln!("{}", serde_json::to_string(&report)?);
            } else {
                eprintln!(
                    "resource run: child={} cleanup={} final={}",
                    report.child_exit_code,
                    report
                        .cleanup_exit_code
                        .map_or_else(|| "not-requested".into(), |code| code.to_string()),
                    report.final_lease_state.as_str()
                );
            }
            let lifecycle_failed = report.authority_lost
                || report.cleanup_exit_code.is_some_and(|code| code != 0)
                || report.final_lease_state != crate::HostLeaseState::Released;
            let exit = if report.child_exit_code != 0 {
                report.child_exit_code
            } else if lifecycle_failed {
                70
            } else {
                0
            };
            if exit != 0 {
                return Err(UsageError::SilentExit(exit));
            }
        }
        "list" => {
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let leases = coordinator.list(parsed.all)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&leases)?);
            } else if leases.is_empty() {
                out!("No active or quarantined host resource leases.");
            } else {
                for lease in &leases {
                    render_host_lease(lease);
                }
            }
        }
        "reap" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let report = coordinator.reap_dead_holders()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Reaped {} dead holder(s): {} capacity unit(s) reclaimed, {} lease(s) released, {} lease(s) retained for cleanup.",
                    report.dead_holders_seen,
                    report.reclaimed_capacity_units,
                    report.released_leases,
                    report.retained_quarantined_leases,
                );
                for lease in &report.leases {
                    out!(
                        "  lease {} generation {} pid {} — reclaimed {} capacity unit(s), {}",
                        lease.lease_id,
                        lease.generation,
                        lease.holder_pid,
                        lease.capacity_units,
                        lease.state.as_str(),
                    );
                }
            }
        }
        "reconcile" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let lease_id = parsed
                .positional
                .get(1)
                .ok_or_else(|| UsageError::Message(RESOURCES_RECONCILE_USAGE.into()))?;
            let generation = parsed
                .confirm
                .as_deref()
                .ok_or_else(|| UsageError::Message(RESOURCES_RECONCILE_USAGE.into()))?
                .parse::<u64>()
                .map_err(|_| {
                    UsageError::Message(format!(
                        "--confirm must be the full numeric generation; {RESOURCES_RECONCILE_USAGE}"
                    ))
                })?;
            let lease = coordinator.reconcile(lease_id, generation)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&lease)?);
            } else {
                render_host_lease(&lease);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown resources action {other:?}; expected plan, explain, acquire, run, renew, release, list, reap, or reconcile"
            )));
        }
    }
    Ok(())
}
