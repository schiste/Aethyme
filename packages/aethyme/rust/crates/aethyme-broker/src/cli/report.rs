//! `broker report`, `quality-report` and `external-events`: filing and ingesting findings.

use super::*;

pub(super) fn run_external_events(parsed: Parsed) -> Result<(), UsageError> {
    const MAX_INPUT_BYTES: u64 = 64 * 1024;
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message("external-events requires ingest, list, show, or reconcile".into())
        })?;
    match action {
        "ingest" => {
            let path = parsed.positional.get(1).ok_or_else(|| {
                UsageError::Message("external-events ingest requires <normalized.json>".into())
            })?;
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(
                    "external-events ingest accepts exactly one normalized JSON path".into(),
                ));
            }
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(UsageError::Message(
                    "external event input must be a regular non-symlink file".into(),
                ));
            }
            if metadata.len() > MAX_INPUT_BYTES {
                return Err(UsageError::Message(format!(
                    "external event input exceeds {MAX_INPUT_BYTES} bytes"
                )));
            }
            let bytes = std::fs::read(path)?;
            let envelope: crate::ExternalEventEnvelope =
                serde_json::from_slice(&bytes).map_err(|error| {
                    UsageError::Message(format!("invalid external event JSON: {error}"))
                })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.ingest_external_event(envelope, now_ms())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "External event {}: {}{}",
                    report.event.id,
                    report.event.status.as_str(),
                    if report.deduplicated {
                        " (idempotent redelivery)"
                    } else {
                        ""
                    }
                );
                if let Some(session_id) = report.event.session_id {
                    out!("  owner session: {session_id}");
                }
                if let Some(remediation) = report.remediation {
                    out!("  reconcile: {remediation}");
                }
                out!("  policy effect: advisory only; no gate or submit state changed");
            }
        }
        "list" => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "external-events list accepts no positional arguments".into(),
                ));
            }
            let mut broker = open_broker(true)?;
            let events = broker.store().external_events(parsed.all)?;
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": crate::EXTERNAL_EVENT_SCHEMA_VERSION,
                        "events": events,
                        "includes_terminal": parsed.all,
                        "limit": 500,
                    }))?
                );
            } else if events.is_empty() {
                out!("No matching external coordination events.");
            } else {
                out!("{:<5} {:<24} {:<22} OWNER", "ID", "TYPE", "STATUS");
                for event in events {
                    out!(
                        "{:<5} {:<24} {:<22} {}",
                        event.id,
                        event.event_type,
                        event.status.as_str(),
                        event
                            .session_id
                            .map(|session| format!("session {session}"))
                            .unwrap_or_else(|| "unresolved".into())
                    );
                }
            }
        }
        "show" => {
            let id = external_event_positional_id(&parsed, "show")?;
            let mut broker = open_broker(true)?;
            let event = broker
                .store()
                .external_event(id)?
                .ok_or(crate::BrokerError::ExternalEventNotFound(id))?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&event)?);
            } else {
                out!("External event {}:", event.id);
                out!(
                    "  type/status: {} / {}",
                    event.event_type,
                    event.status.as_str()
                );
                out!("  repository: {}", event.repository);
                out!("  PR/commit: #{} / {}", event.pr_number, event.commit_sha);
                out!(
                    "  owner: {}",
                    event
                        .session_id
                        .map(|session| format!("session {session}"))
                        .unwrap_or_else(|| "unresolved".into())
                );
                out!("  policy effect: advisory only");
            }
        }
        "reconcile" => {
            let id = external_event_positional_id(&parsed, "reconcile")?;
            let outcome = parsed.outcome.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "external-events reconcile requires --outcome <assign|ignore> --reason <text> and --session <id> when assigning"
                        .into(),
                )
            })?;
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "external-events reconcile requires --outcome <assign|ignore> --reason <text> and --session <id> when assigning"
                        .into(),
                )
            })?;
            let resolution = match outcome {
                "assign" => crate::ExternalEventReconciliation::Assign {
                    session_id: parsed.session.ok_or_else(|| {
                        UsageError::Message(
                            "external-events reconcile --outcome assign requires --session <id>"
                                .into(),
                        )
                    })?,
                },
                "ignore" if parsed.session.is_none() => crate::ExternalEventReconciliation::Ignore,
                "ignore" => {
                    return Err(UsageError::Message(
                        "external-events reconcile --outcome ignore does not accept --session"
                            .into(),
                    ));
                }
                _ => {
                    return Err(UsageError::Message(
                        "external-events reconcile --outcome must be assign or ignore".into(),
                    ));
                }
            };
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.reconcile_external_event(id, resolution, reason, now_ms())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "External event {} reconciled as {} (reason stored as SHA-256 only).",
                    report.event.id,
                    report.event.status.as_str()
                );
                out!("Policy effect: advisory only; no gate or submit state changed.");
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown external-events action {other:?}; expected ingest, list, show, or reconcile"
            )));
        }
    }
    Ok(())
}

