//! Digest-confirmed filing of reviewed issue-form renders through the durable
//! coordinated GitHub operation layer.

use std::io::Write;
use std::path::{Component, Path};

use serde_json::json;

use crate::issue_form::{ISSUE_REVIEW_ARTIFACT_MARKER, IssueReviewArtifactMetadata};
use crate::report::{
    ReportFilingIndex, ensure_safe_reports_directory, load_filing_index, sha256_hex,
};
use crate::{
    Broker, BrokerOpError, CoordinatedCommand, IssueFormRenderResult, IssueFormRenderedField,
    OperationEffect, OperationProvider, OperationStatus, REPORT_FILINGS_FILENAME, REPORT_MAX_BYTES,
};

pub const REPORT_FILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFileState {
    Filed,
    ReconciliationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportFileResult {
    pub schema_version: u32,
    pub path: String,
    /// SHA-256 of the exact reviewed F4 artifact confirmed for this command.
    pub digest: String,
    /// SHA-256 of the original allowlist-only captured report.
    pub report_digest: String,
    pub repository: String,
    pub operation_id: i64,
    pub operation_status: OperationStatus,
    pub state: ReportFileState,
    pub issue_number: Option<u64>,
    pub issue_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReportFileError {
    #[error(
        "invalid report digest confirmation {0:?}; expected 64 lowercase hexadecimal characters"
    )]
    InvalidConfirmation(String),
    #[error(
        "reviewed report changed after confirmation: expected {expected}, current digest is {actual}"
    )]
    DigestMismatch { expected: String, actual: String },
    #[error(
        "invalid reviewed report path {0:?}; use an .issue.md/.issue.json filename or .aethyme/reports/<filename>.issue.md"
    )]
    InvalidPath(String),
    #[error("reviewed report not found: {0}")]
    NotFound(String),
    #[error("invalid reviewed report {path}: {reason}")]
    InvalidArtifact { path: String, reason: String },
    #[error("reviewed report still has required unfilled fields: {0}")]
    RequiredFieldsUnfilled(String),
    #[error("reviewed report digest {digest} is already filed")]
    AlreadyFiled { digest: String },
    #[error(
        "source report digest {digest} already has completed filing operation {operation_id} ({status}); do not create another issue"
    )]
    PreviouslyCompleted {
        digest: String,
        operation_id: i64,
        status: &'static str,
    },
    #[error(transparent)]
    Capture(#[from] crate::ReportCaptureError),
    #[error(transparent)]
    Operation(#[from] BrokerOpError),
    #[error(transparent)]
    Store(#[from] crate::BrokerError),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
struct IssueIdentity {
    number: u64,
    url: String,
}

struct ReviewedIssueArtifact {
    schema_version: u32,
    issue_title: String,
    report_digest: String,
    form_path: String,
    fields: Vec<IssueFormRenderedField>,
    markdown: String,
}

/// File one reviewed F4 render. The confirmed digest covers the exact
/// title, Markdown body, form provenance, and report provenance being filed.
pub fn file_reviewed_report(
    broker: &mut Broker,
    session_id: i64,
    requested: &Path,
    repository: &str,
    confirmed_digest: &str,
) -> Result<ReportFileResult, ReportFileError> {
    validate_confirmation(confirmed_digest)?;
    let reports_root = broker.main_root().join(".aethyme/reports");
    ensure_safe_reports_directory(&reports_root)?;
    let (path, relative) = reviewed_report_path(&reports_root, requested)?;
    let bytes = read_reviewed_report(&path, &relative)?;
    let digest = sha256_hex(&bytes);
    if digest != confirmed_digest {
        return Err(ReportFileError::DigestMismatch {
            expected: confirmed_digest.into(),
            actual: digest,
        });
    }
    let artifact = parse_reviewed_report(&bytes, &relative)?;
    validate_reviewed_report(&artifact)?;

    let index = load_filing_index(&reports_root)?;
    if index.filings.contains_key(&artifact.report_digest) {
        return Err(ReportFileError::AlreadyFiled {
            digest: artifact.report_digest,
        });
    }
    let filing_scope = format!("report:{}", artifact.report_digest);
    if let Some(operation) = broker
        .store()
        .coordinated_operations()?
        .into_iter()
        .rev()
        .find(|operation| operation.repository == repository && operation.scope == filing_scope)
        .filter(|operation| {
            matches!(
                operation.status,
                OperationStatus::Succeeded | OperationStatus::ReconciledSucceeded
            )
        })
    {
        return Err(ReportFileError::PreviouslyCompleted {
            digest: artifact.report_digest,
            operation_id: operation.id,
            status: operation.status.as_str(),
        });
    }

    let mut body_file = tempfile::Builder::new()
        .prefix(".report-file-")
        .tempfile_in(&reports_root)
        .map_err(|source| ReportFileError::Io {
            action: "create temporary issue body",
            path: ".aethyme/reports".into(),
            source,
        })?;
    body_file
        .write_all(artifact.markdown.as_bytes())
        .and_then(|()| body_file.flush())
        .and_then(|()| body_file.as_file().sync_all())
        .map_err(|source| ReportFileError::Io {
            action: "write temporary issue body",
            path: ".aethyme/reports/<temporary>".into(),
            source,
        })?;

    let session = broker.store().session(session_id)?;
    let request = CoordinatedCommand {
        session_id,
        provider: OperationProvider::Github,
        repository: Some(repository.into()),
        resolved_target: None,
        scope: Some(filing_scope),
        declared_effect: Some(OperationEffect::Write),
        destructive_confirmed: false,
        authorization_reason: Some(format!(
            "file reviewed report confirmed by SHA-256 {digest}"
        )),
        args: vec![
            "issue".into(),
            "create".into(),
            "--title".into(),
            artifact.issue_title.clone(),
            "--body-file".into(),
            body_file.path().to_string_lossy().into_owned(),
        ],
    };

    let mut filed_identity = None;
    let operation_report = broker.run_coordinated_operation_at_with_hooks(
        request,
        Path::new(&session.worktree_path),
        || {
            let index = load_filing_index(&reports_root).map_err(|error| error.to_string())?;
            if index.filings.contains_key(&artifact.report_digest) {
                return Err(format!(
                    "reviewed report digest {} is already filed",
                    artifact.report_digest
                ));
            }
            Ok(())
        },
        |stdout, operation_id| {
            let identity = parse_issue_identity(stdout, repository)?;
            persist_filing_record(
                &reports_root,
                &artifact.report_digest,
                &digest,
                repository,
                operation_id,
                &identity,
            )
            .map_err(|error| error.to_string())?;
            filed_identity = Some(identity.clone());
            Ok(Some(json!({
                "kind": "github_issue",
                "report_digest": artifact.report_digest,
                "reviewed_digest": digest,
                "issue_number": identity.number,
                "issue_url": identity.url,
            })))
        },
    )?;

    if operation_report.operation.status == OperationStatus::Succeeded {
        let identity = filed_identity.ok_or_else(|| ReportFileError::InvalidArtifact {
            path: relative.clone(),
            reason: "coordinated success did not return an issue identity".into(),
        })?;
        Ok(ReportFileResult {
            schema_version: REPORT_FILE_SCHEMA_VERSION,
            path: relative,
            digest,
            report_digest: artifact.report_digest,
            repository: repository.into(),
            operation_id: operation_report.operation.id,
            operation_status: operation_report.operation.status,
            state: ReportFileState::Filed,
            issue_number: Some(identity.number),
            issue_url: Some(identity.url),
        })
    } else {
        Ok(ReportFileResult {
            schema_version: REPORT_FILE_SCHEMA_VERSION,
            path: relative,
            digest,
            report_digest: artifact.report_digest,
            repository: repository.into(),
            operation_id: operation_report.operation.id,
            operation_status: operation_report.operation.status,
            state: ReportFileState::ReconciliationRequired,
            issue_number: None,
            issue_url: None,
        })
    }
}

fn validate_confirmation(value: &str) -> Result<(), ReportFileError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ReportFileError::InvalidConfirmation(value.into()));
    }
    Ok(())
}

