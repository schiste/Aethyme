//! The analysis result envelope: what an analysis answer is about, and how far
//! it can be trusted (plan §5.7, §6.11; D38, D44, D47; AQ0; #655).
//!
//! Analysis is advisory. It never changes gate, lease or acceptance
//! authority, and every envelope says so (`authority: advisory_analysis`).
//! What the envelope adds is honesty about scope: an answer names its exact
//! subject and analysis profile, and four independent dimensions say how much
//! of it is known.
//!
//! | Dimension | Values | Meaning |
//! |---|---|---|
//! | `outcome` | available, unavailable, incompatible | Did the query produce a result at all? |
//! | `freshness` | exact, stale | Is the analysis bound to the stated subject? |
//! | `coverage` | complete_within_profile, partial | Did the declared model and query run to completion? |
//! | `limits` | within_limits, truncated | Did a budget cut the result short? |
//!
//! They are separate because they fail separately: a result can be exact for
//! its source and partial in coverage at once, and no scalar confidence can
//! express that (§5.7). Each is a state field of the [record](super::record)
//! layer, so a missing or unrecognized value reads as unknown, never as the
//! good value. `complete_within_profile` means the declared model ran to
//! completion, not that every runtime effect was found.
//!
//! **Empty is not absence.** [`AnalysisEnvelope::absence_is_evidence`] is true
//! only when all four dimensions are at their best known value. In every other
//! case an empty result says nothing about whether impact, references or
//! callers exist.
//!
//! ## Subjects and profiles
//!
//! A subject is an exact [`SourceSnapshotId`](super::SourceSnapshotId), a
//! **legacy Git revision**, or a **changed-path set**. Today's engine binds analysis to a committed
//! revision and a digest over Git object ids (L0 slice D), not to raw bytes, so
//! the baseline adapter must use the legacy form and cannot claim a
//! `SourceSnapshotId` it never computed. Likewise today's impact query takes a
//! list of changed paths, not a candidate tree, so its candidate subject is a
//! changed-path set.
//!
//! A profile is either **pinned** (the [`RecordId`] of an
//! `aethyme.analysis-profile` record naming producer, versions, languages,
//! edge kinds and configuration) or **legacy** (a name such as
//! `aethyme-engine/0.8.26/graph-impact-calls`, standing for whatever that
//! engine version did). Two results are comparable only under the same
//! profile ([`compare_profiles`]); otherwise the answer is
//! `incompatible_analysis_profile`, never a comparison of unrelated meanings.

use super::SourceSnapshotId;
use super::canonical_json::{Object, Value};
use super::record::{
    FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema, StateReading,
};

/// The `schema` of an analysis result record.
pub const ANALYSIS_RESULT_SCHEMA_NAME: &str = "aethyme.analysis-result/experimental-v0";

/// The `schema` of an analysis profile record; its ID pins a profile.
pub const ANALYSIS_PROFILE_SCHEMA_NAME: &str = "aethyme.analysis-profile/experimental-v0";

/// The only authority an analysis result can carry.
pub const ADVISORY_AUTHORITY: &str = "advisory_analysis";

const OUTCOMES: &[&str] = &["available", "unavailable", "incompatible"];
const FRESHNESS: &[&str] = &["exact", "stale"];
const COVERAGE: &[&str] = &["complete_within_profile", "partial"];
const LIMITS: &[&str] = &["within_limits", "truncated"];
const OPERATIONS: [&str; 4] = [
    "resolve_symbol",
    "find_references",
    "describe_change",
    "explain_impact",
];

const fn field(name: &'static str, required: bool, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required,
        kind,
        capability: None,
    }
}

/// The analysis result record schema.
pub static ANALYSIS_RESULT_SCHEMA: RecordSchema = RecordSchema {
    name: ANALYSIS_RESULT_SCHEMA_NAME,
    fields: &[
        field("operation", true, FieldKind::String),
        field("subject", true, FieldKind::Opaque),
        field("base_subject", false, FieldKind::Opaque),
        field("profile", true, FieldKind::Opaque),
        field("authority", true, FieldKind::String),
        field("outcome", false, FieldKind::State(OUTCOMES)),
        field("freshness", false, FieldKind::State(FRESHNESS)),
        field("coverage", false, FieldKind::State(COVERAGE)),
        field("limits", false, FieldKind::State(LIMITS)),
        field("reason", false, FieldKind::String),
        field("gaps", false, FieldKind::StringSet),
        field("heuristic_confidence", false, FieldKind::String),
        field("provenance", false, FieldKind::Opaque),
        field("limit_detail", false, FieldKind::Opaque),
        field("result", false, FieldKind::Opaque),
    ],
    capabilities: &[],
};

