//! `broker advanced integration reconcile`: reconcile the integration branch with a fetched upstream.

use super::*;

pub(super) fn render_integration_reconcile(
    report: &crate::IntegrationReconcileReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!("Local main:  {}", short_commit(&report.local_main));
    out!(
        "Upstream:    {} @ {}",
        report.upstream_ref,
        short_commit(&report.upstream_head)
    );
    out!(
        "Integration: {} -> {}",
        short_commit(&report.old_integration),
        short_commit(&report.new_integration)
    );
    if let Some(path) = &report.resolution_file {
        out!("Resolution:  {path}");
    }
    if let Some(digest) = &report.plan_digest {
        out!("Plan digest: {digest}");
    }
    out!(
        "Result:      {}",
        if report.applied {
            "applied"
        } else if report.safe {
            "safe dry-run"
        } else {
            "blocked"
        }
    );
    for entry in &report.entries {
        out!(
            "  q{} session {}: {} — {}",
            entry.queue_entry_id,
            entry.session_id,
            entry.classification.as_str(),
            entry.evidence
        );
        if !entry.conflicts.is_empty() {
            out!("    conflicts: {}", capped_join(&entry.conflicts, 5));
        }
    }
    if let Some(template) = &report.resolution_template {
        out!(
            "Resolution template: {} recorded, {} unrecorded ({})",
            template.document.resolutions.len(),
            template.document.unrecorded_resolutions.len(),
            if template.complete {
                "complete"
            } else {
                "operator input required"
            }
        );
        out!(
            "  recorded classification: {}",
            template
                .field_contract
                .recorded_classification_allowed_values
                .join(", ")
        );
        for rule in &template.field_contract.unrecorded_dispositions {
            out!(
                "  {}: upstream_commit {}; {}",
                rule.value,
                rule.upstream_commit,
                rule.condition
            );
        }
        out!("  operator: {}", template.field_contract.operator);
        out!("  reason: {}", template.field_contract.reason);
    }
    for warning in &report.warnings {
        out!("Warning: {warning}");
    }
    out!("Next action: {}", report.next_action);
    Ok(())
}

pub(super) fn write_reconciliation_resolution_template(
    path: &std::path::Path,
    document: &crate::IntegrationReconcileResolutionTemplateDocument,
) -> Result<(), UsageError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(UsageError::Message(format!(
            "resolution template parent directory does not exist: {}",
            parent.display()
        )));
    }
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        UsageError::Message(format!(
            "cannot create resolution template beside {}: {error}",
            path.display()
        ))
    })?;
    temporary.write_all(&bytes).map_err(|error| {
        UsageError::Message(format!(
            "cannot write resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        UsageError::Message(format!(
            "cannot sync resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.persist_noclobber(path).map_err(|error| {
        UsageError::Message(format!(
            "refusing to overwrite resolution template {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

/// `broker advanced integration reconcile`.
pub(super) fn run_integration(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "integration requires an action: reconcile".into(),
        ))?;
    match action {
        "reconcile" => {
            if parsed.apply && parsed.dry_run {
                return Err(UsageError::Message(format!(
                    "choose either --dry-run or --apply, not both; {INTEGRATION_RECONCILE_USAGE}"
                )));
            }
            let upstream = parsed
                .upstream
                .clone()
                .ok_or(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()))?;
            if parsed.apply && parsed.confirm.is_none() {
                return Err(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()));
            }
            if parsed.apply && parsed.write_resolution_template.is_some() {
                return Err(UsageError::Message(format!(
                    "--write-resolution-template is a dry-run aid and cannot be combined with --apply; {INTEGRATION_RECONCILE_USAGE}"
                )));
            }
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.reconcile_integration(crate::IntegrationReconcileOptions {
                upstream,
                apply: parsed.apply,
                resolution_file: parsed.resolution_file.clone(),
                confirm: parsed.confirm.clone(),
            })?;
            if let Some(path) = parsed.write_resolution_template.as_deref() {
                let template = report.resolution_template.as_ref().ok_or_else(|| {
                    UsageError::Message(
                        "no reconciliation resolution template is required for this plan".into(),
                    )
                })?;
                write_reconciliation_resolution_template(path, &template.document)?;
                eprintln!("Wrote resolution template to {}", path.display());
            }
            render_integration_reconcile(&report, parsed.json)?;
            if !report.safe {
                return Err(UsageError::Message(
                    "integration reconciliation is ambiguous or conflicting; no state changed"
                        .into(),
                ));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown integration action {other:?} — expected reconcile"
            )));
        }
    }
    Ok(())
}
