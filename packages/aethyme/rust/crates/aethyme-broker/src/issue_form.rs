//! Offline rendering of captured reports into repository-owned GitHub issue forms.
//!
//! This module deliberately depends only on local files and the allowlisted report
//! schema. It does not open broker state, execute Git, or construct a network client.

use std::fmt::Write as _;
use std::path::{Component, Path};

use crate::{
    ReportCaptureError, ReportDocument, ReportInspection, ReportLastFailure, ReportSnapshot,
    show_report,
};

pub const ISSUE_FORM_RENDER_SCHEMA_VERSION: u32 = 1;
const ISSUE_FORM_MAX_BYTES: u64 = 1024 * 1024;
const ISSUE_FORM_DIRECTORY: &str = ".github/ISSUE_TEMPLATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueFormFieldKind {
    Input,
    Textarea,
    Dropdown,
    Checkboxes,
    Markdown,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueFormFieldStatus {
    Mapped,
    Unfilled,
    Static,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IssueFormRenderedField {
    pub id: Option<String>,
    pub label: Option<String>,
    pub kind: IssueFormFieldKind,
    pub required: bool,
    pub status: IssueFormFieldStatus,
}

/// Complete local render result. `valid` is false when one or more required
/// answer fields remain unfilled; the Markdown is still returned for review.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IssueFormRenderResult {
    pub schema_version: u32,
    pub report_path: String,
    pub report_digest: String,
    pub form_path: String,
    pub form_name: String,
    pub issue_title: String,
    pub valid: bool,
    pub missing_required: Vec<String>,
    pub fields: Vec<IssueFormRenderedField>,
    pub markdown: String,
}

#[derive(Debug, serde::Deserialize)]
struct IssueForm {
    name: String,
    #[serde(default)]
    title: String,
    body: Vec<IssueFormItem>,
}

#[derive(Debug, serde::Deserialize)]
struct IssueFormItem {
    #[serde(rename = "type")]
    item_type: String,
    id: Option<String>,
    #[serde(default)]
    attributes: IssueFormAttributes,
    #[serde(default)]
    validations: IssueFormValidations,
}

#[derive(Debug, Default, serde::Deserialize)]
struct IssueFormAttributes {
    label: Option<String>,
    value: Option<String>,
    render: Option<String>,
    #[serde(default)]
    options: Vec<IssueFormOption>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum IssueFormOption {
    Label(String),
    Detailed {
        label: String,
        #[serde(default)]
        required: bool,
    },
}

impl IssueFormOption {
    fn label(&self) -> &str {
        match self {
            Self::Label(label) | Self::Detailed { label, .. } => label,
        }
    }