fn reviewed_report_path(
    reports_root: &Path,
    requested: &Path,
) -> Result<(std::path::PathBuf, String), ReportFileError> {
    let components = requested.components().collect::<Vec<_>>();
    let filename = match components.as_slice() {
        [Component::Normal(filename)] => filename.to_str(),
        [
            Component::Normal(aethyme),
            Component::Normal(reports),
            Component::Normal(filename),
        ] if *aethyme == ".aethyme" && *reports == "reports" => filename.to_str(),
        _ => None,
    }
    .filter(|name| {
        (name.ends_with(".issue.md") || name.ends_with(".issue.json"))
            && !name.starts_with(".report-")
    })
    .ok_or_else(|| ReportFileError::InvalidPath(requested.display().to_string()))?;
    let relative = format!(".aethyme/reports/{filename}");
    Ok((reports_root.join(filename), relative))
}

fn read_reviewed_report(path: &Path, relative: &str) -> Result<Vec<u8>, ReportFileError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ReportFileError::NotFound(relative.into())
        } else {
            ReportFileError::Io {
                action: "inspect reviewed report",
                path: relative.into(),
                source,
            }
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReportFileError::InvalidArtifact {
            path: relative.into(),
            reason: "reviewed report must be a regular file".into(),
        });
    }
    if metadata.len() > REPORT_MAX_BYTES {
        return Err(ReportFileError::InvalidArtifact {
            path: relative.into(),
            reason: format!("artifact exceeds {REPORT_MAX_BYTES} bytes"),
        });
    }
    std::fs::read(path).map_err(|source| ReportFileError::Io {
        action: "read reviewed report",
        path: relative.into(),
        source,
    })
}

