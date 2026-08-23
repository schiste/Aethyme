//! Pure, allowlist-only inputs and outputs for local broker reports.
//!
//! The builder deliberately accepts rich broker rows but constructs a new
//! report schema field by field. Sensitive source fields therefore cannot
//! enter a snapshot through generic serialization or JSON pass-through.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::Broker;
use crate::gates::GateRunOutcome;
use crate::types::{
    CoordinatedOperation, Event, GateFailureClass, GateStatus, OperationEffect, OperationProvider,
    OperationStatus, Session, SessionOrigin, SessionStatus,
};
use crate::version::BinaryBuild;

pub const REPORT_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const REPORT_DOCUMENT_SCHEMA_VERSION: u32 = 1;
pub const REPORT_INVENTORY_SCHEMA_VERSION: u32 = 1;
pub const REPORT_FILINGS_SCHEMA_VERSION: u32 = 1;
pub const REPORT_FILINGS_FILENAME: &str = ".filings.json";
pub const REPORT_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const REPORT_RECENT_EVENT_LIMIT: usize = 20;
pub const REPORT_RECENT_OPERATION_LIMIT: usize = 20;
pub const REPORT_RECENT_GATE_LIMIT: usize = 20;

/// User-facing report category. The spelling is part of the capture and
/// future filing contract, so it is deliberately narrower than free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    Bug,
    Improvement,
}

impl ReportKind {
    pub fn parse(value: &str) -> Result<Self, ReportCaptureError> {
        match value {
            "bug" => Ok(Self::Bug),
            "improvement" => Ok(Self::Improvement),
            other => Err(ReportCaptureError::InvalidKind(other.to_string())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Improvement => "improvement",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReportCaptureError {
    #[error("invalid report kind {0:?}; expected bug or improvement")]
    InvalidKind(String),
    #[error("invalid report title: {0}")]
    InvalidTitle(String),
    #[error(
        "invalid report output {0:?}; use a filename or .aethyme/reports/<filename> without subdirectories"
    )]
    InvalidOutput(String),
    #[error("report destination already exists: {0}")]
    DestinationExists(String),
    #[error("captured report not found: {0}")]
    ReportNotFound(String),
    #[error("invalid captured report {path}: {reason}")]
    InvalidReport { path: String, reason: String },
    #[error("report directory must not be a symbolic link: {0}")]
    SymlinkedReportDirectory(String),
    #[error("report serialization: {0}")]
    Serialize(#[from] serde_json::Error),
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

/// Complete, reviewable offline artifact. The title and kind are explicit
/// user input; all diagnostic data comes through F1's allowlist snapshot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportDocument {
    pub schema_version: u32,
    pub kind: ReportKind,
    pub title: String,
    /// Unix epoch milliseconds supplied by the capture boundary.
    pub captured_at: i64,
    pub snapshot: ReportSnapshot,
}

/// A finalized report byte stream. All output modes consume these exact
/// bytes, preventing serialization drift between stdout, disk, and digest.
#[derive(Debug, Clone)]
pub struct PreparedReport {
    pub document: ReportDocument,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub suggested_filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportCaptureResult {
    /// Repository-relative path. `None` means the report was emitted only
    /// to stdout.
    pub path: Option<String>,
    pub sha256: String,
    pub bytes: usize,
}

/// Stable filed/unfiled state exposed by report inventory commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFilingState {
    Unfiled,
    Filed,
}

/// Stable v1 summary shared by `report list --json` and `report show --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportSummary {
    pub path: String,
    pub title: String,
    pub captured_at: i64,
    pub kind: ReportKind,
    /// Aethyme binary version recorded in the report snapshot.
    pub version: String,
    pub report_schema_version: u32,
    /// Lowercase SHA-256 of the exact current artifact bytes.
    pub digest: String,
    pub filing_state: ReportFilingState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InvalidReportEntry {
    pub path: String,
    pub error: String,
}

/// Stable v1 JSON output of `report list`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportList {
    pub schema_version: u32,
    pub reports: Vec<ReportSummary>,
    pub invalid: Vec<InvalidReportEntry>,
}

/// Stable v1 JSON output of `report show`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportInspection {
    pub schema_version: u32,
    pub summary: ReportSummary,
    pub report: ReportDocument,
}

#[derive(Debug, serde::Deserialize)]
struct ReportFilingIndex {
    schema_version: u32,
    filings: BTreeMap<String, serde_json::Value>,
}

/// List captured reports without mutating report bytes, filing state, or
/// broker telemetry. Invalid artifacts are reported alongside valid ones.
pub fn list_reports(main_root: &Path) -> Result<ReportList, ReportCaptureError> {
    let reports_root = main_root.join(".aethyme/reports");
    ensure_safe_reports_directory(&reports_root)?;
    if !reports_root.exists() {
        return Ok(ReportList {
            schema_version: REPORT_INVENTORY_SCHEMA_VERSION,
            reports: Vec::new(),
            invalid: Vec::new(),
        });
    }
    let filed_digests = load_filed_digests(&reports_root)?;
    let entries = std::fs::read_dir(&reports_root).map_err(|source| ReportCaptureError::Io {
        action: "read report directory",
        path: ".aethyme/reports".into(),
        source,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ReportCaptureError::Io {
            action: "read report directory entry",
            path: ".aethyme/reports".into(),
            source,
        })?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name != REPORT_FILINGS_FILENAME && !name.starts_with(".report-"))
        {
            paths.push(path);
        }
    }
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    let mut reports = Vec::new();
    let mut invalid = Vec::new();
    for path in paths {
        let relative = report_relative_path(&path);
        match read_report(&path, &relative, &filed_digests) {
            Ok((summary, _)) => reports.push(summary),
            Err(error) => invalid.push(InvalidReportEntry {
                path: relative,
                error: inventory_error_message(&error),
            }),
        }
    }
    reports.sort_by(|left, right| {
        right
            .captured_at
            .cmp(&left.captured_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    invalid.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ReportList {
        schema_version: REPORT_INVENTORY_SCHEMA_VERSION,
        reports,
        invalid,
    })
}

/// Inspect one captured report selected by filename or its canonical
/// repository-relative report path.
pub fn show_report(
    main_root: &Path,
    requested: &Path,
) -> Result<ReportInspection, ReportCaptureError> {
    let reports_root = main_root.join(".aethyme/reports");
    ensure_safe_reports_directory(&reports_root)?;
    let filename = report_filename(requested)?;
    let relative = format!(".aethyme/reports/{filename}");
    let path = reports_root.join(filename);
    if !path.exists() {
        return Err(ReportCaptureError::ReportNotFound(relative));
    }
    let filed_digests = load_filed_digests(&reports_root)?;
    let (summary, report) = read_report(&path, &relative, &filed_digests)?;
    Ok(ReportInspection {
        schema_version: REPORT_INVENTORY_SCHEMA_VERSION,
        summary,
        report,
    })
}

fn ensure_safe_reports_directory(reports_root: &Path) -> Result<(), ReportCaptureError> {
    if std::fs::symlink_metadata(reports_root)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ReportCaptureError::SymlinkedReportDirectory(
            ".aethyme/reports".into(),
        ));
    }
    Ok(())
}