    fn required(&self) -> bool {
        matches!(self, Self::Detailed { required: true, .. })
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct IssueFormValidations {
    #[serde(default)]
    required: bool,
}

/// Render one captured report through one `.github/ISSUE_TEMPLATE/*.yml`
/// form. Both selectors are confined to their repository-owned directories.
pub fn render_issue_form(
    main_root: &Path,
    report_requested: &Path,
    form_requested: &Path,
) -> Result<IssueFormRenderResult, ReportCaptureError> {
    let inspection = show_report(main_root, report_requested)?;
    let (form_relative, form) = read_issue_form(main_root, form_requested)?;
    let mut markdown = String::new();
    let mut fields = Vec::with_capacity(form.body.len());
    let mut missing_required = Vec::new();

    for (index, item) in form.body.iter().enumerate() {
        let kind = field_kind(&item.item_type);
        if kind == IssueFormFieldKind::Markdown {
            if let Some(value) = item.attributes.value.as_deref() {
                append_static_markdown(&mut markdown, value);
            }
            fields.push(IssueFormRenderedField {
                id: item.id.clone(),
                label: item.attributes.label.clone(),
                kind,
                required: false,
                status: IssueFormFieldStatus::Static,
            });
            continue;
        }

        let id = item.id.as_deref().unwrap_or("");
        let label = item
            .attributes
            .label
            .as_deref()
            .or(item.id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Unnamed field {}", index + 1));
        let required = item.validations.required
            || item
                .attributes
                .options
                .iter()
                .any(IssueFormOption::required);
        let mapped = mapped_value(id, kind, item, &inspection);
        let status = if mapped.is_some() {
            IssueFormFieldStatus::Mapped
        } else {
            IssueFormFieldStatus::Unfilled
        };

        writeln!(markdown, "## {label}\n").expect("writing to String cannot fail");
        match mapped {
            Some(value) => append_answer(&mut markdown, &value, item.attributes.render.as_deref()),
            None => append_unfilled(&mut markdown, id, kind),
        }

        if required && status == IssueFormFieldStatus::Unfilled {
            missing_required.push(if id.is_empty() {
                label.clone()
            } else {
                id.to_string()
            });
        }
        fields.push(IssueFormRenderedField {
            id: item.id.clone(),
            label: Some(label),
            kind,
            required,
            status,
        });
    }

    Ok(IssueFormRenderResult {
        schema_version: ISSUE_FORM_RENDER_SCHEMA_VERSION,
        report_path: inspection.summary.path,
        report_digest: inspection.summary.digest,
        form_path: form_relative,
        form_name: form.name,
        issue_title: format!("{}{}", form.title, inspection.report.title),
        valid: missing_required.is_empty(),
        missing_required,
        fields,
        markdown: markdown.trim_end().to_string() + "\n",
    })
}

fn read_issue_form(
    main_root: &Path,
    requested: &Path,
) -> Result<(String, IssueForm), ReportCaptureError> {
    let filename = issue_form_filename(requested)?;
    let relative = format!("{ISSUE_FORM_DIRECTORY}/{filename}");
    let github = main_root.join(".github");
    let forms_root = github.join("ISSUE_TEMPLATE");
    let path = forms_root.join(filename);
    for candidate in [&github, &forms_root, &path] {
        if std::fs::symlink_metadata(candidate)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(ReportCaptureError::SymlinkedIssueFormPath(relative));
        }
    }
    let metadata = std::fs::metadata(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ReportCaptureError::IssueFormNotFound(relative.clone())
        } else {
            ReportCaptureError::Io {
                action: "inspect issue form",
                path: relative.clone(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(ReportCaptureError::InvalidIssueForm {
            path: relative,
            reason: "not a regular file".into(),
        });
    }
    if metadata.len() > ISSUE_FORM_MAX_BYTES {
        return Err(ReportCaptureError::InvalidIssueForm {
            path: relative,
            reason: format!("file exceeds {ISSUE_FORM_MAX_BYTES} bytes"),
        });
    }
    let source = std::fs::read_to_string(&path).map_err(|source| ReportCaptureError::Io {
        action: "read issue form",
        path: relative.clone(),
        source,
    })?;
    let form = noyalib::from_str::<IssueForm>(&source).map_err(|error| {
        ReportCaptureError::InvalidIssueForm {
            path: relative.clone(),
            reason: error.to_string(),
        }
    })?;
    if form.name.trim().is_empty() {
        return Err(ReportCaptureError::InvalidIssueForm {
            path: relative,
            reason: "name must not be empty".into(),
        });
    }
    Ok((relative, form))
}

fn issue_form_filename(requested: &Path) -> Result<&str, ReportCaptureError> {
    let components = requested.components().collect::<Vec<_>>();
    let filename = match components.as_slice() {
        [Component::Normal(filename)] => filename.to_str(),
        [
            Component::Normal(github),
            Component::Normal(directory),
            Component::Normal(filename),
        ] if *github == ".github" && *directory == "ISSUE_TEMPLATE" => filename.to_str(),
        _ => None,
    }
    .filter(|name| !name.is_empty() && Path::new(name).extension().is_some_and(|ext| ext == "yml"))
    .ok_or_else(|| ReportCaptureError::InvalidIssueFormPath(requested.display().to_string()))?;
    Ok(filename)
}

fn field_kind(value: &str) -> IssueFormFieldKind {
    match value {
        "input" => IssueFormFieldKind::Input,
        "textarea" => IssueFormFieldKind::Textarea,
        "dropdown" => IssueFormFieldKind::Dropdown,
        "checkboxes" => IssueFormFieldKind::Checkboxes,
        "markdown" => IssueFormFieldKind::Markdown,
        _ => IssueFormFieldKind::Unknown,
    }
}

fn mapped_value(
    id: &str,
    kind: IssueFormFieldKind,
    item: &IssueFormItem,
    inspection: &ReportInspection,
) -> Option<String> {
    if !matches!(
        kind,
        IssueFormFieldKind::Input | IssueFormFieldKind::Textarea | IssueFormFieldKind::Dropdown
    ) {
        return None;
    }
    let value = match id {
        "summary" | "problem" | "description" => inspection.report.title.clone(),
        "kind" | "report_kind" => inspection.report.kind.as_str().to_string(),
        "digest" | "report_digest" => inspection.summary.digest.clone(),
        "version" | "aethyme_version" => render_version(&inspection.report),
        "environment" | "platform" => render_environment(&inspection.report),
        "session" | "session_details" => render_session(&inspection.report.snapshot)?,
        "task" => inspection
            .report
            .snapshot
            .session
            .as_ref()
            .and_then(|session| session.task.clone())?,
        "failure" | "last_failure" | "logs" | "logs_or_output" => {
            render_last_failure(&inspection.report.snapshot)?
        }
        "gates" | "gate_results" => render_gates(&inspection.report.snapshot)?,
        "operations" => render_operations(&inspection.report.snapshot)?,
        "recent_events" | "events" => render_events(&inspection.report.snapshot)?,
        "diagnostics" | "report" | "report_snapshot" => render_diagnostics(inspection),
        _ => return None,
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if kind == IssueFormFieldKind::Dropdown {
        return item
            .attributes
            .options
            .iter()
            .find(|option| option.label().eq_ignore_ascii_case(&value))
            .map(|option| option.label().to_string());
    }
    Some(value)
}

fn render_version(report: &ReportDocument) -> String {
    match report.snapshot.build.commit.as_deref() {
        Some(commit) => format!("{} ({commit})", report.snapshot.build.version),
        None => report.snapshot.build.version.clone(),
    }
}

fn render_environment(report: &ReportDocument) -> String {
    format!(
        "- Aethyme: {}\n- OS: {}\n- Architecture: {}",
        render_version(report),
        report.snapshot.platform.os,
        report.snapshot.platform.arch
    )
}

fn render_session(snapshot: &ReportSnapshot) -> Option<String> {
    let session = snapshot.session.as_ref()?;
    let mut value = format!(
        "- ID: {}\n- Branch: `{}`\n- Origin: {}\n- Status: {}",
        session.id,
        session.branch,
        session.origin.as_str(),
        session.status.as_str()
    );
    if let Some(diff_base) = session.diff_base.as_deref() {
        write!(value, "\n- Diff base: `{diff_base}`").expect("writing to String cannot fail");
    }
    Some(value)
}

fn render_last_failure(snapshot: &ReportSnapshot) -> Option<String> {
    let failure = snapshot.last_known_failure.as_ref()?;
    Some(match failure {
        ReportLastFailure::Session {
            session_id,
            recorded_at,
            exit_code,
        } => format!("Session {session_id} exited with code {exit_code} at {recorded_at}."),
        ReportLastFailure::Operation {
            operation_id,
            recorded_at,
            status,
            exit_code,
        } => format!(
            "Operation {operation_id} was {} at {recorded_at} (exit code {}).",
            status.as_str(),
            exit_code.map_or_else(|| "unknown".into(), |value| value.to_string())
        ),
        ReportLastFailure::Gate {
            gate,
            tree_hash,
            recorded_at,
            status,
            failure_class,
            exit_code,
            cache_source,
        } => format!(
            "Gate `{gate}` was {} for tree `{tree_hash}` at {recorded_at} (class {}, exit code {}, source {}).",
            status.as_str(),
            failure_class.map_or("unknown", |value| value.as_str()),
            exit_code.map_or_else(|| "unknown".into(), |value| value.to_string()),
            match cache_source {
                crate::ReportGateCacheSource::Executed => "executed",
                crate::ReportGateCacheSource::CacheHit => "cache_hit",
            }
        ),
    })
}

fn render_gates(snapshot: &ReportSnapshot) -> Option<String> {
    (!snapshot.gates.is_empty()).then(|| {
        snapshot
            .gates
            .iter()
            .map(|gate| {
                format!(
                    "- `{}`: {} on `{}` ({})",
                    gate.gate,
                    gate.status.as_str(),
                    gate.tree_hash,
                    match gate.cache_source {
                        crate::ReportGateCacheSource::Executed => "executed",
                        crate::ReportGateCacheSource::CacheHit => "cache hit",
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn render_operations(snapshot: &ReportSnapshot) -> Option<String> {
    (!snapshot.operations.is_empty()).then(|| {
        snapshot
            .operations
            .iter()
            .map(|operation| {
                format!(
                    "- Operation {}: {} {} on `{}` — {}",
                    operation.id,
                    operation.provider.as_str(),
                    operation.effect.as_str(),
                    operation.repository,
                    operation.status.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn render_events(snapshot: &ReportSnapshot) -> Option<String> {
    (!snapshot.recent_event_types.is_empty()).then(|| {
        snapshot
            .recent_event_types
            .iter()
            .map(|event| format!("- `{}` at {}", event.kind, event.recorded_at))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn render_diagnostics(inspection: &ReportInspection) -> String {
    let mut value = format!(
        "- Report kind: {}\n- Report digest: `{}`\n{}",
        inspection.report.kind.as_str(),
        inspection.summary.digest,
        render_environment(&inspection.report)
    );
    if let Some(session) = render_session(&inspection.report.snapshot) {
        write!(value, "\n- Session:\n{}", indent_markdown(&session, "  "))
            .expect("writing to String cannot fail");
    }
    if let Some(failure) = render_last_failure(&inspection.report.snapshot) {
        write!(value, "\n- Last known failure: {failure}").expect("writing to String cannot fail");
    }
    value
}

fn append_static_markdown(markdown: &mut String, value: &str) {
    markdown.push_str(value.trim());
    markdown.push_str("\n\n");
}

fn append_answer(markdown: &mut String, value: &str, render: Option<&str>) {
    if let Some(language) = render.map(str::trim).filter(|value| !value.is_empty()) {
        let fence = code_fence(value);
        writeln!(markdown, "{fence}{language}\n{value}\n{fence}\n")
            .expect("writing to String cannot fail");
    } else {
        markdown.push_str(value);
        markdown.push_str("\n\n");
    }
}

fn append_unfilled(markdown: &mut String, id: &str, kind: IssueFormFieldKind) {
    let id = if id.is_empty() { "<missing id>" } else { id };
    writeln!(
        markdown,
        "> Unfilled: no allowlisted report value maps to `{id}` ({kind:?}).\n"
    )
    .expect("writing to String cannot fail");
}

fn code_fence(value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

fn indent_markdown(value: &str, prefix: &str) -> String {
    value
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_accepts_only_yml_files_in_the_issue_template_directory() {
        assert_eq!(
            issue_form_filename(Path::new("bug.yml")).unwrap(),
            "bug.yml"
        );
        assert_eq!(
            issue_form_filename(Path::new(".github/ISSUE_TEMPLATE/bug.yml")).unwrap(),
            "bug.yml"
        );
        for path in [
            "../bug.yml",
            "/tmp/bug.yml",
            "nested/bug.yml",
            "bug.yaml",
            "config.yml/extra",
        ] {
            assert!(
                issue_form_filename(Path::new(path)).is_err(),
                "accepted {path}"
            );
        }
    }

    #[test]
    fn code_fence_is_longer_than_any_fence_in_the_value() {
        assert_eq!(code_fence("plain"), "```");
        assert_eq!(code_fence("before ``` after"), "````");
    }
}