/// The analysis profile record schema. Its record ID is the pinned profile
/// identity, so any change to what it lists is a new profile.
pub static ANALYSIS_PROFILE_SCHEMA: RecordSchema = RecordSchema {
    name: ANALYSIS_PROFILE_SCHEMA_NAME,
    fields: &[
        field("producer", true, FieldKind::String),
        field("producer_version", true, FieldKind::String),
        field("schema_version", true, FieldKind::String),
        field("languages", true, FieldKind::StringSet),
        field("edge_kinds", true, FieldKind::StringSet),
        field("configuration_digest", false, FieldKind::String),
        field("limits", false, FieldKind::Opaque),
    ],
    capabilities: &[],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    ResolveSymbol,
    FindReferences,
    DescribeChange,
    ExplainImpact,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        OPERATIONS[self as usize]
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match OPERATIONS.iter().position(|op| *op == text)? {
            0 => Self::ResolveSymbol,
            1 => Self::FindReferences,
            2 => Self::DescribeChange,
            _ => Self::ExplainImpact,
        })
    }

    /// Operations that compare a base with a candidate.
    pub fn is_comparison(self) -> bool {
        matches!(self, Self::DescribeChange | Self::ExplainImpact)
    }
}

/// What an analysis is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// Exact source bytes (#652).
    Snapshot(SourceSnapshotId),
    /// A committed Git revision, as the current engine binds analysis. Raw
    /// byte identity is not established.
    LegacyGitRevision(String),
    /// A set of changed paths, identified by the legacy graph-impact digest
    /// (lowercase hex SHA-256 of the JSON array of sorted, unique paths). It
    /// names which paths changed, not their content: it is not a candidate
    /// snapshot.
    ChangedPaths(String),
}

/// Which observation method produced the analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRef {
    /// The ID of an `aethyme.analysis-profile` record.
    Pinned(RecordId),
    /// A named legacy behaviour, e.g. `aethyme-engine/0.8.26/graph-impact-calls`.
    Legacy(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Available,
    Unavailable,
    Incompatible,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Exact,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    CompleteWithinProfile,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limits {
    WithinLimits,
    Truncated,
    Unknown,
}

/// The single most severe reading of an envelope, for consumers that need
/// one word. The dimensions remain the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Every dimension at its best known value.
    Complete,
    /// Coverage partial, or a dimension unknown.
    Partial,
    Truncated,
    Stale,
    Unavailable,
    Incompatible,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Truncated => "truncated",
            Self::Stale => "stale",
            Self::Unavailable => "unavailable",
            Self::Incompatible => "incompatible",
        }
    }
}

/// A decoded analysis result envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisEnvelope {
    pub operation: Operation,
    pub subject: Subject,
    pub base_subject: Option<Subject>,
    pub profile: ProfileRef,
    pub outcome: Outcome,
    pub freshness: Freshness,
    pub coverage: Coverage,
    pub limits: Limits,
    /// Why the outcome is not `available`, or what a gap means.
    pub reason: Option<String>,
    /// Named coverage gaps (`missing_edge_kind:calls`, `truncated`, …).
    pub gaps: Vec<String>,
    /// A producer's own confidence label. Kept for display only: it never
    /// implies coverage or freshness.
    pub heuristic_confidence: Option<String>,
    /// Producer, engine and digest provenance, carried as-is.
    pub provenance: Option<Value>,
    /// Budget details, carried as-is.
    pub limit_detail: Option<Value>,
    /// The operation's result. Present only when the outcome is available.
    pub result: Option<Value>,
}

