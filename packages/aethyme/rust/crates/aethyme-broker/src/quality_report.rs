//! Provider-neutral quality-report publication for pull requests.
//!
//! A quality report is evidence produced by a repository-owned runner. The
//! broker validates its provenance, removes unsafe presentation fields, and
//! produces an idempotent publication action. It never runs a gate and never
//! turns a local report into a merge-protection signal.

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const QUALITY_REPORT_SCHEMA_VERSION: u32 = 1;
/// Marker for the quality section inside Aethyme's one owned review comment.
pub const QUALITY_REPORT_SECTION_MARKER: &str = "<!-- aethyme:quality-report -->";
pub const QUALITY_REPORT_SECTION_END_MARKER: &str = "<!-- aethyme:quality-report:end -->";
/// Kept as an alias for callers that used the initial draft name. It is not a
/// comment-ownership marker: `<!-- aethyme:review -->` remains the only owned
/// pull-request comment marker.
pub const QUALITY_REPORT_COMMENT_MARKER: &str = QUALITY_REPORT_SECTION_MARKER;
pub const QUALITY_REPORT_MAX_GATES: usize = 256;
pub const QUALITY_REPORT_MAX_BODY_BYTES: usize = 48 * 1024;
pub const QUALITY_REPORT_MAX_INPUT_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityReportSource {
    Local,
    Ci,
    Queue,
}

impl QualityReportSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ci => "ci",
            Self::Queue => "queue",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityReportStatus {
    Complete,
    Partial,
    Failed,
    Unavailable,
    Stale,
    Malformed,
}