pub(super) fn external_event_positional_id(
    parsed: &Parsed,
    action: &str,
) -> Result<i64, UsageError> {
    if parsed.positional.len() != 2 {
        return Err(UsageError::Message(format!(
            "external-events {action} requires exactly one event id"
        )));
    }
    parsed.positional[1]
        .parse()
        .map_err(|_| UsageError::Message("external event id must be an integer".into()))
}

pub(super) fn run_quality_report(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "quality-report requires plan or publish followed by a report path".into(),
            )
        })?;
    let path = parsed.positional.get(1).ok_or_else(|| {
        UsageError::Message("quality-report requires exactly one report path".into())
    })?;
    if parsed.positional.len() != 2 {
        return Err(UsageError::Message(
            "quality-report accepts exactly one report path".into(),
        ));
    }
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("quality-report requires --repo <owner/name>".into()))?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("quality-report requires --pr <number>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let projection_policy =
        crate::PrProjectionPolicy::load(broker.main_root()).map_err(to_usage)?;
    if !projection_policy.enabled || !projection_policy.comment {
        return Err(UsageError::Message(
            "quality-report publication is opt-in: enable [review.projection] with comment = true"
                .into(),
        ));
    }
    let report = crate::QualityReport::from_path(Path::new(path))
        .map_err(|error| UsageError::Message(error.to_string()))?;
    let facts = read_quality_report_facts(&repository, pull_request)?;
    let plan = crate::plan_quality_report(&report, &facts)
        .map_err(|error| UsageError::Message(error.to_string()))?;

    if action == "plan" {
        if parsed.json {
            out!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            render_quality_report_plan(&plan, pull_request);
        }
        return Ok(());
    }
    if action != "publish" {
        return Err(UsageError::Message(format!(
            "unknown quality-report action {action:?}; expected plan or publish"
        )));
    }

    let Some(publication) = plan.action.as_ref() else {
        broker.store().append_event(
            crate::events::QUALITY_REPORT_PUBLISHED,
            parsed.session,
            Some(&crate::events::quality_report_publication_payload(
                &report, "noop", None, None,
            )),
        )?;
        if parsed.json {
            out!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "plan": plan,
                    "outcome": "noop",
                }))?
            );
        } else {
            out!("Quality report already matches the owned pull-request summary.");
            out!("{}", plan.explanation);
        }
        return Ok(());
    };
    let session_id = parsed.session.ok_or_else(|| {
        UsageError::Message("quality-report publish requires --session <id>".into())
    })?;
    let operation = match broker.run_coordinated_operation(crate::CoordinatedCommand {
        session_id,
        provider: crate::OperationProvider::Github,
        repository: Some(repository.clone()),
        resolved_target: None,
        scope: Some(format!("pr/{pull_request}/quality-report")),
        declared_effect: Some(crate::OperationEffect::Write),
        destructive_confirmed: false,
        authorization_reason: Some(publication.reason(pull_request, &plan.report_digest)),
        args: publication.gh_args(pull_request),
    }) {
        Ok(operation) => operation,
        Err(error) => {
            broker.store().append_event(
                crate::events::QUALITY_REPORT_PUBLICATION_FAILED,
                Some(session_id),
                Some(&crate::events::quality_report_publication_payload(
                    &report, "failed", None, None,
                )),
            )?;
            return Err(UsageError::Message(error.to_string()));
        }
    };
    let external_id = extract_quality_report_external_id(&operation.stdout);
    let succeeded = operation.command_success;
    let event_kind = if succeeded {
        crate::events::QUALITY_REPORT_PUBLISHED
    } else {
        crate::events::QUALITY_REPORT_PUBLICATION_FAILED
    };
    broker.store().append_event(
        event_kind,
        Some(session_id),
        Some(&crate::events::quality_report_publication_payload(
            &report,
            if succeeded { "published" } else { "failed" },
            Some(operation.operation.id),
            external_id.as_deref(),
        )),
    )?;
    if parsed.json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plan": plan,
                "operation_id": operation.operation.id,
                "success": succeeded,
                "external_id": external_id,
            }))?
        );
    } else {
        out!(
            "Quality report {} (operation {}).",
            if succeeded { "published" } else { "failed" },
            operation.operation.id
        );
        out!("{}", plan.explanation);
        if let Some(external_id) = external_id {
            out!("External reference: {external_id}");
        }
    }
    if !succeeded {
        return Err(UsageError::Message(
            "quality-report publication failed; inspect the coordinated operation and retry only after reviewing it".into(),
        ));
    }
    Ok(())
}