impl AnalysisEnvelope {
    /// The most severe reading across the four dimensions.
    pub fn status(&self) -> Status {
        match self.outcome {
            Outcome::Incompatible => return Status::Incompatible,
            Outcome::Unavailable | Outcome::Unknown => return Status::Unavailable,
            Outcome::Available => {}
        }
        if self.freshness == Freshness::Stale {
            return Status::Stale;
        }
        if self.limits == Limits::Truncated {
            return Status::Truncated;
        }
        if self.freshness == Freshness::Exact
            && self.coverage == Coverage::CompleteWithinProfile
            && self.limits == Limits::WithinLimits
        {
            Status::Complete
        } else {
            Status::Partial
        }
    }

    /// True only when an empty result means "none exist within this profile":
    /// available, exact, complete within the profile and not truncated.
    pub fn absence_is_evidence(&self) -> bool {
        self.status() == Status::Complete
    }

    /// Decode and check an analysis result record.
    pub fn from_record(input: &[u8]) -> Result<Self, AnalysisError> {
        let record = Record::decode(input, &[&ANALYSIS_RESULT_SCHEMA])?;
        let text = |name: &str| match record.get(name) {
            Some(Value::String(text)) => Some(text.clone()),
            _ => None,
        };
        let operation_text = text("operation").unwrap_or_default();
        let operation =
            Operation::parse(&operation_text).ok_or(AnalysisError::UnsupportedOperation {
                operation: operation_text,
            })?;
        if text("authority").as_deref() != Some(ADVISORY_AUTHORITY) {
            return Err(AnalysisError::NotAdvisory);
        }
        let subject = read_subject("subject", record.get("subject"))?;
        let base_subject = record
            .get("base_subject")
            .map(|value| read_subject("base_subject", Some(value)))
            .transpose()?;
        if operation.is_comparison() != base_subject.is_some() {
            return Err(AnalysisError::BaseSubjectMismatch);
        }
        let profile = read_profile(record.get("profile"))?;
        let state = |name: &str| match record.state(name) {
            StateReading::Known(value) => Some(value),
            StateReading::Unknown | StateReading::Unrecognized(_) => None,
        };
        let outcome = match state("outcome") {
            Some("available") => Outcome::Available,
            Some("unavailable") => Outcome::Unavailable,
            Some("incompatible") => Outcome::Incompatible,
            _ => Outcome::Unknown,
        };
        let freshness = match state("freshness") {
            Some("exact") => Freshness::Exact,
            Some("stale") => Freshness::Stale,
            _ => Freshness::Unknown,
        };
        let coverage = match state("coverage") {
            Some("complete_within_profile") => Coverage::CompleteWithinProfile,
            Some("partial") => Coverage::Partial,
            _ => Coverage::Unknown,
        };
        let limits = match state("limits") {
            Some("within_limits") => Limits::WithinLimits,
            Some("truncated") => Limits::Truncated,
            _ => Limits::Unknown,
        };
        let result = record.get("result").cloned();
        if result.is_some() && outcome != Outcome::Available {
            return Err(AnalysisError::ResultWithoutOutcome);
        }
        let gaps = match record.get("gaps") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            operation,
            subject,
            base_subject,
            profile,
            outcome,
            freshness,
            coverage,
            limits,
            reason: text("reason"),
            gaps,
            heuristic_confidence: text("heuristic_confidence"),
            provenance: record.get("provenance").cloned(),
            limit_detail: record.get("limit_detail").cloned(),
            result,
        })
    }

    /// The canonical record and its ID, for producers. Unknown dimensions are
    /// omitted, so they read back as unknown.
    ///
    /// A reader passing a result on forwards the original record bytes rather
    /// than re-encoding: re-encoding drops values it did not recognize, which a
    /// newer reader may understand.
    ///
    /// # Panics
    ///
    /// If the envelope breaks a rule [`AnalysisEnvelope::from_record`] checks
    /// (a result without an available outcome, a base subject on a
    /// non-comparison, …).
    pub fn to_record(&self) -> (Vec<u8>, RecordId) {
        let text = |s: &str| Value::String(s.to_string());
        let mut members = vec![
            ("schema".to_string(), text(ANALYSIS_RESULT_SCHEMA_NAME)),
            ("operation".to_string(), text(self.operation.as_str())),
            ("subject".to_string(), subject_value(&self.subject)),
            ("profile".to_string(), profile_value(&self.profile)),
            ("authority".to_string(), text(ADVISORY_AUTHORITY)),
        ];
        if let Some(base) = &self.base_subject {
            members.push(("base_subject".to_string(), subject_value(base)));
        }
        let states = [
            (
                "outcome",
                match self.outcome {
                    Outcome::Available => Some("available"),
                    Outcome::Unavailable => Some("unavailable"),
                    Outcome::Incompatible => Some("incompatible"),
                    Outcome::Unknown => None,
                },
            ),
            (
                "freshness",
                match self.freshness {
                    Freshness::Exact => Some("exact"),
                    Freshness::Stale => Some("stale"),
                    Freshness::Unknown => None,
                },
            ),
            (
                "coverage",
                match self.coverage {
                    Coverage::CompleteWithinProfile => Some("complete_within_profile"),
                    Coverage::Partial => Some("partial"),
                    Coverage::Unknown => None,
                },
            ),
            (
                "limits",
                match self.limits {
                    Limits::WithinLimits => Some("within_limits"),
                    Limits::Truncated => Some("truncated"),
                    Limits::Unknown => None,
                },
            ),
        ];
        for (name, value) in states {
            if let Some(value) = value {
                members.push((name.to_string(), text(value)));
            }
        }
        let optional = [
            ("reason", self.reason.as_deref().map(text)),
            (
                "heuristic_confidence",
                self.heuristic_confidence.as_deref().map(text),
            ),
            (
                "gaps",
                (!self.gaps.is_empty())
                    .then(|| Value::Array(self.gaps.iter().map(|gap| text(gap)).collect())),
            ),
            ("provenance", self.provenance.clone()),
            ("limit_detail", self.limit_detail.clone()),
            ("result", self.result.clone()),
        ];
        for (name, value) in optional {
            if let Some(value) = value {
                members.push((name.to_string(), value));
            }
        }
        let requires = ANALYSIS_RESULT_SCHEMA
            .required_capabilities(members.iter().map(|(name, _)| name.as_str()));
        if !requires.is_empty() {
            members.push((
                "requires".to_string(),
                Value::Array(requires.into_iter().map(text).collect()),
            ));
        }
        let bytes =
            Value::Object(Object::new(members).expect("distinct keys")).to_canonical_bytes();
        let decoded = Self::from_record(&bytes).expect("an envelope encodes to a valid record");
        debug_assert_eq!(decoded.status(), self.status());
        let id = Record::decode(&bytes, &[&ANALYSIS_RESULT_SCHEMA])
            .expect("checked above")
            .id();
        (bytes, id)
    }
}