fn load_filed_digests(reports_root: &Path) -> Result<BTreeSet<String>, ReportCaptureError> {
    let path = reports_root.join(REPORT_FILINGS_FILENAME);
    let relative = format!(".aethyme/reports/{REPORT_FILINGS_FILENAME}");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => {
            return Err(ReportCaptureError::Io {
                action: "inspect report filing index",
                path: relative,
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReportCaptureError::InvalidReport {
            path: relative,
            reason: "filing index must be a regular file".into(),
        });
    }
    if metadata.len() > REPORT_MAX_BYTES {
        return Err(ReportCaptureError::InvalidReport {
            path: relative,
            reason: format!("filing index exceeds {REPORT_MAX_BYTES} bytes"),
        });
    }
    let bytes = std::fs::read(&path).map_err(|source| ReportCaptureError::Io {
        action: "read report filing index",
        path: relative.clone(),
        source,
    })?;
    let index = serde_json::from_slice::<ReportFilingIndex>(&bytes).map_err(|_| {
        ReportCaptureError::InvalidReport {
            path: relative.clone(),
            reason: "invalid filing index JSON or schema shape".into(),
        }
    })?;
    if index.schema_version != REPORT_FILINGS_SCHEMA_VERSION {
        return Err(ReportCaptureError::InvalidReport {
            path: relative,
            reason: format!(
                "unsupported filing index schema {}; expected {}",
                index.schema_version, REPORT_FILINGS_SCHEMA_VERSION
            ),
        });
    }
    let mut digests = BTreeSet::new();
    for digest in index.filings.into_keys() {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ReportCaptureError::InvalidReport {
                path: format!(".aethyme/reports/{REPORT_FILINGS_FILENAME}"),
                reason: "filing index contains an invalid digest key".into(),
            });
        }
        digests.insert(digest);
    }
    Ok(digests)
}

fn read_report(
    path: &Path,
    relative: &str,
    filed_digests: &BTreeSet<String>,
) -> Result<(ReportSummary, ReportDocument), ReportCaptureError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| ReportCaptureError::Io {
        action: "inspect captured report",
        path: relative.to_string(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReportCaptureError::InvalidReport {
            path: relative.to_string(),
            reason: "captured report must be a regular file".into(),
        });
    }
    if metadata.len() > REPORT_MAX_BYTES {
        return Err(ReportCaptureError::InvalidReport {
            path: relative.to_string(),
            reason: format!("artifact exceeds {REPORT_MAX_BYTES} bytes"),
        });
    }
    let bytes = std::fs::read(path).map_err(|source| ReportCaptureError::Io {
        action: "read captured report",
        path: relative.to_string(),
        source,
    })?;
    let digest = sha256_hex(&bytes);
    let report = serde_json::from_slice::<ReportDocument>(&bytes).map_err(|_| {
        ReportCaptureError::InvalidReport {
            path: relative.to_string(),
            reason: "invalid report JSON or schema shape".into(),
        }
    })?;
    if report.schema_version != REPORT_DOCUMENT_SCHEMA_VERSION {
        return Err(ReportCaptureError::InvalidReport {
            path: relative.to_string(),
            reason: format!(
                "unsupported report schema {}; expected {}",
                report.schema_version, REPORT_DOCUMENT_SCHEMA_VERSION
            ),
        });
    }
    if report.snapshot.schema_version != REPORT_SNAPSHOT_SCHEMA_VERSION {
        return Err(ReportCaptureError::InvalidReport {
            path: relative.to_string(),
            reason: format!(
                "unsupported snapshot schema {}; expected {}",
                report.snapshot.schema_version, REPORT_SNAPSHOT_SCHEMA_VERSION
            ),
        });
    }
    validate_report_document(&report, relative)?;
    let summary = ReportSummary {
        path: relative.to_string(),
        title: report.title.clone(),
        captured_at: report.captured_at,
        kind: report.kind,
        version: report.snapshot.build.version.clone(),
        report_schema_version: report.schema_version,
        filing_state: if filed_digests.contains(&digest) {
            ReportFilingState::Filed
        } else {
            ReportFilingState::Unfiled
        },
        digest,
    };
    Ok((summary, report))
}