impl QualityReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Malformed => "malformed",
        }
    }

    fn is_neutral(self) -> bool {
        !matches!(self, Self::Complete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualityReportRoute {
    FastTrack,
    Affected,
    Standard,
    FullSuite,
}

impl QualityReportRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FastTrack => "fast-track",
            Self::Affected => "affected",
            Self::Standard => "standard",
            Self::FullSuite => "full-suite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityGateStatus {
    Passed,
    Failed,
    Cached,
    Skipped,
    Cancelled,
    Error,
}

impl QualityGateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cached => "cached",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityGateCache {
    Hit,
    Miss,
    NotApplicable,
}

impl QualityGateCache {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityReportScope {
    pub files: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityReportTotals {
    pub gates: u64,
    pub passed: u64,
    pub failed: u64,
    pub cached: u64,
    pub skipped: u64,
    pub duration_ms: u64,
    pub queue_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateEvidence {
    pub id: String,
    pub status: QualityGateStatus,
    pub duration_ms: u64,
    pub cache: QualityGateCache,
    #[serde(default)]
    pub rerun: Option<String>,
    #[serde(default)]
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityReportProvenance {
    pub runner_version: String,
    pub policy_version: String,
    pub report_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityReport {
    pub schema_version: u32,
    pub repository: String,
    pub pull_request: i64,
    pub revision: String,
    pub base_revision: String,
    pub source: QualityReportSource,
    pub status: QualityReportStatus,
    pub route: QualityReportRoute,
    pub scope: QualityReportScope,
    pub totals: QualityReportTotals,
    pub gates: Vec<QualityGateEvidence>,
    pub provenance: QualityReportProvenance,
}

#[derive(Debug, thiserror::Error)]
pub enum QualityReportError {
    #[error("cannot read quality report {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("quality report JSON is invalid: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("quality report is invalid: {0}")]
    Invalid(String),
    #[error("quality report provenance does not match the pull request: {0}")]
    ProvenanceMismatch(String),
    #[error("quality report publication failed: {0}")]
    Provider(String),
}

impl QualityReport {
    pub fn from_path(path: &Path) -> Result<Self, QualityReportError> {
        let bytes = std::fs::read(path).map_err(|source| QualityReportError::Read {
            path: path.display().to_string(),
            source,
        })?;
        if bytes.len() > QUALITY_REPORT_MAX_INPUT_BYTES {
            return Err(QualityReportError::Invalid(format!(
                "report exceeds the {}-byte input bound",
                QUALITY_REPORT_MAX_INPUT_BYTES
            )));
        }
        let report: Self = serde_json::from_slice(&bytes)?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), QualityReportError> {
        if self.schema_version != QUALITY_REPORT_SCHEMA_VERSION {
            return Err(QualityReportError::Invalid(format!(
                "schema_version {} is unsupported; expected {}",
                self.schema_version, QUALITY_REPORT_SCHEMA_VERSION
            )));
        }
        validate_repository(&self.repository)?;
        if self.pull_request <= 0 {
            return Err(QualityReportError::Invalid(
                "pull_request must be positive".into(),
            ));
        }
        validate_sha("revision", &self.revision)?;
        validate_sha("base_revision", &self.base_revision)?;
        if self.gates.len() > QUALITY_REPORT_MAX_GATES {
            return Err(QualityReportError::Invalid(format!(
                "gates exceeds the {}-entry bound",
                QUALITY_REPORT_MAX_GATES
            )));
        }
        if self.scope.digest.len() != 64 || !is_hex(&self.scope.digest) {
            return Err(QualityReportError::Invalid(
                "scope.digest must be a 64-character SHA-256 digest".into(),
            ));
        }
        if self.totals.gates != self.gates.len() as u64 {
            return Err(QualityReportError::Invalid(format!(
                "totals.gates ({}) does not match gates length ({})",
                self.totals.gates,
                self.gates.len()
            )));
        }
        let passed = self
            .gates
            .iter()
            .filter(|gate| {
                matches!(
                    gate.status,
                    QualityGateStatus::Passed | QualityGateStatus::Cached
                )
            })
            .count() as u64;
        let failed = self
            .gates
            .iter()
            .filter(|gate| {
                matches!(
                    gate.status,
                    QualityGateStatus::Failed | QualityGateStatus::Error
                )
            })
            .count() as u64;
        let skipped = self
            .gates
            .iter()
            .filter(|gate| matches!(gate.status, QualityGateStatus::Skipped))
            .count() as u64;
        let cached = self
            .gates
            .iter()
            .filter(|gate| matches!(gate.cache, QualityGateCache::Hit))
            .count() as u64;
        if self.totals.passed != passed
            || self.totals.failed != failed
            || self.totals.skipped != skipped
            || self.totals.cached != cached
        {
            return Err(QualityReportError::Invalid(
                "quality report totals do not match per-gate evidence".into(),
            ));
        }
        for (index, gate) in self.gates.iter().enumerate() {
            validate_token(&format!("gates[{index}].id"), &gate.id, 128)?;
            if let Some(rerun) = &gate.rerun {
                validate_publication_text(&format!("gates[{index}].rerun"), rerun, 512)?;
            }
            if let Some(artifact) = &gate.artifact {
                validate_publication_text(&format!("gates[{index}].artifact"), artifact, 1024)?;
            }
        }
        validate_token(
            "provenance.runner_version",
            &self.provenance.runner_version,
            128,
        )?;
        validate_token(
            "provenance.policy_version",
            &self.provenance.policy_version,
            128,
        )?;
        if self.provenance.report_digest.len() != 64 || !is_hex(&self.provenance.report_digest) {
            return Err(QualityReportError::Invalid(
                "provenance.report_digest must be a 64-character SHA-256 digest".into(),
            ));
        }
        let expected = self.computed_digest();
        if expected != self.provenance.report_digest {
            return Err(QualityReportError::Invalid(
                "provenance.report_digest does not match the canonical report".into(),
            ));
        }
        Ok(())
    }

    pub fn with_computed_digest(mut self) -> Self {
        self.provenance.report_digest.clear();
        self.provenance.report_digest = self.computed_digest();
        self
    }

    pub fn computed_digest(&self) -> String {
        let mut unsigned = self.clone();
        unsigned.provenance.report_digest.clear();
        let bytes = serde_json::to_vec(&unsigned).expect("quality report is serializable");
        sha256(&bytes)
    }

    fn redacted(&self) -> Self {
        let mut redacted = self.clone();
        for gate in &mut redacted.gates {
            gate.rerun = gate.rerun.as_deref().map(redact_publication_text);
            gate.artifact = gate.artifact.as_deref().map(redact_publication_text);
        }
        redacted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualityReportComment {
    pub id: i64,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QualityReportPublicationFacts {
    pub repository: String,
    pub pull_request: i64,
    pub head_revision: String,
    pub base_revision: String,
    pub owned_comment: Option<QualityReportComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum QualityReportPublicationAction {
    CreateComment { body: String },
    UpdateComment { comment_id: i64, body: String },
}

impl QualityReportPublicationAction {
    pub fn gh_args(&self, pull_request: i64) -> Vec<String> {
        match self {
            Self::CreateComment { body } => vec![
                "pr".into(),
                "comment".into(),
                pull_request.to_string(),
                "--body".into(),
                body.clone(),
            ],
            Self::UpdateComment { comment_id, body } => vec![
                "api".into(),
                "--method".into(),
                "PATCH".into(),
                format!("repos/{{owner}}/{{repo}}/issues/comments/{comment_id}"),
                "-f".into(),
                format!("body={body}"),
            ],
        }
    }

    pub fn reason(&self, pull_request: i64, digest: &str) -> String {
        match self {
            Self::CreateComment { .. } | Self::UpdateComment { .. } => format!(
                "publish revision-bound quality report {digest} on pull request #{pull_request}"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualityReportPublicationPlan {
    pub report_digest: String,
    pub status: QualityReportStatus,
    pub action: Option<QualityReportPublicationAction>,
    pub explanation: String,
}

pub fn plan_quality_report(
    report: &QualityReport,
    facts: &QualityReportPublicationFacts,
) -> Result<QualityReportPublicationPlan, QualityReportError> {
    report.validate()?;
    if report.repository != facts.repository {
        return Err(QualityReportError::ProvenanceMismatch(format!(
            "report repository {} differs from pull request repository {}",
            report.repository, facts.repository
        )));
    }
    if report.pull_request != facts.pull_request {
        return Err(QualityReportError::ProvenanceMismatch(format!(
            "report pull request {} differs from requested pull request {}",
            report.pull_request, facts.pull_request
        )));
    }
    if report.revision != facts.head_revision {
        return Err(QualityReportError::ProvenanceMismatch(format!(
            "report revision {} differs from pull request head {}",
            report.revision, facts.head_revision
        )));
    }
    if report.base_revision != facts.base_revision {
        return Err(QualityReportError::ProvenanceMismatch(format!(
            "report base revision {} differs from pull request base {}",
            report.base_revision, facts.base_revision
        )));
    }

    let body = merge_quality_report_section(
        facts
            .owned_comment
            .as_ref()
            .map(|comment| comment.body.as_str()),
        report,
    );
    let action = match &facts.owned_comment {
        Some(comment) if comment.body.trim() == body.trim() => None,
        Some(comment) => Some(QualityReportPublicationAction::UpdateComment {
            comment_id: comment.id,
            body,
        }),
        None => Some(QualityReportPublicationAction::CreateComment { body }),
    };
    let explanation = if report.status.is_neutral() {
        "publishes a neutral quality report; it cannot certify or replace CI".into()
    } else if report.totals.failed > 0 {
        "publishes a completed report with failing gates; it is not a green verdict".into()
    } else if action.is_none() {
        "the exact report is already published; no GitHub write is needed".into()
    } else {
        "publishes the validated report as an advisory pull-request summary".into()
    };
    Ok(QualityReportPublicationPlan {
        report_digest: report.provenance.report_digest.clone(),
        status: report.status,
        action,
        explanation,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualityReportPublicationReceipt {
    pub report_digest: String,
    pub action: String,
    pub external_id: Option<String>,
}

/// Provider-neutral write seam. Tests and non-GitHub adapters can implement
/// this without shelling out. The CLI's GitHub adapter turns the same action
/// into one coordinated `gh` operation.
pub trait QualityReportPublisher {
    fn create_comment(&mut self, pull_request: i64, body: &str) -> Result<String, String>;
    fn update_comment(&mut self, comment_id: i64, body: &str) -> Result<String, String>;
}

pub fn publish_quality_report(
    plan: &QualityReportPublicationPlan,
    pull_request: i64,
    publisher: &mut dyn QualityReportPublisher,
) -> Result<QualityReportPublicationReceipt, QualityReportError> {
    let Some(action) = &plan.action else {
        return Ok(QualityReportPublicationReceipt {
            report_digest: plan.report_digest.clone(),
            action: "noop".into(),
            external_id: None,
        });
    };
    let (action_name, external_id) = match action {
        QualityReportPublicationAction::CreateComment { body } => (
            "create_comment",
            publisher
                .create_comment(pull_request, body)
                .map_err(QualityReportError::Provider)?,
        ),
        QualityReportPublicationAction::UpdateComment { comment_id, body } => (
            "update_comment",
            publisher
                .update_comment(*comment_id, body)
                .map_err(QualityReportError::Provider)?,
        ),
    };
    Ok(QualityReportPublicationReceipt {
        report_digest: plan.report_digest.clone(),
        action: action_name.into(),
        external_id: Some(external_id),
    })
}

pub fn render_quality_report(report: &QualityReport) -> String {
    let redacted = report.redacted();
    let mut out = String::new();
    let _ = writeln!(out, "{QUALITY_REPORT_SECTION_MARKER}");
    let _ = writeln!(out, "### Aethyme quality report");
    let _ = writeln!(out);
    let has_incomplete_gate = redacted.gates.iter().any(|gate| {
        matches!(
            gate.status,
            QualityGateStatus::Skipped | QualityGateStatus::Cancelled
        )
    });
    // A green banner has to stand for evidence, and a report that ran no gates
    // has none. `gates: []` with everything zero validates and reached the
    // COMPLETE arm, so an empty route published a check mark for a revision
    // where nothing was measured.
    let banner = if redacted.status == QualityReportStatus::Complete && redacted.totals.gates == 0 {
        "⚠️ NO GATE EVIDENCE — nothing was measured for this revision"
    } else if redacted.status == QualityReportStatus::Complete
        && redacted.totals.failed == 0
        && !has_incomplete_gate
    {
        "✅ COMPLETE — advisory evidence only; required CI remains authoritative"
    } else if redacted.status == QualityReportStatus::Complete && redacted.totals.failed > 0 {
        "❌ COMPLETE WITH FAILURES — advisory evidence only; required CI remains authoritative"
    } else if redacted.status == QualityReportStatus::Complete {
        "⚠️ COMPLETE WITH INCOMPLETE GATES — advisory evidence only; required CI remains authoritative"
    } else {
        "⚠️ NEUTRAL — this report is not a certification or permission to reduce checks"
    };
    let _ = writeln!(out, "**{banner}**");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Status: `{}`", redacted.status.as_str());
    let _ = writeln!(out, "- Source: `{}`", redacted.source.as_str());
    let _ = writeln!(out, "- Route: `{}`", redacted.route.as_str());
    let _ = writeln!(out, "- Head: `{}`", escape_inline(&redacted.revision));
    let _ = writeln!(out, "- Base: `{}`", escape_inline(&redacted.base_revision));
    let _ = writeln!(
        out,
        "- Scope: {} file(s), `{}`",
        redacted.scope.files,
        escape_inline(&redacted.scope.digest)
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "**Totals**");
    let _ = writeln!(
        out,
        "- Gates: {} ({} passed, {} failed, {} cached, {} skipped)",
        redacted.totals.gates,
        redacted.totals.passed,
        redacted.totals.failed,
        redacted.totals.cached,
        redacted.totals.skipped
    );
    let _ = writeln!(
        out,
        "- Duration: {} ms (queue {} ms)",
        redacted.totals.duration_ms, redacted.totals.queue_ms
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "**Gates**");
    if redacted.gates.is_empty() {
        let _ = writeln!(out, "- No gate evidence was produced.");
    } else {
        for gate in &redacted.gates {
            let _ = write!(
                out,
                "- `{}` — `{}` in {} ms (`{}`)",
                escape_inline(&gate.id),
                gate.status.as_str(),
                gate.duration_ms,
                gate.cache.as_str()
            );
            if let Some(rerun) = &gate.rerun {
                let _ = write!(out, "; rerun: `{}`", escape_inline(rerun));
            }
            if let Some(artifact) = &gate.artifact {
                let _ = write!(out, "; artifact: `{}`", escape_inline(artifact));
            }
            let _ = writeln!(out);
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "<sub>Report `{}` from runner `{}` under policy `{}`. Aethyme does not replace CI.</sub>",
        escape_inline(&redacted.provenance.report_digest),
        escape_inline(&redacted.provenance.runner_version),
        escape_inline(&redacted.provenance.policy_version)
    );
    if out.len() > QUALITY_REPORT_MAX_BODY_BYTES {
        let suffix = format!(
            "\n\n[report truncated by broker; inspect the source artifact]\n{QUALITY_REPORT_SECTION_END_MARKER}\n"
        );
        let limit = QUALITY_REPORT_MAX_BODY_BYTES.saturating_sub(suffix.len());
        let mut end = limit.min(out.len());
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str(&suffix);
    } else {
        let _ = writeln!(out, "{QUALITY_REPORT_SECTION_END_MARKER}");
    }
    out
}

/// Add or replace the quality section in the one owned review comment.
pub fn merge_quality_report_section(
    existing_comment: Option<&str>,
    report: &QualityReport,
) -> String {
    let rendered = render_quality_report(report);
    let base = existing_comment.unwrap_or({
        // A report may arrive before the first review projection. Still create
        // the same single owned comment, so the next review sweep can edit it
        // in place rather than creating a second quality-only comment.
        "<!-- aethyme:review -->\n### Aethyme review\n"
    });
    replace_quality_report_section(base, &rendered)
}

/// Preserve a previously published quality section while a review projection
/// refreshes the rest of the owned comment.
pub fn preserve_quality_report_section(new_comment: &str, existing_comment: &str) -> String {
    let Some(section) = quality_report_section(existing_comment) else {
        return new_comment.to_string();
    };
    replace_quality_report_section(new_comment, section)
}

fn replace_quality_report_section(base: &str, section: &str) -> String {
    if let Some((start, end)) = quality_report_section_range(base) {
        let mut merged = String::with_capacity(base.len() + section.len());
        merged.push_str(&base[..start]);
        merged.push_str(section);
        merged.push_str(&base[end..]);
        return merged;
    }
    let mut merged = base.trim_end().to_string();
    merged.push_str("\n\n");
    merged.push_str(section.trim_start());
    if !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
}

fn quality_report_section(body: &str) -> Option<&str> {
    let (start, end) = quality_report_section_range(body)?;
    Some(&body[start..end])
}

fn quality_report_section_range(body: &str) -> Option<(usize, usize)> {
    let start = body.find(QUALITY_REPORT_SECTION_MARKER)?;
    let end_marker = body[start..].find(QUALITY_REPORT_SECTION_END_MARKER)?;
    let end = start + end_marker + QUALITY_REPORT_SECTION_END_MARKER.len();
    Some((start, end))
}

fn validate_repository(repository: &str) -> Result<(), QualityReportError> {
    if repository.is_empty()
        || repository.len() > 256
        || repository.matches('/').count() != 1
        || repository.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || !part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        })
    {
        return Err(QualityReportError::Invalid(
            "repository must be an owner/name identifier".into(),
        ));
    }
    Ok(())
}

fn validate_sha(field: &str, value: &str) -> Result<(), QualityReportError> {
    if !(value.len() == 40 || value.len() == 64) || !is_hex(value) {
        return Err(QualityReportError::Invalid(format!(
            "{field} must be a full hexadecimal commit SHA"
        )));
    }
    Ok(())
}

fn validate_token(field: &str, value: &str, max: usize) -> Result<(), QualityReportError> {
    if value.is_empty()
        || value.len() > max
        || value.chars().any(|character| character.is_control())
    {
        return Err(QualityReportError::Invalid(format!(
            "{field} must be non-empty, bounded, and free of control characters"
        )));
    }
    Ok(())
}

fn validate_publication_text(
    field: &str,
    value: &str,
    max: usize,
) -> Result<(), QualityReportError> {
    validate_token(field, value, max)?;
    if value.contains('\n') || value.contains('\r') {
        return Err(QualityReportError::Invalid(format!(
            "{field} must be a single-line publication reference"
        )));
    }
    Ok(())
}

fn is_hex(value: &str) -> bool {
    value.chars().all(|character| character.is_ascii_hexdigit())
}

fn redact_publication_text(value: &str) -> String {
    let mut redacted = value.to_string();
    let mut earliest: Option<usize> = None;
    for marker in [
        "ghp_",
        "github_pat_",
        "sk-",
        "Bearer ",
        "TOKEN=",
        "token=",
        "SECRET=",
        "secret=",
        "PASSWORD=",
        "password=",
        "API_KEY=",
        "api_key=",
    ] {
        if let Some(index) = redacted.find(marker) {
            earliest = Some(earliest.map_or(index, |seen: usize| seen.min(index)));
        }
    }
    // The cut must be at the earliest marker in the *text*, not at whichever
    // marker happens to come first in this list. Breaking on the first list hit
    // truncated after anything that appeared before it: `PASSWORD=hunter2
    // TOKEN=ghp_x` matched `ghp_` first and published the password verbatim
    // into a public comment.
    if let Some(index) = earliest {
        redacted.truncate(index);
        redacted.push_str("[redacted]");
    }
    if [
        "/Users/",
        "/home/",
        "/tmp/",
        "/private/tmp/",
        "/var/folders/",
        "C:\\",
        "C:/",
        "\\\\Users\\",
    ]
    .iter()
    .any(|marker| redacted.contains(marker))
    {
        return "[redacted-path]".into();
    }
    redacted
}

fn escape_inline(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {

    /// Redaction must cut at the earliest secret in the text, not at whichever
    /// marker is listed first. Ordering the cut by the marker list published
    /// everything that appeared before the matched one.
    #[test]
    fn redaction_cuts_at_the_first_secret_in_the_text() {
        let redacted = redact_publication_text("make ci PASSWORD=hunter2 TOKEN=ghp_abc");
        assert!(
            !redacted.contains("hunter2"),
            "a secret before the matched marker was published: {redacted}"
        );
        assert!(redacted.starts_with("make ci "));
        assert!(redacted.ends_with("[redacted]"));

        // And in the other order, so the test cannot pass by luck of the list.
        let redacted = redact_publication_text("run TOKEN=ghp_abc PASSWORD=hunter2");
        assert!(!redacted.contains("ghp_abc"), "got {redacted}");
        assert!(!redacted.contains("hunter2"), "got {redacted}");
    }
    use super::*;

    fn report() -> QualityReport {
        QualityReport {
            schema_version: QUALITY_REPORT_SCHEMA_VERSION,
            repository: "owner/repo".into(),
            pull_request: 7,
            revision: "a".repeat(40),
            base_revision: "b".repeat(40),
            source: QualityReportSource::Local,
            status: QualityReportStatus::Complete,
            route: QualityReportRoute::Affected,
            scope: QualityReportScope {
                files: 2,
                digest: "c".repeat(64),
            },
            totals: QualityReportTotals {
                gates: 2,
                passed: 1,
                failed: 0,
                cached: 1,
                skipped: 1,
                duration_ms: 42,
                queue_ms: 3,
            },
            gates: vec![
                QualityGateEvidence {
                    id: "lint".into(),
                    status: QualityGateStatus::Passed,
                    duration_ms: 20,
                    cache: QualityGateCache::Miss,
                    rerun: Some("cargo test".into()),
                    artifact: Some("logs/lint.txt".into()),
                },
                QualityGateEvidence {
                    id: "unit".into(),
                    status: QualityGateStatus::Skipped,
                    duration_ms: 0,
                    cache: QualityGateCache::Hit,
                    rerun: Some("/Users/secret/project/run --token=ghp_secret".into()),
                    artifact: Some("/tmp/private.log".into()),
                },
            ],
            provenance: QualityReportProvenance {
                runner_version: "runner-1".into(),
                policy_version: "policy-1".into(),
                report_digest: String::new(),
            },
        }
        .with_computed_digest()
    }

    #[test]
    fn validates_digest_totals_and_renders_idempotently() {
        let report = report();
        report.validate().unwrap();
        assert_eq!(
            render_quality_report(&report),
            render_quality_report(&report)
        );
        assert!(render_quality_report(&report).contains("quality-report"));
    }

    #[test]
    fn publication_is_bound_and_idempotent() {
        let report = report();
        let facts = QualityReportPublicationFacts {
            repository: "owner/repo".into(),
            pull_request: 7,
            head_revision: "a".repeat(40),
            base_revision: "b".repeat(40),
            ..Default::default()
        };
        let plan = plan_quality_report(&report, &facts).unwrap();
        assert!(matches!(
            plan.action,
            Some(QualityReportPublicationAction::CreateComment { .. })
        ));
        let body = match plan.action.as_ref().unwrap() {
            QualityReportPublicationAction::CreateComment { body } => body.clone(),
            _ => unreachable!(),
        };
        let same = plan_quality_report(
            &report,
            &QualityReportPublicationFacts {
                owned_comment: Some(QualityReportComment { id: 3, body }),
                ..facts.clone()
            },
        )
        .unwrap();
        assert!(same.action.is_none());
    }

    #[test]
    fn publication_updates_the_single_owned_review_comment() {
        let report = report();
        let existing_body = format!(
            "{}\n### Aethyme review\n\n- ✓ **security** — satisfied\n",
            crate::COMMENT_MARKER
        );
        let plan = plan_quality_report(
            &report,
            &QualityReportPublicationFacts {
                repository: "owner/repo".into(),
                pull_request: 7,
                head_revision: "a".repeat(40),
                base_revision: "b".repeat(40),
                owned_comment: Some(QualityReportComment {
                    id: 11,
                    body: existing_body,
                }),
            },
        )
        .unwrap();
        let Some(QualityReportPublicationAction::UpdateComment { body, .. }) = plan.action else {
            panic!("quality evidence must edit the existing owned review comment");
        };
        assert_eq!(body.matches(crate::COMMENT_MARKER).count(), 1);
        assert_eq!(body.matches(QUALITY_REPORT_SECTION_MARKER).count(), 1);
        assert!(body.contains("security"));
    }

    #[test]
    fn stale_and_partial_reports_are_visibly_neutral_and_redacted() {
        let mut report = report();
        report.status = QualityReportStatus::Stale;
        report.gates[0].rerun = Some("/Users/christophe/token=ghp_secret".into());
        report.provenance.report_digest.clear();
        report = report.with_computed_digest();
        let body = render_quality_report(&report);
        assert!(body.contains("NEUTRAL"));
        assert!(!body.contains("/Users/christophe"));
        assert!(!body.contains("ghp_secret"));
        assert!(!body.contains("token="));
    }

    struct FakePublisher {
        calls: Vec<String>,
    }

    impl QualityReportPublisher for FakePublisher {
        fn create_comment(&mut self, _pull_request: i64, _body: &str) -> Result<String, String> {
            self.calls.push("create".into());
            Ok("comment-1".into())
        }

        fn update_comment(&mut self, _comment_id: i64, _body: &str) -> Result<String, String> {
            self.calls.push("update".into());
            Ok("comment-1".into())
        }
    }

    #[test]
    fn fake_publisher_contract_calls_one_idempotent_action() {
        let report = report();
        let facts = QualityReportPublicationFacts {
            repository: "owner/repo".into(),
            pull_request: 7,
            head_revision: "a".repeat(40),
            base_revision: "b".repeat(40),
            ..Default::default()
        };
        let plan = plan_quality_report(&report, &facts).unwrap();
        let mut publisher = FakePublisher { calls: Vec::new() };
        let receipt = publish_quality_report(&plan, 7, &mut publisher).unwrap();
        assert_eq!(receipt.action, "create_comment");
        assert_eq!(publisher.calls, vec!["create"]);
    }

    #[test]
    fn fake_publisher_contract_updates_the_owned_comment_in_place() {
        let report = report();
        let plan = plan_quality_report(
            &report,
            &QualityReportPublicationFacts {
                repository: "owner/repo".into(),
                pull_request: 7,
                head_revision: "a".repeat(40),
                base_revision: "b".repeat(40),
                owned_comment: Some(QualityReportComment {
                    id: 3,
                    body: format!("{}\nold", crate::COMMENT_MARKER),
                }),
            },
        )
        .unwrap();
        let mut publisher = FakePublisher { calls: Vec::new() };
        let receipt = publish_quality_report(&plan, 7, &mut publisher).unwrap();
        assert_eq!(receipt.action, "update_comment");
        assert_eq!(publisher.calls, vec!["update"]);
    }
}