/// Whether results under two profiles can be compared or combined.
pub fn compare_profiles(base: &ProfileRef, candidate: &ProfileRef) -> Result<(), AnalysisError> {
    if base == candidate {
        Ok(())
    } else {
        Err(AnalysisError::IncompatibleProfile {
            base: profile_label(base),
            candidate: profile_label(candidate),
        })
    }
}

/// A continuation token for a paged query. It pins one analysis view and one
/// query, so pages from different generations can never be mixed (§6.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub view: String,
    pub query_digest: String,
    pub offset: u64,
}

impl Cursor {
    /// Refuse a cursor minted for another view or another query.
    pub fn check(&self, current_view: &str, query_digest: &str) -> Result<u64, AnalysisError> {
        if self.view == current_view && self.query_digest == query_digest {
            Ok(self.offset)
        } else {
            Err(AnalysisError::StaleCursor)
        }
    }
}

fn profile_label(profile: &ProfileRef) -> String {
    match profile {
        ProfileRef::Pinned(id) => id.to_string(),
        ProfileRef::Legacy(name) => format!("legacy:{name}"),
    }
}

fn subject_value(subject: &Subject) -> Value {
    let (kind, key, value) = match subject {
        Subject::Snapshot(id) => ("source_snapshot", "id", id.as_str()),
        Subject::LegacyGitRevision(revision) => {
            ("legacy_git_revision", "revision", revision.as_str())
        }
        Subject::ChangedPaths(digest) => ("changed_paths", "digest", digest.as_str()),
    };
    object(&[("kind", kind), (key, value)])
}

fn profile_value(profile: &ProfileRef) -> Value {
    match profile {
        ProfileRef::Pinned(id) => object(&[("kind", "pinned"), ("id", id.as_str())]),
        ProfileRef::Legacy(name) => object(&[("kind", "legacy"), ("name", name)]),
    }
}