fn validate_report_document(
    report: &ReportDocument,
    relative: &str,
) -> Result<(), ReportCaptureError> {
    let invalid = |reason: String| ReportCaptureError::InvalidReport {
        path: relative.to_string(),
        reason,
    };
    validate_title(&report.title).map_err(|error| invalid(error.to_string()))?;
    if report.captured_at < 0 {
        return Err(invalid("captured_at must be non-negative".into()));
    }
    if report.snapshot.recent_event_types.len() > REPORT_RECENT_EVENT_LIMIT
        || report.snapshot.operations.len() > REPORT_RECENT_OPERATION_LIMIT
        || report.snapshot.gates.len() > REPORT_RECENT_GATE_LIMIT
    {
        return Err(invalid("snapshot exceeds report collection bounds".into()));
    }
    if report
        .snapshot
        .recent_event_types
        .iter()
        .any(|event| !is_safe_event_kind(&event.kind))
    {
        return Err(invalid("snapshot contains an invalid event kind".into()));
    }
    if report
        .snapshot
        .gates
        .iter()
        .filter_map(|gate| gate.triggered_by.as_deref())
        .any(|path| !is_safe_repo_relative(path))
    {
        return Err(invalid(
            "snapshot contains a non-repository-relative trigger path".into(),
        ));
    }
    if !report.snapshot.includes_task
        && (report
            .snapshot
            .session
            .as_ref()
            .is_some_and(|session| session.task.is_some())
            || report
                .snapshot
                .operations
                .iter()
                .any(|operation| operation.authorization_reason.is_some()))
    {
        return Err(invalid(
            "snapshot contains task text without includes_task opt-in".into(),
        ));
    }
    Ok(())
}

fn report_relative_path(path: &Path) -> String {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    format!(".aethyme/reports/{filename}")
}