fn parse_reviewed_report(
    bytes: &[u8],
    relative: &str,
) -> Result<ReviewedIssueArtifact, ReportFileError> {
    if relative.ends_with(".issue.json") {
        let rendered = serde_json::from_slice::<IssueFormRenderResult>(bytes).map_err(|_| {
            ReportFileError::InvalidArtifact {
                path: relative.into(),
                reason: "expected an F4 issue-form render produced with `report render --json`"
                    .into(),
            }
        })?;
        return Ok(ReviewedIssueArtifact {
            schema_version: rendered.schema_version,
            issue_title: rendered.issue_title,
            report_digest: rendered.report_digest,
            form_path: rendered.form_path,
            fields: rendered.fields,
            markdown: rendered.markdown,
        });
    }

    let source = std::str::from_utf8(bytes).map_err(|_| ReportFileError::InvalidArtifact {
        path: relative.into(),
        reason: "reviewed Markdown report is not UTF-8".into(),
    })?;
    let prefix = format!("<!-- {ISSUE_REVIEW_ARTIFACT_MARKER}\n");
    let (metadata, markdown) = source
        .strip_prefix(&prefix)
        .and_then(|remainder| remainder.split_once("\n-->\n"))
        .ok_or_else(|| ReportFileError::InvalidArtifact {
            path: relative.into(),
            reason: "missing reviewed-report metadata envelope".into(),
        })?;
    let metadata = serde_json::from_str::<IssueReviewArtifactMetadata>(metadata).map_err(|_| {
        ReportFileError::InvalidArtifact {
            path: relative.into(),
            reason: "invalid reviewed-report metadata envelope".into(),
        }
    })?;
    Ok(ReviewedIssueArtifact {
        schema_version: metadata.schema_version,
        issue_title: metadata.issue_title,
        report_digest: metadata.report_digest,
        form_path: metadata.form_path,
        fields: metadata.fields,
        markdown: markdown.into(),
    })
}

fn validate_reviewed_report(artifact: &ReviewedIssueArtifact) -> Result<(), ReportFileError> {
    if artifact.schema_version != crate::ISSUE_REVIEW_ARTIFACT_SCHEMA_VERSION
        && artifact.schema_version != crate::ISSUE_FORM_RENDER_SCHEMA_VERSION
    {
        return Err(ReportFileError::InvalidArtifact {
            path: artifact.form_path.clone(),
            reason: format!(
                "unsupported render schema {}; expected {}",
                artifact.schema_version,
                crate::ISSUE_FORM_RENDER_SCHEMA_VERSION
            ),
        });
    }
    validate_confirmation(&artifact.report_digest)?;
    let title = artifact.issue_title.trim();
    if title.is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
        return Err(ReportFileError::InvalidArtifact {
            path: artifact.form_path.clone(),
            reason: "issue title must be 1-256 characters on one line".into(),
        });
    }
    if artifact.markdown.trim().is_empty()
        || artifact
            .markdown
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ReportFileError::InvalidArtifact {
            path: artifact.form_path.clone(),
            reason: "issue body must be non-empty text without unsupported control characters"
                .into(),
        });
    }
    let missing = artifact
        .fields
        .iter()
        .filter(|field| field.required)
        .filter_map(|field| {
            let label = field.label.as_deref()?;
            let answer = markdown_section(&artifact.markdown, label);
            let still_unfilled = answer.is_none_or(|answer| {
                answer.trim().is_empty()
                    || answer
                        .lines()
                        .any(|line| line.trim_start().starts_with("> Unfilled:"))
            });
            still_unfilled.then(|| field.id.clone().unwrap_or_else(|| label.into()))
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ReportFileError::RequiredFieldsUnfilled(missing.join(", ")));
    }
    Ok(())
}

fn markdown_section<'a>(markdown: &'a str, label: &str) -> Option<&'a str> {
    let heading = format!("## {label}");
    let mut cursor = 0;
    let mut content_start = None;
    for segment in markdown.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        if let Some(start) = content_start
            && line.starts_with("## ")
        {
            return Some(&markdown[start..cursor]);
        }
        if line == heading {
            content_start = Some(cursor + segment.len());
        }
        cursor += segment.len();
    }
    content_start.map(|start| &markdown[start..])
}