fn object(members: &[(&str, &str)]) -> Value {
    Value::Object(
        Object::new(
            members
                .iter()
                .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                .collect(),
        )
        .expect("distinct keys"),
    )
}

/// Read a `{kind, …}` object with exactly the members its kind needs.
fn tagged<'a>(
    field: &str,
    value: Option<&'a Value>,
    kinds: &[(&str, &str)],
) -> Result<(&'a str, &'a str), AnalysisError> {
    let malformed = || AnalysisError::Malformed {
        field: field.to_string(),
    };
    let Some(Value::Object(object)) = value else {
        return Err(malformed());
    };
    let Some(Value::String(kind)) = object.get("kind") else {
        return Err(malformed());
    };
    let (_, key) = kinds
        .iter()
        .find(|(k, _)| k == kind)
        .ok_or_else(malformed)?;
    match object.get(key) {
        Some(Value::String(text)) if object.len() == 2 && !text.is_empty() => Ok((kind, text)),
        _ => Err(malformed()),
    }
}

fn read_subject(field: &str, value: Option<&Value>) -> Result<Subject, AnalysisError> {
    let kinds = [
        ("source_snapshot", "id"),
        ("legacy_git_revision", "revision"),
        ("changed_paths", "digest"),
    ];
    match tagged(field, value, &kinds)? {
        ("source_snapshot", id) => {
            SourceSnapshotId::parse(id)
                .map(Subject::Snapshot)
                .map_err(|_| AnalysisError::Malformed {
                    field: field.to_string(),
                })
        }
        ("legacy_git_revision", revision) if is_git_object_id(revision) => {
            Ok(Subject::LegacyGitRevision(revision.to_string()))
        }
        ("changed_paths", digest) if digest.len() == 64 && is_git_object_id(digest) => {
            Ok(Subject::ChangedPaths(digest.to_string()))
        }
        _ => Err(AnalysisError::Malformed {
            field: field.to_string(),
        }),
    }
}

fn read_profile(value: Option<&Value>) -> Result<ProfileRef, AnalysisError> {
    match tagged("profile", value, &[("pinned", "id"), ("legacy", "name")])? {
        ("pinned", id) => {
            RecordId::parse(id)
                .map(ProfileRef::Pinned)
                .map_err(|_| AnalysisError::Malformed {
                    field: "profile".into(),
                })
        }
        (_, name) => Ok(ProfileRef::Legacy(name.to_string())),
    }
}

/// A full lowercase SHA-1 or SHA-256 Git object id.
fn is_git_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("unsupported analysis operation {operation:?}")]
    UnsupportedOperation { operation: String },
    #[error(
        "an analysis result must carry authority {ADVISORY_AUTHORITY:?}; analysis never grants gate, lease or acceptance authority"
    )]
    NotAdvisory,
    #[error("{field} is malformed")]
    Malformed { field: String },
    #[error(
        "describe_change and explain_impact need a base_subject; other operations must not have one"
    )]
    BaseSubjectMismatch,
    #[error("a result is present but the outcome is not available")]
    ResultWithoutOutcome,
    #[error("incompatible_analysis_profile: {base} vs {candidate}")]
    IncompatibleProfile { base: String, candidate: String },
    #[error("the cursor belongs to another analysis view or query; restart the query")]
    StaleCursor,
}

impl AnalysisError {
    /// The stable refusal code shared with the golden vectors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Record(error) => error.code(),
            Self::UnsupportedOperation { .. } => "unsupported_operation",
            Self::NotAdvisory => "not_advisory",
            Self::Malformed { .. } => "malformed",
            Self::BaseSubjectMismatch => "base_subject_mismatch",
            Self::ResultWithoutOutcome => "result_without_outcome",
            Self::IncompatibleProfile { .. } => "incompatible_analysis_profile",
            Self::StaleCursor => "stale_cursor",
        }
    }
}

/// The digest a pinned profile is known by, for callers building one from a
/// profile record they already decoded.
pub fn profile_id(profile_record: &Record) -> Option<RecordId> {
    (profile_record.schema() == ANALYSIS_PROFILE_SCHEMA_NAME).then(|| profile_record.id())
}