fn inventory_error_message(error: &ReportCaptureError) -> String {
    match error {
        ReportCaptureError::InvalidReport { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// Gather local broker state and finalize one report without network, Git,
/// graph, or gate activity.
pub fn prepare_report(
    broker: &mut Broker,
    kind: ReportKind,
    title: &str,
    session_id: Option<i64>,
    include_task: bool,
    captured_at: i64,
) -> Result<PreparedReport, ReportCaptureError> {
    validate_title(title)?;

    let session = session_id
        .map(|id| broker.store().session(id))
        .transpose()?;
    let events = broker
        .store()
        .recent_events(REPORT_RECENT_EVENT_LIMIT as i64, session_id)?;
    let operations = broker
        .store()
        .recent_coordinated_operations(REPORT_RECENT_OPERATION_LIMIT as i64, session_id)?;
    let gate_events = broker
        .store()
        .recent_gate_events(REPORT_RECENT_GATE_LIMIT as i64, session_id)?;
    let gate_rows = gate_events
        .iter()
        .filter_map(gate_observation_from_event)
        .collect::<Vec<_>>();
    let gate_observations = gate_rows
        .iter()
        .map(|(outcome, recorded_at)| ReportGateObservation {
            outcome,
            recorded_at: *recorded_at,
            triggered_by: None,
        })
        .collect::<Vec<_>>();

    let build = crate::version::current_binary_build();
    let mut builder =
        ReportSnapshotBuilder::new(&build, std::env::consts::OS, std::env::consts::ARCH)
            .recent_events(&events)
            .operations(&operations)
            .gate_observations(&gate_observations)
            .include_task(include_task);
    if let Some(session) = session.as_ref() {
        builder = builder.session(session);
    }
    let document = ReportDocument {
        schema_version: REPORT_DOCUMENT_SCHEMA_VERSION,
        kind,
        title: title.to_string(),
        captured_at,
        snapshot: builder.build(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    let sha256 = sha256_hex(&bytes);
    let suggested_filename = format!(
        "{}-{}-{}.json",
        captured_at,
        kind.as_str(),
        title_slug(title)
    );
    Ok(PreparedReport {
        document,
        bytes,
        sha256,
        suggested_filename,
    })
}

/// Publish a prepared report atomically beneath `.aethyme/reports/`.
/// Existing explicit destinations are never replaced.
pub fn write_report_atomic(
    main_root: &Path,
    requested_output: Option<&Path>,
    report: &PreparedReport,
) -> Result<ReportCaptureResult, ReportCaptureError> {
    let reports_root = main_root.join(".aethyme/reports");
    if std::fs::symlink_metadata(&reports_root)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ReportCaptureError::SymlinkedReportDirectory(
            ".aethyme/reports".into(),
        ));
    }
    std::fs::create_dir_all(&reports_root).map_err(|source| ReportCaptureError::Io {
        action: "create report directory",
        path: ".aethyme/reports".into(),
        source,
    })?;

    let explicit_name = requested_output.map(report_filename).transpose()?;
    let mut collision_index = 0_u32;
    loop {
        let filename = explicit_name.clone().unwrap_or_else(|| {
            if collision_index == 0 {
                report.suggested_filename.clone()
            } else {
                let stem = report
                    .suggested_filename
                    .strip_suffix(".json")
                    .unwrap_or(&report.suggested_filename);
                format!("{stem}-{collision_index}.json")
            }
        });
        let destination = reports_root.join(&filename);
        let relative = format!(".aethyme/reports/{filename}");

        let mut temporary = tempfile::Builder::new()
            .prefix(".report-")
            .tempfile_in(&reports_root)
            .map_err(|source| ReportCaptureError::Io {
                action: "create temporary report",
                path: ".aethyme/reports".into(),
                source,
            })?;
        temporary
            .write_all(&report.bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| ReportCaptureError::Io {
                action: "write temporary report",
                path: relative.clone(),
                source,
            })?;

        match temporary.persist_noclobber(&destination) {
            Ok(_) => {
                sync_directory(&reports_root)?;
                return Ok(ReportCaptureResult {
                    path: Some(relative),
                    sha256: report.sha256.clone(),
                    bytes: report.bytes.len(),
                });
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if explicit_name.is_some() {
                    return Err(ReportCaptureError::DestinationExists(relative));
                }
                collision_index = collision_index.saturating_add(1);
            }
            Err(error) => {
                return Err(ReportCaptureError::Io {
                    action: "publish report",
                    path: relative,
                    source: error.error,
                });
            }
        }
    }
}

fn validate_title(title: &str) -> Result<(), ReportCaptureError> {
    if title.trim().is_empty() {
        return Err(ReportCaptureError::InvalidTitle(
            "title must not be empty".into(),
        ));
    }
    if title.chars().any(char::is_control) {
        return Err(ReportCaptureError::InvalidTitle(
            "title must be a single line without control characters".into(),
        ));
    }
    Ok(())
}

fn report_filename(path: &Path) -> Result<String, ReportCaptureError> {
    let parts = path.components().collect::<Vec<_>>();
    let name = match parts.as_slice() {
        [Component::Normal(name)] => Some(name),
        [
            Component::Normal(aethyme),
            Component::Normal(reports),
            Component::Normal(name),
        ] if *aethyme == ".aethyme" && *reports == "reports" => Some(name),
        _ => None,
    };
    let Some(name) = name.and_then(|name| name.to_str()) else {
        return Err(ReportCaptureError::InvalidOutput(
            path.to_string_lossy().into_owned(),
        ));
    };
    if name.is_empty()
        || name == "."
        || name == ".."
        || name == REPORT_FILINGS_FILENAME
        || name.starts_with(".report-")
    {
        return Err(ReportCaptureError::InvalidOutput(
            path.to_string_lossy().into_owned(),
        ));
    }
    Ok(name.to_string())
}

fn sync_directory(path: &Path) -> Result<(), ReportCaptureError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ReportCaptureError::Io {
            action: "sync report directory",
            path: ".aethyme/reports".into(),
            source,
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "report".into()
    } else {
        slug
    }
}

fn gate_observation_from_event(event: &crate::Event) -> Option<(GateRunOutcome, i64)> {
    let payload = serde_json::from_str::<serde_json::Value>(event.payload_json.as_deref()?).ok()?;
    let gate = payload.get("gate")?.as_str()?.to_string();
    let tree_hash = payload.get("tree")?.as_str()?.to_string();
    let cached = event.kind == crate::events::GATE_CACHED;
    let status = if cached {
        crate::GateStatus::parse(payload.get("cached_status")?.as_str()?).ok()?
    } else {
        crate::GateStatus::parse(event.kind.strip_prefix("gate.")?).ok()?
    };
    let failure_class = payload
        .get("failure_class")
        .and_then(serde_json::Value::as_str)
        .map(crate::GateFailureClass::parse)
        .transpose()
        .ok()?;
    Some((
        GateRunOutcome {
            gate,
            tree_hash,
            status,
            failure_class,
            cached,
            exit_code: None,
            duration_ms: payload.get("saved_ms").and_then(serde_json::Value::as_i64),
            log_path: None,
        },
        event.ts,
    ))
}

/// One observed gate result plus the provenance not carried by
/// [`GateRunOutcome`] itself.
#[derive(Debug, Clone, Copy)]
pub struct ReportGateObservation<'a> {
    pub outcome: &'a GateRunOutcome,
    /// Unix epoch milliseconds.
    pub recorded_at: i64,
    /// Relevant gate-trigger path, when known. Unsafe or absolute spellings
    /// are omitted by the builder.
    pub triggered_by: Option<&'a str>,
}

/// Pure builder for a redacted report snapshot.
///
/// It performs no filesystem, environment, database, Git, or network access.
/// Capture layers gather rows and platform strings; this type only projects
/// them through the explicit report allowlist.
pub struct ReportSnapshotBuilder<'a> {
    build: &'a BinaryBuild,
    os: &'a str,
    arch: &'a str,
    session: Option<&'a Session>,
    events: &'a [Event],
    operations: &'a [CoordinatedOperation],
    gates: &'a [ReportGateObservation<'a>],
    include_task: bool,
}