fn parse_issue_identity(stdout: &[u8], repository: &str) -> Result<IssueIdentity, String> {
    let output = std::str::from_utf8(stdout)
        .map_err(|_| "GitHub issue creation returned non-UTF-8 output".to_string())?;
    let url = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "GitHub issue creation returned no issue URL".to_string())?;
    if !url.starts_with("https://") || url.chars().any(char::is_whitespace) {
        return Err("GitHub issue creation returned an invalid issue URL".into());
    }
    let path = url
        .split_once("://")
        .map(|(_, remainder)| remainder)
        .and_then(|remainder| remainder.split_once('/').map(|(_, path)| path))
        .ok_or_else(|| "GitHub issue creation returned an invalid issue URL".to_string())?;
    let segments = path.trim_end_matches('/').split('/').collect::<Vec<_>>();
    let repository_parts = repository.split('/').collect::<Vec<_>>();
    if segments.len() < 4
        || segments[segments.len() - 4] != repository_parts[0]
        || segments[segments.len() - 3] != repository_parts[1]
        || segments[segments.len() - 2] != "issues"
    {
        return Err("GitHub issue URL does not match the requested repository".into());
    }
    let number = segments[segments.len() - 1]
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| "GitHub issue URL does not contain a valid issue number".to_string())?;
    Ok(IssueIdentity {
        number,
        url: url.into(),
    })
}

fn persist_filing_record(
    reports_root: &Path,
    report_digest: &str,
    reviewed_digest: &str,
    repository: &str,
    operation_id: i64,
    identity: &IssueIdentity,
) -> Result<(), ReportFileError> {
    let mut index = load_filing_index(reports_root)?;
    if index.filings.contains_key(report_digest) {
        return Err(ReportFileError::AlreadyFiled {
            digest: report_digest.into(),
        });
    }
    index.filings.insert(
        report_digest.into(),
        json!({
            "repository": repository,
            "operation_id": operation_id,
            "reviewed_digest": reviewed_digest,
            "issue_number": identity.number,
            "issue_url": identity.url,
        }),
    );
    write_filing_index_atomic(reports_root, &index)
}

fn write_filing_index_atomic(
    reports_root: &Path,
    index: &ReportFilingIndex,
) -> Result<(), ReportFileError> {
    let path = reports_root.join(REPORT_FILINGS_FILENAME);
    let mut bytes =
        serde_json::to_vec_pretty(index).map_err(|error| ReportFileError::InvalidArtifact {
            path: format!(".aethyme/reports/{REPORT_FILINGS_FILENAME}"),
            reason: error.to_string(),
        })?;
    bytes.push(b'\n');
    let mut temporary = tempfile::Builder::new()
        .prefix(".report-filing-index-")
        .tempfile_in(reports_root)
        .map_err(|source| ReportFileError::Io {
            action: "create temporary filing index",
            path: ".aethyme/reports".into(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ReportFileError::Io {
            action: "write temporary filing index",
            path: ".aethyme/reports/<temporary>".into(),
            source,
        })?;
    temporary
        .persist(&path)
        .map_err(|error| ReportFileError::Io {
            action: "publish filing index",
            path: format!(".aethyme/reports/{REPORT_FILINGS_FILENAME}"),
            source: error.error,
        })?;
    std::fs::File::open(reports_root)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ReportFileError::Io {
            action: "sync report directory",
            path: ".aethyme/reports".into(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_url_parser_accepts_github_and_enterprise_hosts() {
        for url in [
            "https://github.com/owner/repo/issues/42\n",
            "https://github.example/owner/repo/issues/42\n",
        ] {
            let issue = parse_issue_identity(url.as_bytes(), "owner/repo").unwrap();
            assert_eq!(issue.number, 42);
            assert_eq!(issue.url, url.trim());
        }
    }

    #[test]
    fn issue_url_parser_rejects_mismatched_or_unidentified_results() {
        for output in [
            "",
            "created\n",
            "http://github.com/owner/repo/issues/42\n",
            "https://github.com/other/repo/issues/42\n",
            "https://github.com/owner/repo/issues/not-a-number\n",
        ] {
            assert!(parse_issue_identity(output.as_bytes(), "owner/repo").is_err());
        }
    }

    #[test]
    fn reviewed_required_sections_must_replace_unfilled_markers() {
        assert!(
            markdown_section("## Summary\n\nDone\n\n## Next\n\nValue\n", "Summary")
                .is_some_and(|section| section.contains("Done"))
        );
        assert!(markdown_section("## Other\n\nDone\n", "Summary").is_none());
    }
}