pub(super) fn read_quality_report_facts(
    repository: &str,
    pull_request: i64,
) -> Result<crate::QualityReportPublicationFacts, UsageError> {
    if pull_request <= 0 {
        return Err(UsageError::Message("--pr must be positive".into()));
    }
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "number,headRefOid,baseRefOid,comments",
        ])
        .output()
        .map_err(|error| UsageError::Message(format!("cannot run gh pr view: {error}")))?;
    if !output.status.success() {
        return Err(UsageError::Message(format!(
            "gh could not read {repository}#{pull_request}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| UsageError::Message(format!("gh returned invalid PR JSON: {error}")))?;
    let number = value["number"]
        .as_i64()
        .ok_or_else(|| UsageError::Message("gh PR JSON omitted number".into()))?;
    if number != pull_request {
        return Err(UsageError::Message(format!(
            "gh returned PR #{number} while #{pull_request} was requested"
        )));
    }
    let head_revision = value["headRefOid"]
        .as_str()
        .ok_or_else(|| UsageError::Message("gh PR JSON omitted headRefOid".into()))?;
    let base_revision = value["baseRefOid"]
        .as_str()
        .ok_or_else(|| UsageError::Message("gh PR JSON omitted baseRefOid".into()))?;
    let comments = value["comments"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let owned_comment =
        owned_comment_from_view(repository, pull_request, comments)?.map(|comment| {
            crate::QualityReportComment {
                id: comment.id,
                body: comment.body,
            }
        });
    Ok(crate::QualityReportPublicationFacts {
        repository: repository.into(),
        pull_request: number,
        head_revision: head_revision.into(),
        base_revision: base_revision.into(),
        owned_comment,
    })
}

pub(super) fn render_quality_report_plan(
    plan: &crate::QualityReportPublicationPlan,
    pull_request: i64,
) {
    out!(
        "Quality report for PR #{pull_request}: {}",
        plan.status.as_str()
    );
    out!("Digest: {}", plan.report_digest);
    out!("{}", plan.explanation);
    match &plan.action {
        Some(action) => out!("Planned GitHub action: {:?}", action),
        None => out!("Planned GitHub action: none (idempotent no-op)"),
    }
}

pub(super) fn extract_quality_report_external_id(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .find(|token| token.contains("#issuecomment-") || token.contains("/pull/"))
        .map(|token| {
            token
                .trim_matches(|character: char| ",.!)]}".contains(character))
                .to_string()
        })
}

pub(super) fn run_report(parsed: Parsed) -> Result<(), UsageError> {
    match parsed.positional.first().map(String::as_str) {
        Some("capture") if parsed.positional.len() == 1 => {
            if parsed.stdout && parsed.output.is_some() {
                return Err(UsageError::Message(
                    "--stdout and --output are mutually exclusive".into(),
                ));
            }
            if parsed.stdout && parsed.json {
                return Err(UsageError::Message(
                    "--stdout already emits the JSON report; do not combine it with --json".into(),
                ));
            }
            let kind = crate::ReportKind::parse(
                parsed
                    .kind
                    .as_deref()
                    .ok_or(UsageError::Message("report capture requires --kind".into()))?,
            )?;
            let title = parsed.title.as_deref().ok_or(UsageError::Message(
                "report capture requires --title".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let selected_session = if let Some(session_id) = parsed.session {
                Some(session_id)
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|error| UsageError::Message(error.to_string()))?;
                let checkout = crate::GitRepo::discover(&cwd)?;
                let worktree = checkout.root().to_string_lossy();
                broker
                    .store()
                    .session_for_worktree(&worktree)?
                    .map(|session| session.id)
            };
            let prepared = crate::prepare_report(
                &mut broker,
                kind,
                title,
                selected_session,
                parsed.include_task,
                now_ms(),
            )?;
            if parsed.stdout {
                use std::io::Write;
                std::io::stdout()
                    .lock()
                    .write_all(&prepared.bytes)
                    .map_err(|error| UsageError::Message(error.to_string()))?;
                eprintln!("SHA-256: {}", prepared.sha256);
            } else {
                let result = crate::write_report_atomic(
                    broker.main_root(),
                    parsed.output.as_deref(),
                    &prepared,
                )?;
                if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    out!(
                        "Captured {} report: {}",
                        kind.as_str(),
                        result.path.as_deref().unwrap_or("-")
                    );
                    out!("SHA-256: {}", result.sha256);
                    out!("Review this local report before any later filing step.");
                }
            }
            Ok(())
        }
        Some("list") if parsed.positional.len() == 1 => {
            let main_root = report_main_root()?;
            let report = crate::list_reports(&main_root)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.reports.is_empty() && report.invalid.is_empty() {
                out!("No captured reports.");
            } else {
                if report.reports.is_empty() {
                    out!("No valid captured reports.");
                } else {
                    out!(
                        "CAPTURED_AT    KIND          STATE     VERSION          DIGEST       PATH"
                    );
                    for item in &report.reports {
                        out!(
                            "{:<14} {:<13} {:<9} {:<16} {:<12} {}",
                            item.captured_at,
                            item.kind.as_str(),
                            match item.filing_state {
                                crate::ReportFilingState::Unfiled => "unfiled",
                                crate::ReportFilingState::Filed => "filed",
                            },
                            item.version,
                            &item.digest[..12],
                            item.path,
                        );
                    }
                }
                for invalid in &report.invalid {
                    eprintln!("Invalid report {}: {}", invalid.path, invalid.error);
                }
            }
            Ok(())
        }
        Some("show") if parsed.positional.len() == 2 => {
            let main_root = report_main_root()?;
            let inspection =
                crate::show_report(&main_root, PathBuf::from(&parsed.positional[1]).as_path())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                out!("Report: {}", inspection.summary.path);
                out!("  title: {}", inspection.summary.title);
                out!("  captured at: {}", inspection.summary.captured_at);
                out!("  kind: {}", inspection.summary.kind.as_str());
                out!("  version: {}", inspection.summary.version);
                out!("  digest: {}", inspection.summary.digest);
                out!(
                    "  filing state: {}",
                    match inspection.summary.filing_state {
                        crate::ReportFilingState::Unfiled => "unfiled",
                        crate::ReportFilingState::Filed => "filed",
                    }
                );
                out!("\n{}", serde_json::to_string_pretty(&inspection.report)?);
            }
            Ok(())
        }
        Some("render") if parsed.positional.len() == 2 => {
            let main_root = report_main_root()?;
            let form = parsed.form.as_deref().ok_or(UsageError::Message(
                "report render requires --form <form.yml>".into(),
            ))?;
            let rendered = crate::render_issue_form(
                &main_root,
                PathBuf::from(&parsed.positional[1]).as_path(),
                form,
            )?;
            if let Some(output) = parsed.output.as_deref() {
                let written = crate::write_issue_form_render_atomic(&main_root, output, &rendered)?;
                if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&written)?);
                } else {
                    out!("Rendered reviewed report: {}", written.path);
                    out!("SHA-256: {}", written.sha256);
                    if !written.valid {
                        out!(
                            "Edit the required unfilled sections before filing: {}",
                            written.missing_required.join(", ")
                        );
                    }
                }
            } else if parsed.json {
                out!("{}", serde_json::to_string_pretty(&rendered)?);
            } else {
                print!("{}", rendered.markdown);
                eprintln!("Issue title: {}", rendered.issue_title);
                eprintln!("Report SHA-256: {}", rendered.report_digest);
            }
            if rendered.valid {
                Ok(())
            } else {
                Err(UsageError::Exit {
                    message: format!(
                        "required issue-form fields remain unfilled: {}",
                        rendered.missing_required.join(", ")
                    ),
                    code: 1,
                })
            }
        }
        Some("file") if parsed.positional.len() == 2 => {
            let repository = parsed.repository.as_deref().ok_or(UsageError::Message(
                "report file requires --repo <owner/name>".into(),
            ))?;
            let confirmation = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "report file requires --confirm <sha256>".into(),
            ))?;
            let cwd = std::env::current_dir()
                .map_err(|error| UsageError::Message(error.to_string()))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let worktree = checkout.root().to_string_lossy();
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let session = broker
                .store()
                .session_for_worktree(&worktree)?
                .ok_or(UsageError::Message(
                    "report file requires a broker session for the current worktree; run `aethyme broker adopt --task \"File reviewed report\"` first".into(),
                ))?;
            let filed = crate::file_reviewed_report(
                &mut broker,
                session.id,
                PathBuf::from(&parsed.positional[1]).as_path(),
                repository,
                confirmation,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&filed)?);
            } else {
                match filed.state {
                    crate::ReportFileState::Filed => {
                        out!(
                            "Filed {} as {}#{}",
                            filed.path,
                            filed.repository,
                            filed.issue_number.unwrap_or_default()
                        );
                        if let Some(url) = filed.issue_url.as_deref() {
                            out!("Issue: {url}");
                        }
                        out!("Operation: {}", filed.operation_id);
                    }
                    crate::ReportFileState::ReconciliationRequired => {
                        out!(
                            "Report filing outcome is unknown (operation {}).",
                            filed.operation_id
                        );
                    }
                }
            }
            if filed.state == crate::ReportFileState::ReconciliationRequired {
                let operation = broker
                    .store()
                    .coordinated_operation(filed.operation_id)?
                    .ok_or_else(|| {
                        UsageError::Message(format!(
                            "coordinated operation {} disappeared before recovery guidance could be rendered",
                            filed.operation_id
                        ))
                    })?;
                return Err(UsageError::Exit {
                    message: crate::UnknownOutcomeRecovery::from_operation(&operation).to_string(),
                    code: 1,
                });
            }
            Ok(())
        }
        Some("capture" | "list" | "show" | "render" | "file") => Err(UsageError::Message(
            "invalid report arguments; expected capture, list, show <filename>, render <filename> --form <form.yml> [--output <name>.issue.md], or file <path> --repo <owner/name> --confirm <sha256>".into(),
        )),
        Some(other) => Err(UsageError::Message(format!(
            "unknown report action {other:?}; expected capture, list, show, render, or file"
        ))),
        None => Err(UsageError::Message(
            "report requires an action: capture, list, show, render, or file".into(),
        )),
    }
}

pub(super) fn report_main_root() -> Result<PathBuf, UsageError> {
    let cwd = std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
    let repo = crate::GitRepo::discover(&cwd)?;
    Ok(repo.main_root()?)
}