impl<'a> ReportSnapshotBuilder<'a> {
    pub fn new(build: &'a BinaryBuild, os: &'a str, arch: &'a str) -> Self {
        Self {
            build,
            os,
            arch,
            session: None,
            events: &[],
            operations: &[],
            gates: &[],
            include_task: false,
        }
    }

    pub fn session(mut self, session: &'a Session) -> Self {
        self.session = Some(session);
        self
    }

    pub fn recent_events(mut self, events: &'a [Event]) -> Self {
        self.events = events;
        self
    }

    pub fn operations(mut self, operations: &'a [CoordinatedOperation]) -> Self {
        self.operations = operations;
        self
    }

    pub fn gate_observations(mut self, gates: &'a [ReportGateObservation<'a>]) -> Self {
        self.gates = gates;
        self
    }

    /// Opt in to task text and coordinated-operation authorization reasons.
    /// Future CLI capture must wire this only from an explicit
    /// `--include-task` invocation flag.
    pub fn include_task(mut self, include: bool) -> Self {
        self.include_task = include;
        self
    }

    pub fn build(self) -> ReportSnapshot {
        let session = self.session.map(|session| ReportSession {
            id: session.id,
            branch: session.branch.clone(),
            origin: session.origin,
            status: session.status,
            diff_base: session.diff_base.clone(),
            task: self.include_task.then(|| session.task.clone()).flatten(),
        });

        let mut recent_event_types = self
            .events
            .iter()
            .filter(|event| is_safe_event_kind(&event.kind))
            .map(|event| ReportEventType {
                id: event.id,
                recorded_at: event.ts,
                kind: event.kind.clone(),
                session_id: event.session_id,
            })
            .collect::<Vec<_>>();
        recent_event_types.sort_by(|left, right| {
            right
                .id
                .cmp(&left.id)
                .then_with(|| right.recorded_at.cmp(&left.recorded_at))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        recent_event_types.truncate(REPORT_RECENT_EVENT_LIMIT);

        let mut operations = self
            .operations
            .iter()
            .map(|operation| ReportOperation {
                id: operation.id,
                session_id: operation.session_id,
                provider: operation.provider,
                repository: operation.repository.clone(),
                effect: operation.effect,
                status: operation.status,
                exit_code: operation.exit_code,
                started_at: operation.created_at,
                finished_at: operation.finished_at,
                authorization_reason: self
                    .include_task
                    .then(|| operation.authorization_reason.clone())
                    .flatten(),
            })
            .collect::<Vec<_>>();
        operations.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        operations.truncate(REPORT_RECENT_OPERATION_LIMIT);

        let mut gates = self
            .gates
            .iter()
            .map(|observation| ReportGateProvenance {
                gate: observation.outcome.gate.clone(),
                tree_hash: observation.outcome.tree_hash.clone(),
                status: observation.outcome.status,
                failure_class: observation.outcome.failure_class,
                cache_source: if observation.outcome.cached {
                    ReportGateCacheSource::CacheHit
                } else {
                    ReportGateCacheSource::Executed
                },
                exit_code: observation.outcome.exit_code,
                duration_ms: observation.outcome.duration_ms,
                recorded_at: observation.recorded_at,
                triggered_by: observation
                    .triggered_by
                    .filter(|path| is_safe_repo_relative(path))
                    .map(str::to_string),
            })
            .collect::<Vec<_>>();
        gates.sort_by(|left, right| {
            right
                .recorded_at
                .cmp(&left.recorded_at)
                .then_with(|| left.gate.cmp(&right.gate))
                .then_with(|| left.tree_hash.cmp(&right.tree_hash))
        });
        gates.truncate(REPORT_RECENT_GATE_LIMIT);

        let last_known_failure = last_known_failure(self.session, self.operations, self.gates);

        ReportSnapshot {
            schema_version: REPORT_SNAPSHOT_SCHEMA_VERSION,
            includes_task: self.include_task,
            build: ReportBuild {
                version: self.build.version.clone(),
                commit: self.build.commit.clone(),
            },
            platform: ReportPlatform {
                os: self.os.to_string(),
                arch: self.arch.to_string(),
            },
            session,
            recent_event_types,
            operations,
            gates,
            last_known_failure,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportSnapshot {
    pub schema_version: u32,
    pub includes_task: bool,
    pub build: ReportBuild,
    pub platform: ReportPlatform,
    pub session: Option<ReportSession>,
    pub recent_event_types: Vec<ReportEventType>,
    pub operations: Vec<ReportOperation>,
    pub gates: Vec<ReportGateProvenance>,
    pub last_known_failure: Option<ReportLastFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportBuild {
    pub version: String,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportPlatform {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportSession {
    pub id: i64,
    pub branch: String,
    pub origin: SessionOrigin,
    pub status: SessionStatus,
    pub diff_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportEventType {
    pub id: i64,
    pub recorded_at: i64,
    pub kind: String,
    pub session_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportOperation {
    pub id: i64,
    pub session_id: i64,
    pub provider: OperationProvider,
    pub repository: String,
    pub effect: OperationEffect,
    pub status: OperationStatus,
    pub exit_code: Option<i64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportGateCacheSource {
    Executed,
    CacheHit,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReportGateProvenance {
    pub gate: String,
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cache_source: ReportGateCacheSource,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub recorded_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ReportLastFailure {
    Session {
        session_id: i64,
        recorded_at: i64,
        exit_code: i64,
    },
    Operation {
        operation_id: i64,
        recorded_at: i64,
        status: OperationStatus,
        exit_code: Option<i64>,
    },
    Gate {
        gate: String,
        tree_hash: String,
        recorded_at: i64,
        status: GateStatus,
        failure_class: Option<GateFailureClass>,
        exit_code: Option<i64>,
        cache_source: ReportGateCacheSource,
    },
}

struct FailureCandidate {
    recorded_at: i64,
    source_rank: u8,
    stable_key: String,
    failure: ReportLastFailure,
}

fn last_known_failure(
    session: Option<&Session>,
    operations: &[CoordinatedOperation],
    gates: &[ReportGateObservation<'_>],
) -> Option<ReportLastFailure> {
    let mut candidates = Vec::new();
    if let Some(session) = session
        && let Some(exit_code) = session.exit_code.filter(|code| *code != 0)
    {
        candidates.push(FailureCandidate {
            recorded_at: session.updated_at,
            source_rank: 0,
            stable_key: session.id.to_string(),
            failure: ReportLastFailure::Session {
                session_id: session.id,
                recorded_at: session.updated_at,
                exit_code,
            },
        });
    }

    for operation in operations {
        if !operation_failed(operation) {
            continue;
        }
        let recorded_at = operation.finished_at.unwrap_or(operation.updated_at);
        candidates.push(FailureCandidate {
            recorded_at,
            source_rank: 1,
            stable_key: operation.id.to_string(),
            failure: ReportLastFailure::Operation {
                operation_id: operation.id,
                recorded_at,
                status: operation.status,
                exit_code: operation.exit_code,
            },
        });
    }

    for observation in gates {
        if !gate_failed(observation.outcome) {
            continue;
        }
        candidates.push(FailureCandidate {
            recorded_at: observation.recorded_at,
            source_rank: 2,
            stable_key: format!(
                "{}:{}",
                observation.outcome.gate, observation.outcome.tree_hash
            ),
            failure: ReportLastFailure::Gate {
                gate: observation.outcome.gate.clone(),
                tree_hash: observation.outcome.tree_hash.clone(),
                recorded_at: observation.recorded_at,
                status: observation.outcome.status,
                failure_class: observation.outcome.failure_class,
                exit_code: observation.outcome.exit_code,
                cache_source: if observation.outcome.cached {
                    ReportGateCacheSource::CacheHit
                } else {
                    ReportGateCacheSource::Executed
                },
            },
        });
    }

    candidates
        .into_iter()
        .max_by(compare_failure_candidates)
        .map(|candidate| candidate.failure)
}

fn compare_failure_candidates(left: &FailureCandidate, right: &FailureCandidate) -> Ordering {
    left.recorded_at
        .cmp(&right.recorded_at)
        .then_with(|| left.source_rank.cmp(&right.source_rank))
        .then_with(|| left.stable_key.cmp(&right.stable_key))
}

fn operation_failed(operation: &CoordinatedOperation) -> bool {
    operation.exit_code.is_some_and(|code| code != 0)
        || matches!(
            operation.status,
            OperationStatus::Failed
                | OperationStatus::OutcomeUnknown
                | OperationStatus::ReconciledFailed
        )
}

fn gate_failed(outcome: &GateRunOutcome) -> bool {
    outcome.exit_code.is_some_and(|code| code != 0)
        || matches!(outcome.status, GateStatus::Fail | GateStatus::Error)
}

fn is_safe_event_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_safe_repo_relative(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.bytes().any(|byte| byte.is_ascii_control())
        || looks_like_windows_absolute(path)
    {
        return false;
    }
    !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn looks_like_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::process::Command;

    use super::*;

    fn build() -> BinaryBuild {
        BinaryBuild {
            version: "0.1.5".into(),
            describe: Some("v0.1.5-1-gabc".into()),
            commit: Some("abcdef0123456789".into()),
            path: Some("/private/bin/aethyme-BINARY-PATH-SECRET".into()),
        }
    }

    fn session() -> Session {
        Session {
            id: 42,
            worktree_path: "/private/repo/WORKTREE-PATH-SECRET".into(),
            branch: "agent/report-capture".into(),
            origin: SessionOrigin::Adopted,
            status: SessionStatus::Exited,
            task: Some("TASK-TEXT-SECRET".into()),
            diff_base: Some("0123456789abcdef".into()),
            pid: Some(999),
            command: Some("COMMAND-SECRET --token TOKEN-SECRET".into()),
            log_path: Some("/private/log/SESSION-LOG-SECRET".into()),
            exit_code: Some(17),
            created_at: 100,
            updated_at: 500,
            last_activity_at: 490,
        }
    }

    fn event(id: i64, kind: &str, payload: &str) -> Event {
        Event {
            id,
            schema_version: 1,
            ts: id * 10,
            kind: kind.into(),
            session_id: Some(42),
            payload_json: Some(payload.into()),
        }
    }

    fn operation() -> CoordinatedOperation {
        CoordinatedOperation {
            id: 7,
            session_id: 42,
            provider: OperationProvider::Github,
            repository: "owner/repo".into(),
            scope: "/private/repo/OPERATION-SCOPE-PATH-SECRET".into(),
            effect: OperationEffect::Write,
            status: OperationStatus::Failed,
            authorization_reason: Some("OPERATION-REASON-SECRET".into()),
            command_json: r#"["issue","create","FILE-CONTENT-SECRET"]"#.into(),
            pid: 1234,
            exit_code: Some(2),
            details_json: Some(r#"{"diff":"DIFF-SECRET","hunk":"HUNK-SECRET"}"#.into()),
            created_at: 200,
            updated_at: 300,
            finished_at: Some(300),
        }
    }

    fn gate() -> GateRunOutcome {
        GateRunOutcome {
            gate: "cargo-test".into(),
            tree_hash: "fedcba9876543210".into(),
            status: GateStatus::Fail,
            failure_class: Some(GateFailureClass::TestFailure),
            cached: false,
            exit_code: Some(101),
            duration_ms: Some(1200),
            log_path: Some("/private/log/GATE-LOG-SECRET".into()),
        }
    }

    #[test]
    fn default_snapshot_is_an_explicit_forbidden_field_boundary() {
        let build = build();
        let session = session();
        let events = vec![event(
            8,
            "gate.failed",
            r#"{"content":"FILE-CONTENT-SECRET","diff":"DIFF-SECRET","hunk":"HUNK-SECRET"}"#,
        )];
        let operations = vec![operation()];
        let gate = gate();
        let gates = vec![ReportGateObservation {
            outcome: &gate,
            recorded_at: 400,
            triggered_by: Some("/private/repo/ABSOLUTE-TRIGGER-PATH-SECRET"),
        }];

        let snapshot = ReportSnapshotBuilder::new(&build, "linux", "x86_64")
            .session(&session)
            .recent_events(&events)
            .operations(&operations)
            .gate_observations(&gates)
            .build();
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let value = serde_json::to_value(&snapshot).unwrap();

        for forbidden in [
            "BINARY-PATH-SECRET",
            "WORKTREE-PATH-SECRET",
            "TASK-TEXT-SECRET",
            "COMMAND-SECRET",
            "TOKEN-SECRET",
            "SESSION-LOG-SECRET",
            "OPERATION-SCOPE-PATH-SECRET",
            "OPERATION-REASON-SECRET",
            "FILE-CONTENT-SECRET",
            "DIFF-SECRET",
            "HUNK-SECRET",
            "GATE-LOG-SECRET",
            "ABSOLUTE-TRIGGER-PATH-SECRET",
        ] {
            assert!(
                !json.contains(forbidden),
                "leaked forbidden value {forbidden}"
            );
        }
        assert!(!snapshot.includes_task);
        assert_eq!(snapshot.session.as_ref().unwrap().task, None);
        assert_eq!(snapshot.operations[0].authorization_reason, None);
        assert_eq!(snapshot.gates[0].triggered_by, None);

        let root_keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            root_keys,
            BTreeSet::from([
                "build",
                "gates",
                "includes_task",
                "last_known_failure",
                "operations",
                "platform",
                "recent_event_types",
                "schema_version",
                "session",
            ])
        );
        let session_keys = value["session"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            session_keys,
            BTreeSet::from(["branch", "diff_base", "id", "origin", "status"])
        );
        let operation_keys = value["operations"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            operation_keys,
            BTreeSet::from([
                "effect",
                "exit_code",
                "finished_at",
                "id",
                "provider",
                "repository",
                "session_id",
                "started_at",
                "status",
            ])
        );
    }

    #[test]
    fn include_task_is_the_only_task_and_reason_opt_in() {
        let build = build();
        let session = session();
        let operations = vec![operation()];
        let snapshot = ReportSnapshotBuilder::new(&build, "macos", "aarch64")
            .session(&session)
            .operations(&operations)
            .include_task(true)
            .build();

        assert!(snapshot.includes_task);
        assert_eq!(
            snapshot.session.unwrap().task.as_deref(),
            Some("TASK-TEXT-SECRET")
        );
        assert_eq!(
            snapshot.operations[0].authorization_reason.as_deref(),
            Some("OPERATION-REASON-SECRET")
        );
    }

    #[test]
    fn snapshot_is_bounded_and_deterministic_with_relative_paths_only() {
        let build = build();
        let events = (1..=REPORT_RECENT_EVENT_LIMIT as i64 + 5)
            .rev()
            .map(|id| event(id, "gate.passed", "PAYLOAD-SECRET"))
            .collect::<Vec<_>>();
        let first_gate = GateRunOutcome {
            status: GateStatus::Pass,
            cached: true,
            exit_code: Some(0),
            ..gate()
        };
        let second_gate = GateRunOutcome {
            gate: "lint".into(),
            tree_hash: "aaaaaaaaaaaaaaaa".into(),
            status: GateStatus::Pass,
            failure_class: None,
            cached: false,
            exit_code: Some(0),
            duration_ms: Some(10),
            log_path: None,
        };
        let gates = vec![
            ReportGateObservation {
                outcome: &first_gate,
                recorded_at: 100,
                triggered_by: Some("src/lib.rs"),
            },
            ReportGateObservation {
                outcome: &second_gate,
                recorded_at: 100,
                triggered_by: Some("../outside.rs"),
            },
        ];

        let make = || {
            ReportSnapshotBuilder::new(&build, "linux", "x86_64")
                .recent_events(&events)
                .gate_observations(&gates)
                .build()
        };
        let first = make();
        let second = make();

        assert_eq!(first, second);
        assert_eq!(first.recent_event_types.len(), REPORT_RECENT_EVENT_LIMIT);
        assert_eq!(first.recent_event_types[0].id, 25);
        assert_eq!(first.recent_event_types.last().unwrap().id, 6);
        assert_eq!(first.gates[0].gate, "cargo-test");
        assert_eq!(first.gates[0].triggered_by.as_deref(), Some("src/lib.rs"));
        assert_eq!(first.gates[1].triggered_by, None);
    }

    #[test]
    fn last_known_failure_uses_latest_timestamp_without_failure_text() {
        let build = build();
        let session = session();
        let operations = vec![operation()];
        let gate = gate();
        let gates = vec![ReportGateObservation {
            outcome: &gate,
            recorded_at: 400,
            triggered_by: None,
        }];

        let snapshot = ReportSnapshotBuilder::new(&build, "linux", "x86_64")
            .session(&session)
            .operations(&operations)
            .gate_observations(&gates)
            .build();

        assert_eq!(
            snapshot.last_known_failure,
            Some(ReportLastFailure::Session {
                session_id: 42,
                recorded_at: 500,
                exit_code: 17,
            })
        );
    }

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn prepared_bytes_have_stable_sha256_and_atomic_no_clobber_output() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let mut broker = Broker::open(tmp.path()).unwrap();
        let report = prepare_report(
            &mut broker,
            ReportKind::Bug,
            "Gate failed",
            None,
            false,
            1_234,
        )
        .unwrap();

        assert_eq!(report.sha256, sha256_hex(&report.bytes));
        assert_eq!(report.bytes.last(), Some(&b'\n'));
        assert_eq!(report.suggested_filename, "1234-bug-gate-failed.json");

        let first =
            write_report_atomic(tmp.path(), Some(Path::new("review.json")), &report).unwrap();
        assert_eq!(first.path.as_deref(), Some(".aethyme/reports/review.json"));
        assert_eq!(
            std::fs::read(tmp.path().join(first.path.unwrap())).unwrap(),
            report.bytes
        );
        assert!(matches!(
            write_report_atomic(tmp.path(), Some(Path::new("review.json")), &report),
            Err(ReportCaptureError::DestinationExists(_))
        ));
        assert!(
            std::fs::read_dir(tmp.path().join(".aethyme/reports"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".report-"))
        );

        let default_first = write_report_atomic(tmp.path(), None, &report).unwrap();
        let default_second = write_report_atomic(tmp.path(), None, &report).unwrap();
        assert_eq!(
            default_first.path.as_deref(),
            Some(".aethyme/reports/1234-bug-gate-failed.json")
        );
        assert_eq!(
            default_second.path.as_deref(),
            Some(".aethyme/reports/1234-bug-gate-failed-1.json")
        );
    }

    #[test]
    fn preparation_preserves_cached_gate_provenance_from_events() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let mut broker = Broker::open(tmp.path()).unwrap();
        broker
            .store()
            .append_event(
                crate::events::GATE_CACHED,
                None,
                Some(&crate::events::gate_cached_payload(
                    "cargo-test",
                    "0123456789abcdef",
                    42,
                    GateStatus::Pass,
                    None,
                )),
            )
            .unwrap();

        let report = prepare_report(
            &mut broker,
            ReportKind::Improvement,
            "Cache provenance",
            None,
            false,
            2_000,
        )
        .unwrap();
        let gate = &report.document.snapshot.gates[0];
        assert_eq!(gate.gate, "cargo-test");
        assert_eq!(gate.tree_hash, "0123456789abcdef");
        assert_eq!(gate.cache_source, ReportGateCacheSource::CacheHit);
        assert_eq!(gate.duration_ms, Some(42));
    }

    #[test]
    fn report_output_is_confined_and_titles_are_single_line() {
        for unsafe_path in [
            Path::new("../report.json"),
            Path::new("/tmp/report.json"),
            Path::new("nested/report.json"),
            Path::new(".aethyme/reports/nested/report.json"),
            Path::new(".filings.json"),
            Path::new(".report-temporary"),
        ] {
            assert!(matches!(
                report_filename(unsafe_path),
                Err(ReportCaptureError::InvalidOutput(_))
            ));
        }
        assert_eq!(
            report_filename(Path::new(".aethyme/reports/review.json")).unwrap(),
            "review.json"
        );
        assert!(validate_title("Useful title").is_ok());
        assert!(validate_title("line one\nline two").is_err());
        assert!(validate_title("  ").is_err());
    }

    #[test]
    fn sha256_matches_the_standard_empty_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
