//! Bounded resolution requests and exact candidate manifests (L4, #665;
//! plan v3 §7.3 steps 6-9, §7.4, §7.5). Provisional until E1 (#650).
//!
//! Three records, all canonical (#653) and kept in the archive:
//!
//! - A **candidate manifest** ([`write_manifest`]) names exactly what one
//!   composition used and produced: every retained input, the baseline, the
//!   profile, the recipe, the output subject and the actual mode, plus what
//!   is still undecided. A composition that produced no candidate gets the
//!   same record without a subject, so an attempt is never success-shaped.
//! - A **resolution request** ([`request_resolution`]) turns a conflict, or
//!   a candidate whose independent checks failed, into a bounded decision.
//!   Raising it is the authorization point: whatever preference it carries
//!   was given by whoever raised it.
//! - A **synthesized group** ([`resolve`]) records a resolver's accepted
//!   proposal and the new candidate it became.
//!
//! [`resolve`] takes only the request's record reference. It reads the
//! archived record, re-reads every contribution it names from the archive,
//! and recomputes the decision key, the scope and the protected paths;
//! nothing a caller holds in memory is trusted. It then enforces, in order:
//!
//! 1. the allowance: [`MAX_SYNTHESIS_ATTEMPTS`] per decision, charged before
//!    dispatch, shared by every request for the same decision or for a
//!    larger or smaller set of the same contributions;
//! 2. scope: writes outside the permitted paths, to protected policy,
//!    harness or toolchain paths under any spelling, that touch a symlink,
//!    or that make a file executable are `scope_violation`;
//! 3. no last writer: without an authorized preference, keeping one side of
//!    a conflict (compared after whitespace and line-ending normalization)
//!    is `unresolved`;
//! 4. nothing dropped: a proposal that removes a contribution's change from
//!    a path is `unresolved`, for conflicts and failed checks alike.
//!
//! Only then is the proposal materialized as a new candidate, produced by
//! [`Producer::Resolver`] and unverified like every candidate. Every failure,
//! including an error after dispatch, is recorded as one of `unresolved`,
//! `inconclusive`, `scope_violation`, `infrastructure_deferred` or
//! `budget_exhausted`.
//!
//! No resolver ships here: [`Resolver`] is the narrow interface a bounded
//! synthesizer implements, exercised by fakes in tests.
//!
//! See `docs/architecture/local-v3-l4-resolution.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use aethyme_contracts::experimental_v0::canonical_json::{MAX_DOCUMENT_BYTES, Object, Value};
use aethyme_contracts::experimental_v0::{
    EntryKind, FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rusqlite::{OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::collaboration_archive::{self, ArchiveError, CommitOid, ObjectDigest, RetainedSnapshot};
use crate::collaboration_capture::OperationId;
use crate::collaboration_state::CollaborationStore;
use crate::composer::{
    self, COMPOSER_BASELINE_SOURCE, ComposeError, Composition, CompositionRequest, Entries, Entry,
};
use crate::composition::{
    Candidate, CandidateInput, CompositionMode, CompositionOutcome, Producer,
};

pub const CANDIDATE_MANIFEST_SCHEMA_NAME: &str = "aethyme.candidate-manifest/experimental-v0";
pub const RESOLUTION_REQUEST_SCHEMA_NAME: &str = "aethyme.resolution-request/experimental-v0";
pub const SYNTHESIZED_GROUP_SCHEMA_NAME: &str = "aethyme.synthesized-group/experimental-v0";

/// The producer profile of a candidate a resolver wrote.
pub const SYNTHESIS_PROFILE: &str = "aethyme.resolve.bounded/provisional-v0";

/// Automatic synthesis attempts per decision (plan §7.5's provisional
/// experiment; D15 sets the real caps before paid dispatch).
pub const MAX_SYNTHESIS_ATTEMPTS: u32 = 2;

/// Maximum encoded asynchronous response retained for one attempt.
const MAX_ASYNC_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const ASYNC_ATTEMPT_SCHEMA_VERSION: i64 = 27;
const ASYNC_RESPONSE_MAGIC: &[u8] = b"aethyme-resolution-response\0\x01";
const ASYNC_INPUT_MANIFEST_MAGIC: &[u8] = b"aethyme-resolution-inputs\0\x01";
pub const ASYNC_RESPONSE_ENVELOPE_SCHEMA: &str =
    "aethyme.resolution-attempt-response/experimental-v0";

/// Directory names no resolver may write under, at any depth: the broker's
/// policy and gates, CI, Cargo configuration and tool configuration such as
/// `.config/nextest.toml` (whose `default-filter` can turn a test gate into
/// a no-op).
pub const PROTECTED_DIRECTORIES: &[&str] = &[".aethyme", ".github", ".cargo", ".config", ".git"];

/// File names no resolver may write, at any depth: Git settings that change
/// how content is read, the toolchain pin, build manifests and build
/// scripts.
pub const PROTECTED_FILES: &[&str] = &[
    ".gitattributes",
    ".gitmodules",
    "rust-toolchain",
    "rust-toolchain.toml",
    "cargo.toml",
    "cargo.lock",
    "build.rs",
];

/// The review rule requirement whose paths are protected too.
const SECURITY_REQUIREMENT: &str = "security";

const fn field(name: &'static str, kind: FieldKind, required: bool) -> FieldSpec {
    FieldSpec {
        name,
        required,
        kind,
        capability: None,
    }
}

pub static CANDIDATE_MANIFEST_SCHEMA: RecordSchema = RecordSchema {
    name: CANDIDATE_MANIFEST_SCHEMA_NAME,
    fields: &[
        field("outcome", FieldKind::String, true),
        field("profile", FieldKind::String, true),
        field("baseline_commit", FieldKind::String, true),
        field("baseline", FieldKind::String, false),
        field("baseline_unreadable", FieldKind::String, false),
        field("inputs", FieldKind::Opaque, true),
        field("order", FieldKind::Opaque, true),
        field("recipe", FieldKind::String, false),
        field("subject", FieldKind::String, false),
        field("candidate_commit", FieldKind::String, false),
        field("mode", FieldKind::String, false),
        field("requires_verification", FieldKind::Boolean, true),
        field("unresolved", FieldKind::Opaque, true),
        field("detail", FieldKind::String, false),
    ],
    capabilities: &[],
};

pub static RESOLUTION_REQUEST_SCHEMA: RecordSchema = RecordSchema {
    name: RESOLUTION_REQUEST_SCHEMA_NAME,
    fields: &[
        field("kind", FieldKind::String, true),
        field("decision", FieldKind::String, true),
        field("decision_members", FieldKind::Opaque, true),
        field("decision_paths", FieldKind::Opaque, true),
        field("members", FieldKind::Opaque, true),
        field("contents", FieldKind::Opaque, true),
        field("trigger", FieldKind::Opaque, true),
        field("baseline", FieldKind::String, true),
        field("baseline_commit", FieldKind::String, true),
        field("accumulator", FieldKind::String, true),
        field("accumulator_commit", FieldKind::String, true),
        field("requirements", FieldKind::Opaque, true),
        field("scope", FieldKind::Opaque, true),
        field("protected", FieldKind::Opaque, true),
        field("preference", FieldKind::Opaque, false),
    ],
    capabilities: &[],
};

pub static SYNTHESIZED_GROUP_SCHEMA: RecordSchema = RecordSchema {
    name: SYNTHESIZED_GROUP_SCHEMA_NAME,
    fields: &[
        field("request", FieldKind::String, true),
        field("decision", FieldKind::String, true),
        field("constituents", FieldKind::Opaque, true),
        field("contents", FieldKind::Opaque, true),
        field("derived_from", FieldKind::Opaque, true),
        field("resolver", FieldKind::Opaque, true),
        field("brief", FieldKind::String, false),
        field("attempt", FieldKind::Integer, true),
        field("baseline", FieldKind::String, true),
        field("accumulator", FieldKind::String, true),
        field("candidate", FieldKind::String, true),
        field("candidate_commit", FieldKind::String, true),
        field("profile", FieldKind::String, true),
        field("requires_verification", FieldKind::Boolean, true),
    ],
    capabilities: &[],
};

/// Where a record is: its ID and the archive object holding its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRef {
    pub id: RecordId,
    pub object: ObjectDigest,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("collaboration state: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("resolution request {record}: {detail}")]
    RecordMismatch { record: String, detail: String },
    #[error(
        "asynchronous resolution attempts require schema {required} or newer (store schema {actual})"
    )]
    UnsupportedSchema { required: i64, actual: i64 },
    #[error("{path} in {snapshot} is not an input of this resolution request")]
    OutOfScopeRead { snapshot: String, path: String },
}

impl ResolutionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Compose(error) => error.code(),
            Self::Archive(error) => error.code(),
            Self::Sqlite(_) => "sqlite",
            Self::InvalidRequest(_) => "invalid_request",
            Self::RecordMismatch { .. } => "record_mismatch",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::OutOfScopeRead { .. } => "out_of_scope_read",
        }
    }
}

fn invalid(detail: impl Into<String>) -> ResolutionError {
    ResolutionError::InvalidRequest(detail.into())
}

// ------------------------------------------------------------ validation

/// Refuse a composition that `request` did not produce: an order naming
/// something not delivered, or a baseline or recipe that disagrees.
fn validate(
    store: &CollaborationStore,
    request: &CompositionRequest,
    composition: &Composition,
) -> Result<Option<String>, ResolutionError> {
    for id in &composition.order {
        if !request.deliveries.contains(id) && !request.catalog.iter().any(|s| s.id == *id) {
            return Err(invalid(format!(
                "the composition applied {id}, which the request does not know"
            )));
        }
    }
    let baseline = match &composition.outcome {
        CompositionOutcome::Candidate(candidate) => Some(&candidate.baseline),
        CompositionOutcome::Conflict { baseline, .. }
        | CompositionOutcome::NoChange { baseline } => Some(baseline),
        _ => None,
    };
    if let Some(baseline) = baseline
        && *baseline != request.baseline
    {
        return Err(invalid(format!(
            "the composition is on {}, the request on {}",
            baseline.as_str(),
            request.baseline.as_str()
        )));
    }
    let Some(candidate) = composition.outcome.candidate() else {
        return Ok(None);
    };
    let Some(recipe) = &composition.recipe else {
        return Err(invalid("a candidate without its recipe"));
    };
    let record = composer::read_recipe(store, recipe)?;
    if record.get("accepted_baseline").is_some() {
        return Err(invalid(
            "incremental accepted-baseline resolution needs its own appointed profile",
        ));
    }
    let field = |name: &str| match record.get(name) {
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    };
    if field("candidate") != candidate.subject.as_str()
        || field("candidate_commit") != candidate.commit.as_str()
        || field("baseline_commit") != request.baseline.as_str()
        || field("profile") != candidate.producer.profile()
    {
        return Err(invalid(
            "the candidate does not match its recipe: subject, commit, baseline or profile",
        ));
    }
    Ok(Some(field("profile")))
}

// --------------------------------------------------------------- manifests

/// One input as the archive retains it. `lineage` is `None` for a
/// contribution known by name only; then nothing else is known either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRef {
    pub id: String,
    pub lineage: Option<RecordId>,
    pub base: Option<SourceSnapshotId>,
    pub result: Option<SourceSnapshotId>,
}

/// The manifest of one composition.
#[derive(Debug, Clone)]
pub struct CandidateManifest {
    pub record: RecordRef,
    /// The outcome code: `candidate`, `conflict`, a refusal code, ...
    pub outcome: String,
    /// Only a candidate has one.
    pub subject: Option<SourceSnapshotId>,
    pub inputs: Vec<InputRef>,
    /// True for every candidate: composing is not verifying.
    pub requires_verification: bool,
}

/// Write the manifest of `composition`, which `request` produced in `repo`.
/// Refuses a composition that does not match `request`. `resolutions` are
/// requests already raised for it; they and any conflicts are its
/// unresolved decisions.
pub fn write_manifest(
    store: &CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    composition: &Composition,
    resolutions: &[&ResolutionRequest],
) -> Result<CandidateManifest, ResolutionError> {
    // A candidate's profile is its recipe's. Nothing else has a recipe, and
    // the provisional text profile is the only one a composer runs.
    let profile = validate(store, request, composition)?
        .unwrap_or_else(|| composer::PROVISIONAL_TEXT_PROFILE.to_string());
    let mut seen = BTreeSet::new();
    let mut inputs = Vec::new();
    for id in &request.deliveries {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let lineage = request
            .catalog
            .iter()
            .find(|spec| spec.id == *id)
            .and_then(|spec| spec.lineage.clone());
        let retained = match &lineage {
            Some(lineage) => collaboration_archive::retained_contribution(store, lineage)?,
            None => None,
        };
        inputs.push(InputRef {
            id: id.clone(),
            lineage,
            base: retained.as_ref().map(|r| r.base.snapshot_id.clone()),
            result: retained.as_ref().map(|r| r.result.snapshot_id.clone()),
        });
    }
    let mut unresolved: Vec<Value> = Vec::new();
    if let CompositionOutcome::Conflict { conflicts, .. } = &composition.outcome {
        for conflict in conflicts {
            unresolved.push(object(vec![
                ("kind", text("conflict")),
                ("path", text(&conflict.path)),
                ("reason", text(conflict.reason.code())),
                ("input", text(conflict.input.as_str())),
            ]));
        }
    }
    for resolution in resolutions {
        unresolved.push(object(vec![
            ("kind", text("resolution_request")),
            ("record", text(resolution.record().id.as_str())),
        ]));
    }
    let candidate = composition.outcome.candidate();
    let mut members = vec![
        ("schema", text(CANDIDATE_MANIFEST_SCHEMA_NAME)),
        ("outcome", text(composition.outcome.code())),
        ("profile", text(&profile)),
        ("baseline_commit", text(request.baseline.as_str())),
        (
            "inputs",
            Value::Array(inputs.iter().map(input_value).collect()),
        ),
        (
            "order",
            Value::Array(composition.order.iter().map(|id| text(id)).collect()),
        ),
        ("requires_verification", Value::Bool(candidate.is_some())),
        ("unresolved", Value::Array(unresolved)),
    ];
    // The baseline is named exactly, or the reason it could not be is.
    match collaboration_archive::snapshot_of_commit(repo, &request.baseline) {
        Ok(snapshot) => members.push(("baseline", text(snapshot.id().as_str()))),
        Err(error) => members.push(("baseline_unreadable", text(error.code()))),
    }
    if let Some(recipe) = &composition.recipe {
        members.push(("recipe", text(recipe.id.as_str())));
    }
    if let Some(candidate) = candidate {
        members.push(("subject", text(candidate.subject.as_str())));
        members.push(("candidate_commit", text(candidate.commit.as_str())));
        members.push(("mode", text(candidate.mode.code())));
    }
    match &composition.outcome {
        CompositionOutcome::Refused { detail, .. }
        | CompositionOutcome::Unsupported { detail, .. } => {
            members.push(("detail", text(detail)));
        }
        _ => {}
    }
    let record = store_record(store, &CANDIDATE_MANIFEST_SCHEMA, members)?;
    Ok(CandidateManifest {
        record,
        outcome: composition.outcome.code().to_string(),
        subject: candidate.map(|candidate| candidate.subject.clone()),
        inputs,
        requires_verification: candidate.is_some(),
    })
}

fn input_value(input: &InputRef) -> Value {
    let mut members = vec![("id", text(&input.id))];
    if let Some(lineage) = &input.lineage {
        members.push(("lineage", text(lineage.as_str())));
    }
    if let Some(base) = &input.base {
        members.push(("base", text(base.as_str())));
    }
    if let Some(result) = &input.result {
        members.push(("result", text(result.as_str())));
    }
    object(members)
}

// ----------------------------------------------------- resolution requests

/// A contribution, exactly as the archive retains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub id: String,
    pub lineage: RecordId,
    pub base: SourceSnapshotId,
    pub result: SourceSnapshotId,
    pub base_commit: CommitOid,
    pub result_commit: CommitOid,
}

impl GroupMember {
    fn pair(&self) -> String {
        format!("{}>{}", self.base.as_str(), self.result.as_str())
    }
}

/// A check that failed on a complete candidate, by an opaque identifier.
/// Deliberately nothing else: a check's own messages can carry its
/// expectations, and a resolver must not be handed the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    pub check: String,
}

/// One conflicting path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPath {
    pub path: String,
    /// The result commit whose application conflicted.
    pub input: CommitOid,
    pub reason: String,
}

/// What raised the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Conflict(Vec<ConflictPath>),
    /// The candidate composed, but these independent checks failed on it.
    FailedChecks {
        candidate: SourceSnapshotId,
        checks: Vec<FailedCheck>,
    },
}

impl Trigger {
    fn kind(&self) -> &'static str {
        match self {
            Self::Conflict(_) => "conflict",
            Self::FailedChecks { .. } => "failed_checks",
        }
    }
}

/// An independent requirement the result must meet. A requirement with a
/// `path` is harness: no resolver may write it or anything under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementRef {
    pub id: String,
    pub path: Option<String>,
}

/// An authorized choice between conflicting contributions: who decided,
/// and which contribution's version wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preference {
    pub authorized_by: String,
    pub prefer: String,
}

/// Synthesis attempts charged to a decision, of [`MAX_SYNTHESIS_ATTEMPTS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    pub limit: u32,
    pub used: u32,
}

impl Allowance {
    pub fn remaining(&self) -> u32 {
        self.limit.saturating_sub(self.used)
    }
}

/// What a decision is about, for the allowance: its kind, every
/// contribution in the selection that touches it, and, for a conflict, the
/// conflicting paths. Two requests for the same decision, whatever the
/// order, names or commits, share one allowance; so do requests for a
/// larger or smaller set of the same contributions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decision {
    kind: &'static str,
    members: BTreeSet<String>,
    paths: BTreeSet<String>,
}

impl Decision {
    fn key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"aethyme resolution decision v0\0");
        hasher.update(self.kind.as_bytes());
        for member in &self.members {
            hasher.update(b"\0m");
            hasher.update(member.as_bytes());
        }
        for path in &self.paths {
            hasher.update(b"\0p");
            hasher.update(path.as_bytes());
        }
        hex(&hasher.finalize())
    }

    /// Whether one decision's contributions and paths contain the other's.
    fn comparable(&self, members: &BTreeSet<String>, paths: &BTreeSet<String>) -> bool {
        (self.members.is_subset(members) && self.paths.is_subset(paths))
            || (members.is_subset(&self.members) && paths.is_subset(&self.paths))
    }
}

/// One bounded decision. Read-only: [`resolve`] rebuilds it from its
/// archived record, so nothing a caller holds can widen it.
#[derive(Debug, Clone)]
pub struct ResolutionRequest {
    /// Set before any request leaves this module.
    record: Option<RecordRef>,
    decision: Decision,
    decision_members: Vec<GroupMember>,
    members: Vec<GroupMember>,
    contents: Vec<GroupMember>,
    trigger: Trigger,
    baseline: RetainedSnapshot,
    accumulator: RetainedSnapshot,
    requirements: Vec<RequirementRef>,
    scope: Vec<String>,
    protection: Protection,
    preference: Option<Preference>,
    allowance: Allowance,
}

impl ResolutionRequest {
    pub fn record(&self) -> &RecordRef {
        self.record
            .as_ref()
            .expect("a request is recorded before it leaves this module")
    }
    /// The decision's allowance key.
    pub fn decision_key(&self) -> String {
        self.decision.key()
    }
    /// Canonical decision fields used by the durable managed-attempt ledger.
    pub(crate) fn managed_decision_fields(&self) -> (String, &'static str, String, String) {
        (
            self.decision.key(),
            self.decision.kind,
            join_set(&self.decision.members),
            join_set(&self.decision.paths),
        )
    }
    /// The contributions the resolver decides between: for a conflict, those
    /// applied up to and including the conflicting step that touch a
    /// conflicting path; for failed checks, every contribution.
    pub fn members(&self) -> &[GroupMember] {
        &self.members
    }
    /// Every selected contribution used to define the decision allowance.
    pub fn decision_members(&self) -> &[GroupMember] {
        &self.decision_members
    }
    /// Every contribution the accumulator already contains, in order.
    pub fn contents(&self) -> &[GroupMember] {
        &self.contents
    }
    pub fn trigger(&self) -> &Trigger {
        &self.trigger
    }
    pub fn baseline(&self) -> &RetainedSnapshot {
        &self.baseline
    }
    /// The state the proposal applies to: the composition before the
    /// conflicting step, or the failing candidate itself.
    pub fn accumulator(&self) -> &RetainedSnapshot {
        &self.accumulator
    }
    pub fn requirements(&self) -> &[RequirementRef] {
        &self.requirements
    }
    /// The paths a resolver may write: those the members' changes touch.
    pub fn scope(&self) -> &[String] {
        &self.scope
    }
    /// A description of every protected path rule.
    pub fn protected(&self) -> Vec<String> {
        self.protection.describe()
    }
    pub fn preference(&self) -> Option<&Preference> {
        self.preference.as_ref()
    }
    /// The decision's allowance when this request was loaded.
    pub fn allowance(&self) -> Allowance {
        self.allowance
    }

    /// Versioned, bounded metadata view for export. The matching input
    /// manifest is emitted separately and carries no content bytes.
    pub fn export_json(
        &self,
        status: &ResolutionAttemptStatus,
        manifest: &ResolutionInputManifest,
    ) -> Result<serde_json::Value, ResolutionError> {
        if self.record() != &status.request
            || status.request_sha256 != status.request.object.hex()
            || status.input_sha256 != resolution_input_digest(self, &status.request.object)
            || manifest.sha256 != status.input_manifest_sha256
        {
            return Err(invalid("resolution export packet binding mismatch"));
        }
        validate_manifest_scope(self, manifest)?;
        let encoded_manifest = encode_input_manifest(
            &status.request.object,
            status.limits,
            &manifest.entries,
            usize::try_from(status.limits.max_manifest_bytes)
                .map_err(|_| invalid("stored manifest byte limit"))?,
        )?;
        if ObjectDigest::of(&encoded_manifest).hex() != manifest.sha256 {
            return Err(invalid("resolution export manifest digest mismatch"));
        }
        let member_json = |member: &GroupMember| {
            serde_json::json!({
                "id": member.id,
                "lineage_record_id": member.lineage.as_str(),
                "base_snapshot": member.base.as_str(),
                "result_snapshot": member.result.as_str(),
                "base_commit": member.base_commit.as_str(),
                "result_commit": member.result_commit.as_str(),
            })
        };
        let snapshot_json = |snapshot: &RetainedSnapshot| {
            serde_json::json!({
                "snapshot": snapshot.snapshot_id.as_str(),
                "record_id": snapshot.record_id.as_str(),
                "commit": snapshot.commit.as_str(),
                "entry_count": snapshot.entry_count,
                "content_bytes": snapshot.content_bytes,
            })
        };
        let trigger = match &self.trigger {
            Trigger::Conflict(conflicts) => serde_json::json!({
                "kind": "conflict",
                "conflicts": conflicts.iter().map(|conflict| serde_json::json!({
                    "path": conflict.path,
                    "input_commit": conflict.input.as_str(),
                    "reason": conflict.reason,
                })).collect::<Vec<_>>(),
            }),
            Trigger::FailedChecks { candidate, checks } => serde_json::json!({
                "kind": "failed_checks",
                "candidate": candidate.as_str(),
                "checks": checks.iter().map(|check| serde_json::json!({"check": check.check})).collect::<Vec<_>>(),
            }),
        };
        Ok(serde_json::json!({
            "schema": "aethyme.resolution-attempt-packet/experimental-v0",
            "attempt": status.to_json(),
            "request": {
                "record_id": self.record().id.as_str(),
                "object_sha256": self.record().object.hex(),
                "decision_key": self.decision_key(),
                "trigger": trigger,
                "baseline": snapshot_json(&self.baseline),
                "accumulator": snapshot_json(&self.accumulator),
                "decision_members": self.decision_members.iter().map(member_json).collect::<Vec<_>>(),
                "members": self.members.iter().map(member_json).collect::<Vec<_>>(),
                "contents": self.contents.iter().map(member_json).collect::<Vec<_>>(),
                "requirements": self.requirements.iter().map(|requirement| serde_json::json!({
                    "id": requirement.id,
                    "path": requirement.path,
                })).collect::<Vec<_>>(),
                "scope": self.scope,
                "protected": self.protected(),
                "preference": self.preference.as_ref().map(|preference| serde_json::json!({
                    "authorized_by": preference.authorized_by,
                    "prefer": preference.prefer,
                })),
                "allowance": {
                    "limit": self.allowance.limit,
                    "used": self.allowance.used,
                    "remaining": self.allowance.remaining(),
                },
            },
            "input_manifest": {
                "sha256": manifest.sha256,
                "entries": manifest.entries.iter().map(|entry| serde_json::json!({
                    "snapshot": entry.snapshot.as_str(),
                    "path": entry.path,
                    "kind": entry.kind.map(entry_kind_name),
                    "bytes": entry.bytes,
                    "sha256": entry.sha256,
                })).collect::<Vec<_>>(),
            },
            "authority": {
                "session_label": "provenance_only",
                "dispatch": "not_authorized_by_this_packet",
                "acceptance": "not_authorized_by_this_packet",
            },
        }))
    }
}

/// Raise a resolution request for `composition`, which `request` produced
/// in `repo`: from its conflicts, or, for a candidate, from `checks` that
/// failed on it. Anything else has nothing to resolve. `preference` is
/// authorized by whoever raises the request; it cannot be added later.
pub fn request_resolution(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    composition: &Composition,
    checks: &[FailedCheck],
    requirements: &[RequirementRef],
    preference: Option<Preference>,
) -> Result<ResolutionRequest, ResolutionError> {
    validate(store, request, composition)?;
    let ordered = members_of(store, request, &composition.order)?;
    let mut changed = Vec::with_capacity(ordered.len());
    for member in &ordered {
        changed.push(changed_paths(store, member)?);
    }
    let baseline = composer::retain_commit(store, repo, &request.baseline)?;
    let (trigger, decision_members, members, contents, accumulator) = match &composition.outcome {
        CompositionOutcome::Conflict { conflicts, .. } => {
            if !checks.is_empty() {
                return Err(invalid("a conflict has no candidate for checks to fail on"));
            }
            let Some(first) = conflicts.first() else {
                return Err(invalid("a conflict outcome with no conflicts"));
            };
            let Some(step) = ordered
                .iter()
                .position(|member| member.result_commit == first.input)
            else {
                return Err(invalid(format!(
                    "conflicting input {} is not in the composition order",
                    first.input.as_str()
                )));
            };
            let paths: BTreeSet<&str> = conflicts.iter().map(|c| c.path.as_str()).collect();
            let touches = |index: usize| changed[index].iter().any(|p| paths.contains(p.as_str()));
            // The decision: every selected contribution touching a
            // conflicting path, applied yet or not.
            let decision_members: Vec<GroupMember> = (0..ordered.len())
                .filter(|&index| touches(index))
                .map(|index| ordered[index].clone())
                .collect();
            // The resolver decides between those applied so far.
            let members: Vec<GroupMember> = (0..=step)
                .filter(|&index| index == step || touches(index))
                .map(|index| ordered[index].clone())
                .collect();
            let contents = ordered[..step].to_vec();
            let accumulator = accumulator_before(store, repo, request, &composition.order, step)?
                .unwrap_or_else(|| baseline.clone());
            let trigger = Trigger::Conflict(
                conflicts
                    .iter()
                    .map(|c| ConflictPath {
                        path: c.path.clone(),
                        input: c.input.clone(),
                        reason: c.reason.code().to_string(),
                    })
                    .collect(),
            );
            (trigger, decision_members, members, contents, accumulator)
        }
        CompositionOutcome::Candidate(candidate) => {
            if checks.is_empty() {
                return Err(invalid(
                    "a candidate with no failed checks has nothing to resolve",
                ));
            }
            // Without a qualified analysis nothing narrows the interaction,
            // so the decision is every contribution in the candidate.
            let accumulator = retained_candidate(store, candidate)?;
            let trigger = Trigger::FailedChecks {
                candidate: candidate.subject.clone(),
                checks: checks.to_vec(),
            };
            (
                trigger,
                ordered.clone(),
                ordered.clone(),
                ordered.clone(),
                accumulator,
            )
        }
        other => {
            return Err(invalid(format!(
                "a {} outcome has nothing to resolve",
                other.code()
            )));
        }
    };
    let mut requirements = requirements.to_vec();
    for requirement in &mut requirements {
        if let Some(path) = &requirement.path {
            requirement.path = Some(normalize_path(path)?);
        }
    }
    let assembled = assemble(
        store,
        Parts {
            decision_members,
            members,
            contents,
            trigger,
            baseline,
            accumulator,
            requirements,
            preference,
        },
    )?;
    let record = store_record(store, &RESOLUTION_REQUEST_SCHEMA, assembled.fields())?;
    Ok(ResolutionRequest {
        record: Some(record),
        ..assembled
    })
}

struct Parts {
    decision_members: Vec<GroupMember>,
    members: Vec<GroupMember>,
    contents: Vec<GroupMember>,
    trigger: Trigger,
    baseline: RetainedSnapshot,
    accumulator: RetainedSnapshot,
    requirements: Vec<RequirementRef>,
    preference: Option<Preference>,
}

/// Everything derived from a request's parts: the decision, scope,
/// protection and allowance. The same code runs when a request is raised
/// and when [`resolve`] reloads it, so a record cannot claim more.
fn assemble(
    store: &CollaborationStore,
    parts: Parts,
) -> Result<ResolutionRequest, ResolutionError> {
    let decision_paths: BTreeSet<String> = match &parts.trigger {
        Trigger::Conflict(conflicts) => conflicts.iter().map(|c| c.path.clone()).collect(),
        Trigger::FailedChecks { .. } => BTreeSet::new(),
    };
    let decision = Decision {
        kind: parts.trigger.kind(),
        members: parts
            .decision_members
            .iter()
            .map(GroupMember::pair)
            .collect(),
        paths: decision_paths,
    };
    let mut scope = BTreeSet::new();
    for member in &parts.members {
        scope.extend(changed_paths(store, member)?);
    }
    let baseline_entries = composer::snapshot_entries(store, &parts.baseline.snapshot_id)?;
    let protection = Protection::of(store, &baseline_entries, &parts.requirements)?;
    let allowance = Allowance {
        limit: MAX_SYNTHESIS_ATTEMPTS,
        used: attempts_used(store, &decision)?,
    };
    Ok(ResolutionRequest {
        record: None,
        decision,
        decision_members: parts.decision_members,
        members: parts.members,
        contents: parts.contents,
        trigger: parts.trigger,
        baseline: parts.baseline,
        accumulator: parts.accumulator,
        requirements: parts.requirements,
        scope: scope.into_iter().collect(),
        protection,
        preference: parts.preference,
        allowance,
    })
}

impl ResolutionRequest {
    fn fields(&self) -> Vec<(&'static str, Value)> {
        let members = |list: &[GroupMember]| Value::Array(list.iter().map(member_value).collect());
        let strings = |list: &mut dyn Iterator<Item = &String>| {
            Value::Array(list.map(|item| text(item)).collect())
        };
        let trigger = match &self.trigger {
            Trigger::Conflict(conflicts) => object(vec![
                ("kind", text("conflict")),
                (
                    "conflicts",
                    Value::Array(
                        conflicts
                            .iter()
                            .map(|c| {
                                object(vec![
                                    ("path", text(&c.path)),
                                    ("reason", text(&c.reason)),
                                    ("input", text(c.input.as_str())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            Trigger::FailedChecks { candidate, checks } => object(vec![
                ("kind", text("failed_checks")),
                ("candidate", text(candidate.as_str())),
                (
                    "checks",
                    Value::Array(checks.iter().map(|c| text(&c.check)).collect()),
                ),
            ]),
        };
        let mut fields = vec![
            ("schema", text(RESOLUTION_REQUEST_SCHEMA_NAME)),
            ("kind", text(self.decision.kind)),
            ("decision", text(&self.decision.key())),
            ("decision_members", members(&self.decision_members)),
            ("decision_paths", strings(&mut self.decision.paths.iter())),
            ("members", members(&self.members)),
            ("contents", members(&self.contents)),
            ("trigger", trigger),
            ("baseline", text(self.baseline.snapshot_id.as_str())),
            ("baseline_commit", text(self.baseline.commit.as_str())),
            ("accumulator", text(self.accumulator.snapshot_id.as_str())),
            ("accumulator_commit", text(self.accumulator.commit.as_str())),
            (
                "requirements",
                Value::Array(
                    self.requirements
                        .iter()
                        .map(|r| {
                            let mut m = vec![("id", text(&r.id))];
                            if let Some(path) = &r.path {
                                m.push(("path", text(path)));
                            }
                            object(m)
                        })
                        .collect(),
                ),
            ),
            ("scope", strings(&mut self.scope.iter())),
            (
                "protected",
                Value::Array(self.protection.describe().iter().map(|p| text(p)).collect()),
            ),
        ];
        if let Some(preference) = &self.preference {
            fields.push((
                "preference",
                object(vec![
                    ("authorized_by", text(&preference.authorized_by)),
                    ("prefer", text(&preference.prefer)),
                ]),
            ));
        }
        fields
    }
}

/// Rebuild the request recorded at `record` from the archive alone: every
/// contribution it names is re-read and must match, and the decision,
/// scope and protection are recomputed and must equal what was recorded.
pub fn load_request(
    store: &mut CollaborationStore,
    repo: &Path,
    record: &RecordRef,
) -> Result<ResolutionRequest, ResolutionError> {
    let bytes = collaboration_archive::read_object(store, &record.object)?;
    load_request_from_bytes(store, Some(repo), record, &bytes)
}

pub(crate) fn load_request_bounded(
    store: &mut CollaborationStore,
    repo: &Path,
    record: &RecordRef,
    max_bytes: usize,
) -> Result<ResolutionRequest, ResolutionError> {
    let bytes = collaboration_archive::read_object_bounded(store, &record.object, max_bytes)?;
    load_request_from_bytes(store, Some(repo), record, &bytes)
}

fn load_request_from_bytes(
    store: &CollaborationStore,
    repo: Option<&Path>,
    record: &RecordRef,
    bytes: &[u8],
) -> Result<ResolutionRequest, ResolutionError> {
    let decoded = Record::decode(bytes, &[&RESOLUTION_REQUEST_SCHEMA])
        .map_err(|error| invalid(format!("resolution request record: {error}")))?;
    let mismatch = |detail: String| ResolutionError::RecordMismatch {
        record: record.id.as_str().to_string(),
        detail,
    };
    if decoded.id() != record.id || decoded.canonical_bytes() != bytes {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: record.object.hex(),
        }));
    }
    let string = |name: &str| -> Result<String, ResolutionError> {
        match decoded.get(name) {
            Some(Value::String(value)) => Ok(value.clone()),
            _ => Err(mismatch(format!("{name} is missing"))),
        }
    };
    let array = |name: &str| -> Result<Vec<Value>, ResolutionError> {
        match decoded.get(name) {
            Some(Value::Array(values)) => Ok(values.clone()),
            _ => Err(mismatch(format!("{name} is missing"))),
        }
    };
    let strings = |name: &str| -> Result<Vec<String>, ResolutionError> {
        array(name)?
            .into_iter()
            .map(|value| match value {
                Value::String(value) => Ok(value),
                _ => Err(mismatch(format!("{name} holds a non-string"))),
            })
            .collect()
    };
    let snapshot = |value: &str| {
        SourceSnapshotId::parse(value).map_err(|_| mismatch(format!("{value} is not a snapshot")))
    };
    let commit = |value: &str| {
        CommitOid::parse(value).map_err(|_| mismatch(format!("{value} is not a commit")))
    };
    let members_of_record = |name: &str| -> Result<Vec<GroupMember>, ResolutionError> {
        let mut members = Vec::new();
        for value in array(name)? {
            let Value::Object(member) = value else {
                return Err(mismatch(format!("{name} holds a non-object")));
            };
            let get = |key: &str| match member.get(key) {
                Some(Value::String(value)) => Ok(value.clone()),
                _ => Err(mismatch(format!("a member of {name} has no {key}"))),
            };
            let lineage = RecordId::parse(&get("lineage")?)
                .map_err(|_| mismatch(format!("a member of {name} has a malformed lineage")))?;
            let retained = collaboration_archive::retained_contribution(store, &lineage)?
                .ok_or_else(|| mismatch(format!("{} is no longer retained", lineage.as_str())))?;
            if retained.base.snapshot_id.as_str() != get("base")?
                || retained.result.snapshot_id.as_str() != get("result")?
            {
                return Err(mismatch(format!(
                    "{} does not match its archived lineage",
                    get("id")?
                )));
            }
            members.push(GroupMember {
                id: get("id")?,
                lineage,
                base: retained.base.snapshot_id,
                result: retained.result.snapshot_id,
                base_commit: retained.base.commit,
                result_commit: retained.result.commit,
            });
        }
        Ok(members)
    };
    let decision_members = members_of_record("decision_members")?;
    let members = members_of_record("members")?;
    let contents = members_of_record("contents")?;

    let trigger = match decoded.get("trigger") {
        Some(Value::Object(trigger)) => {
            let kind = match trigger.get("kind") {
                Some(Value::String(kind)) => kind.clone(),
                _ => String::new(),
            };
            match kind.as_str() {
                "conflict" => {
                    let Some(Value::Array(conflicts)) = trigger.get("conflicts") else {
                        return Err(mismatch("a conflict trigger without conflicts".into()));
                    };
                    let mut paths = Vec::new();
                    for conflict in conflicts {
                        let Value::Object(conflict) = conflict else {
                            return Err(mismatch("a malformed conflict".into()));
                        };
                        let get = |key: &str| match conflict.get(key) {
                            Some(Value::String(value)) => Ok(value.clone()),
                            _ => Err(mismatch(format!("a conflict has no {key}"))),
                        };
                        paths.push(ConflictPath {
                            path: get("path")?,
                            input: commit(&get("input")?)?,
                            reason: get("reason")?,
                        });
                    }
                    Trigger::Conflict(paths)
                }
                "failed_checks" => {
                    let candidate = match trigger.get("candidate") {
                        Some(Value::String(value)) => snapshot(value)?,
                        _ => return Err(mismatch("failed checks without a candidate".into())),
                    };
                    let Some(Value::Array(checks)) = trigger.get("checks") else {
                        return Err(mismatch("failed checks without checks".into()));
                    };
                    let checks = checks
                        .iter()
                        .map(|check| match check {
                            Value::String(check) => Ok(FailedCheck {
                                check: check.clone(),
                            }),
                            _ => Err(mismatch("a malformed check".into())),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Trigger::FailedChecks { candidate, checks }
                }
                other => return Err(mismatch(format!("unknown trigger kind {other:?}"))),
            }
        }
        _ => return Err(mismatch("trigger is missing".into())),
    };

    // The baseline and the accumulator, re-read and checked.
    let baseline_snapshot = snapshot(&string("baseline")?)?;
    let baseline_commit = commit(&string("baseline_commit")?)?;
    let mut baseline = collaboration_archive::retained(store, &baseline_snapshot)?
        .ok_or_else(|| mismatch("the baseline snapshot is no longer retained".into()))?;
    if let Some(repo) = repo
        && collaboration_archive::snapshot_of_commit(repo, &baseline_commit)?.id()
            != baseline_snapshot
    {
        return Err(mismatch(
            "the baseline commit no longer names its snapshot".into(),
        ));
    }
    baseline.commit = baseline_commit;
    if baseline.snapshot_id.as_str() != string("baseline")? {
        return Err(mismatch(
            "the baseline commit no longer names its snapshot".into(),
        ));
    }
    let accumulator_id = snapshot(&string("accumulator")?)?;
    let accumulator_commit = commit(&string("accumulator_commit")?)?;
    let mut retained = collaboration_archive::retained(store, &accumulator_id)?
        .ok_or_else(|| mismatch("the accumulator is no longer retained".into()))?;
    if let Some(repo) = repo
        && collaboration_archive::snapshot_of_commit(repo, &accumulator_commit)?.id()
            != accumulator_id
    {
        return Err(mismatch(
            "the accumulator commit does not name its snapshot".into(),
        ));
    }
    retained.commit = accumulator_commit;
    let accumulator = retained;
    if let Trigger::FailedChecks { candidate, .. } = &trigger
        && *candidate != accumulator.snapshot_id
    {
        return Err(mismatch(
            "the failing candidate is not the accumulator".into(),
        ));
    }

    let mut requirements = Vec::new();
    for value in array("requirements")? {
        let Value::Object(requirement) = value else {
            return Err(mismatch("a malformed requirement".into()));
        };
        let id = match requirement.get("id") {
            Some(Value::String(id)) => id.clone(),
            _ => return Err(mismatch("a requirement without an id".into())),
        };
        let path = match requirement.get("path") {
            Some(Value::String(path)) => Some(normalize_path(path)?),
            None => None,
            _ => return Err(mismatch("a malformed requirement path".into())),
        };
        requirements.push(RequirementRef { id, path });
    }
    let preference = match decoded.get("preference") {
        Some(Value::Object(preference)) => {
            let get = |key: &str| match preference.get(key) {
                Some(Value::String(value)) => Ok(value.clone()),
                _ => Err(mismatch(format!("a preference without {key}"))),
            };
            Some(Preference {
                authorized_by: get("authorized_by")?,
                prefer: get("prefer")?,
            })
        }
        None => None,
        _ => return Err(mismatch("a malformed preference".into())),
    };

    let assembled = assemble(
        store,
        Parts {
            decision_members,
            members,
            contents,
            trigger,
            baseline,
            accumulator,
            requirements,
            preference,
        },
    )?;
    if string("kind")? != assembled.decision.kind
        || string("decision")? != assembled.decision.key()
        || strings("decision_paths")?
            != assembled.decision.paths.iter().cloned().collect::<Vec<_>>()
    {
        return Err(mismatch(
            "the decision does not match its contributions".into(),
        ));
    }
    if strings("scope")? != assembled.scope {
        return Err(mismatch(
            "the scope does not match the members' changes".into(),
        ));
    }
    if strings("protected")? != assembled.protection.describe() {
        return Err(mismatch(
            "the protected paths do not match the baseline's policy".into(),
        ));
    }
    Ok(ResolutionRequest {
        record: Some(record.clone()),
        ..assembled
    })
}

fn member_value(member: &GroupMember) -> Value {
    object(vec![
        ("id", text(&member.id)),
        ("lineage", text(member.lineage.as_str())),
        ("base", text(member.base.as_str())),
        ("result", text(member.result.as_str())),
    ])
}

/// The retained members of `order`, read from the archive, not the caller.
fn members_of(
    store: &CollaborationStore,
    request: &CompositionRequest,
    order: &[String],
) -> Result<Vec<GroupMember>, ResolutionError> {
    let mut members = Vec::with_capacity(order.len());
    for id in order {
        let lineage = request
            .catalog
            .iter()
            .find(|spec| spec.id == *id)
            .and_then(|spec| spec.lineage.clone())
            .ok_or_else(|| invalid(format!("{id} has no lineage record")))?;
        let retained = collaboration_archive::retained_contribution(store, &lineage)?
            .ok_or_else(|| invalid(format!("{id} is not retained")))?;
        members.push(GroupMember {
            id: id.clone(),
            lineage,
            base: retained.base.snapshot_id,
            result: retained.result.snapshot_id,
            base_commit: retained.base.commit,
            result_commit: retained.result.commit,
        });
    }
    Ok(members)
}

fn changed_paths(
    store: &CollaborationStore,
    member: &GroupMember,
) -> Result<Vec<String>, ResolutionError> {
    let base = composer::snapshot_entries(store, &member.base)?;
    let result = composer::snapshot_entries(store, &member.result)?;
    let paths: BTreeSet<&Vec<u8>> = base
        .keys()
        .chain(result.keys())
        .filter(|path| base.get(*path) != result.get(*path))
        .collect();
    paths
        .into_iter()
        .map(|path| {
            String::from_utf8(path.clone()).map_err(|_| {
                invalid(format!(
                    "{} changes a path that is not UTF-8; this profile cannot scope it",
                    member.id
                ))
            })
        })
        .collect()
}

/// The accumulator the step at `step` was applied to: the composition of
/// the earlier steps, retained. It is an input to the decision, never
/// offered for acceptance, so atomic grouping is lifted to build it.
/// `None` when nothing came before.
fn accumulator_before(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    order: &[String],
    step: usize,
) -> Result<Option<RetainedSnapshot>, ResolutionError> {
    if step == 0 {
        return Ok(None);
    }
    let mut prefix = request.clone();
    prefix.deliveries = order[..step].to_vec();
    for spec in &mut prefix.catalog {
        spec.atomic_group = None;
    }
    let composition = composer::compose(store, repo, &prefix)?;
    match &composition.outcome {
        CompositionOutcome::Candidate(candidate) => Ok(Some(retained_candidate(store, candidate)?)),
        CompositionOutcome::NoChange { .. } => Ok(None),
        other => Err(invalid(format!(
            "rebuilding the accumulator before step {step} gave {}",
            other.code()
        ))),
    }
}

fn retained_candidate(
    store: &CollaborationStore,
    candidate: &Candidate,
) -> Result<RetainedSnapshot, ResolutionError> {
    let retained = collaboration_archive::retained(store, &candidate.subject)?
        .ok_or_else(|| invalid(format!("candidate {} is not retained", candidate.subject)))?;
    Ok(RetainedSnapshot {
        commit: candidate.commit.clone(),
        ..retained
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ----------------------------------------------------------------- paths

/// A requirement path in one spelling: no `.` or empty components, no
/// leading or trailing `/`. `..` is refused.
fn normalize_path(path: &str) -> Result<String, ResolutionError> {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(invalid(format!(
                    "requirement path {path:?} leaves the tree"
                )));
            }
            other => components.push(other),
        }
    }
    if components.is_empty() {
        return Err(invalid(format!("requirement path {path:?} names nothing")));
    }
    Ok(components.join("/"))
}

/// One path component as a case- and normalization-insensitive file
/// system sees it: NFC, without HFS+ ignorable code points, without an NTFS
/// stream suffix or trailing dots and spaces, lowercased.
fn fold_component(component: &str) -> String {
    let composed: String = component
        .nfc()
        .filter(|c| {
            !matches!(
                c,
                '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}'
            )
        })
        .collect();
    let stem = composed.split(':').next().unwrap_or_default();
    stem.trim_end_matches(['.', ' ']).to_lowercase()
}

fn fold_path(path: &str) -> String {
    path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .map(fold_component)
        .collect::<Vec<_>>()
        .join("/")
}

/// The paths no resolver may write.
#[derive(Debug, Clone)]
struct Protection {
    /// Requirement harness paths, folded; everything under them too.
    harness: BTreeSet<String>,
    /// Files a gate command names, folded.
    gate_files: BTreeSet<String>,
    /// The security review rule's globs, as written.
    security_patterns: Vec<String>,
    security: Option<GlobSet>,
}

impl Protection {
    /// The fixed rules, the requirements' harness paths, and the policy the
    /// baseline itself carries: the files its gate commands name and the
    /// paths its security review rule lists.
    fn of(
        store: &CollaborationStore,
        baseline: &Entries,
        requirements: &[RequirementRef],
    ) -> Result<Self, ResolutionError> {
        let harness = requirements
            .iter()
            .filter_map(|r| r.path.as_deref())
            .map(fold_path)
            .collect();
        let read = |path: &str| -> Result<Option<String>, ResolutionError> {
            match baseline.get(path.as_bytes()) {
                Some((_, digest)) => Ok(Some(
                    String::from_utf8_lossy(&composer::blob(store, digest)?).into_owned(),
                )),
                None => Ok(None),
            }
        };
        let mut gate_files = BTreeSet::new();
        // Either format (#1223); an ambiguous or invalid file contributes nothing,
        // exactly like an unparseable one.
        let config_toml = |stem: &str| -> Result<Option<String>, ResolutionError> {
            Ok(aethyme_config::try_read_config_text_with(
                aethyme_config::CONFIG_DIR,
                stem,
                |relpath| read(relpath),
            )?
            .ok()
            .flatten()
            .and_then(|text| text.toml_text().ok().map(|toml| toml.into_owned())))
        };
        if let Some(gates) = config_toml("gates")?
            && let Ok(gates) = crate::gates::parse_gates(&gates)
        {
            for gate in gates {
                for token in gate.command.split(|c: char| {
                    c.is_whitespace() || matches!(c, '\'' | '"' | '=' | ';' | '&' | '|' | '(' | ')')
                }) {
                    if let Ok(path) = normalize_path(token)
                        && baseline.contains_key(path.as_bytes())
                    {
                        gate_files.insert(fold_path(&path));
                    }
                }
            }
        }
        let mut security_patterns = Vec::new();
        if let Some(config) = config_toml("config")?
            && let Ok(config) = config.parse::<toml::Table>()
            && let Some(rules) = config
                .get("review")
                .and_then(|review| review.get("trigger"))
                .and_then(|trigger| trigger.get("rule"))
                .and_then(|rules| rules.as_array())
        {
            for rule in rules {
                let security = rule
                    .get("require")
                    .and_then(|require| require.as_array())
                    .is_some_and(|require| {
                        require
                            .iter()
                            .any(|r| r.as_str() == Some(SECURITY_REQUIREMENT))
                    });
                if !security {
                    continue;
                }
                for path in rule
                    .get("paths")
                    .and_then(|paths| paths.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|path| path.as_str())
                {
                    security_patterns.push(path.to_string());
                }
            }
        }
        security_patterns.sort();
        security_patterns.dedup();
        let security = if security_patterns.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &security_patterns {
                let glob = GlobBuilder::new(&fold_path(pattern))
                    .literal_separator(true)
                    .case_insensitive(true)
                    .build()
                    .map_err(|error| invalid(format!("security path {pattern:?}: {error}")))?;
                builder.add(glob);
            }
            Some(
                builder
                    .build()
                    .map_err(|error| invalid(format!("security paths: {error}")))?,
            )
        };
        Ok(Self {
            harness,
            gate_files,
            security_patterns,
            security,
        })
    }

    /// Why `path` is protected, if it is.
    fn reason(&self, path: &str) -> Option<String> {
        let folded = fold_path(path);
        let components: Vec<&str> = folded.split('/').collect();
        if let Some(directory) = components[..components.len().saturating_sub(1)]
            .iter()
            .find(|c| PROTECTED_DIRECTORIES.contains(c))
        {
            return Some(format!("{path} is under protected {directory}/"));
        }
        if let Some(name) = components.last()
            && (PROTECTED_FILES.contains(name) || PROTECTED_DIRECTORIES.contains(name))
        {
            return Some(format!("{path} is a protected {name}"));
        }
        if let Some(harness) = self
            .harness
            .iter()
            .find(|h| folded == **h || folded.starts_with(&format!("{h}/")))
        {
            return Some(format!("{path} is requirement harness ({harness})"));
        }
        if self.gate_files.contains(&folded) {
            return Some(format!("{path} is named by a gate command"));
        }
        if self
            .security
            .as_ref()
            .is_some_and(|set| set.is_match(&folded))
        {
            return Some(format!("{path} is on the security review list"));
        }
        None
    }

    fn describe(&self) -> Vec<String> {
        let mut rules: Vec<String> = PROTECTED_DIRECTORIES
            .iter()
            .map(|d| format!("dir:{d}"))
            .chain(PROTECTED_FILES.iter().map(|f| format!("file:{f}")))
            .chain(self.harness.iter().map(|h| format!("harness:{h}")))
            .chain(self.gate_files.iter().map(|g| format!("gate:{g}")))
            .chain(
                self.security_patterns
                    .iter()
                    .map(|s| format!("security:{s}")),
            )
            .collect();
        rules.sort();
        rules
    }
}

// ----------------------------------------------------------- the allowance

/// Idempotent initialization for existing callers. Collaboration schema 7
/// adopts this ledger without resetting already charged attempts (#666).
const LEDGER: &str = "CREATE TABLE IF NOT EXISTS resolution_attempts (
     attempt_id INTEGER PRIMARY KEY,
     decision_key TEXT NOT NULL,
     kind TEXT NOT NULL,
     members TEXT NOT NULL,
     paths TEXT NOT NULL,
     request_record_id TEXT NOT NULL,
     resolver TEXT NOT NULL,
     outcome TEXT,
     charged_ms INTEGER NOT NULL
 ) STRICT;";

fn split_set(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn join_set(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join("\n")
}

/// Attempts charged to `decision`, or to any decision of its kind whose
/// contributions and paths contain or are contained in its own.
fn comparable_attempts(
    connection: &rusqlite::Connection,
    decision: &Decision,
) -> Result<u32, ResolutionError> {
    let mut statement =
        connection.prepare("SELECT members, paths FROM resolution_attempts WHERE kind = ?1")?;
    let rows = statement.query_map([decision.kind], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0u32;
    for row in rows {
        let (members, paths) = row?;
        if decision.comparable(&split_set(&members), &split_set(&paths)) {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

/// Whether one stored managed-attempt row is the exact canonical decision
/// represented by this already validated request.
#[cfg(test)]
fn managed_attempt_decision_matches(
    request: &ResolutionRequest,
    key: &str,
    kind: &str,
    members: &str,
    paths: &str,
) -> bool {
    request.decision.key() == key
        && request.decision.kind == kind
        && join_set(&request.decision.members) == members
        && join_set(&request.decision.paths) == paths
}

fn decode_canonical_decision_set(value: &str) -> Result<BTreeSet<String>, ResolutionError> {
    if value.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(
            "managed attempt decision set exceeds the size limit",
        ));
    }
    let set = split_set(value);
    if join_set(&set) != value {
        return Err(invalid("managed attempt decision set is not canonical"));
    }
    Ok(set)
}

/// Whether an unresolved async managed attempt reserves a comparable
/// contribution/path decision. The durable key is rederived from its bounded,
/// canonical member/path sets before it can block admission.
pub(crate) fn has_unresolved_managed_comparable_in_transaction(
    connection: &rusqlite::Connection,
    request: &ResolutionRequest,
) -> Result<bool, ResolutionError> {
    let mut statement = connection.prepare(
        "SELECT decision_key,members,paths FROM resolution_attempts
         WHERE kind=?1 AND resolver='local-managed-resolution'
           AND session_label IS NULL AND outcome IS NULL
           AND async_state IN ('dispatching','outcome_unknown')
         ORDER BY attempt_id",
    )?;
    let rows = statement.query_map([request.decision.kind], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (key, raw_members, raw_paths) = row?;
        let members = decode_canonical_decision_set(&raw_members)?;
        let paths = decode_canonical_decision_set(&raw_paths)?;
        let stored = Decision {
            kind: request.decision.kind,
            members,
            paths,
        };
        if stored.key() != key {
            return Err(invalid(
                "unresolved managed attempt decision key does not match",
            ));
        }
        if request.decision.comparable(&stored.members, &stored.paths) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn attempts_used(store: &CollaborationStore, decision: &Decision) -> Result<u32, ResolutionError> {
    let connection = store.read_connection();
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'resolution_attempts'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(0);
    }
    comparable_attempts(connection, decision)
}

/// Insert the comparable-decision charge into an already-open transaction.
/// Callers supply the archived request, not a member/path summary, so the
/// decision key and allowance query always come from the validated record.
fn charge_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    request: &ResolutionRequest,
    record: &RecordRef,
    resolver: &str,
) -> Result<Option<(u32, i64)>, ResolutionError> {
    charge_in_transaction_with_metadata(transaction, request, record, resolver, None)
}

/// The managed route must preserve the exact manifest and stable idempotency
/// key on the same shared comparable-attempt row used by manual resolution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ManagedChargeContext<'a> {
    pub(crate) input_manifest: ObjectDigest,
    pub(crate) client_key: &'a OperationId,
    pub(crate) async_token: &'a OperationId,
    pub(crate) input_sha256: &'a str,
    pub(crate) limits: DispatchLimits,
}

#[derive(Debug)]
struct AttemptChargeMetadata {
    request_sha256: String,
    input_sha256: String,
    input_manifest_sha256: String,
    client_key: String,
    async_token: String,
    dispatch_limits: String,
}

/// Insert one managed charge through the same decision/cap check as manual
/// resolution. Managed rows begin as exported; the admission caller advances
/// them to dispatching after it writes the reservation and its bindings. The
/// caller owns the Immediate transaction and initializes the resolution
/// ledger before invoking this helper.
pub(crate) fn charge_managed_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    request: &ResolutionRequest,
    context: ManagedChargeContext<'_>,
) -> Result<Option<(u32, i64)>, ResolutionError> {
    validate_attempt_binding(context.client_key.as_str(), "client key")?;
    validate_attempt_binding(context.async_token.as_str(), "attempt token")?;
    let limits = context.limits.validate()?;
    let record = request.record();
    let metadata = AttemptChargeMetadata {
        request_sha256: record.object.hex(),
        input_sha256: context.input_sha256.to_owned(),
        input_manifest_sha256: context.input_manifest.hex(),
        client_key: context.client_key.as_str().to_owned(),
        async_token: context.async_token.as_str().to_owned(),
        dispatch_limits: limits.canonical_ledger_value(),
    };
    charge_in_transaction_with_metadata(
        transaction,
        request,
        record,
        "local-managed-resolution",
        Some(&metadata),
    )
}

fn charge_in_transaction_with_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &ResolutionRequest,
    record: &RecordRef,
    resolver: &str,
    metadata: Option<&AttemptChargeMetadata>,
) -> Result<Option<(u32, i64)>, ResolutionError> {
    if request.record() != record {
        return Err(invalid(
            "charged request reference differs from its archived record",
        ));
    }
    let decision = &request.decision;
    let used = comparable_attempts(transaction, decision)?;
    if used >= MAX_SYNTHESIS_ATTEMPTS {
        return Ok(None);
    }
    let request_sha256 = metadata.map(|metadata| metadata.request_sha256.as_str());
    let input_sha256 = metadata.map(|metadata| metadata.input_sha256.as_str());
    let input_manifest_sha256 = metadata.map(|metadata| metadata.input_manifest_sha256.as_str());
    let client_key = metadata.map(|metadata| metadata.client_key.as_str());
    let async_token = metadata.map(|metadata| metadata.async_token.as_str());
    let dispatch_limits = metadata.map(|metadata| metadata.dispatch_limits.as_str());
    transaction.execute(
        "INSERT INTO resolution_attempts
             (decision_key, kind, members, paths, request_record_id, resolver, charged_ms,
              attempt_number, request_sha256, input_sha256, input_manifest_sha256,
              client_key, async_token, dispatch_limits, async_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            decision.key(),
            decision.kind,
            join_set(&decision.members),
            join_set(&decision.paths),
            record.id.as_str(),
            resolver,
            now_ms(),
            i64::from(used + 1),
            request_sha256,
            input_sha256,
            input_manifest_sha256,
            client_key,
            async_token,
            dispatch_limits,
            if metadata.is_some() {
                "exported"
            } else {
                "dispatching"
            },
        ],
    )?;
    let row = transaction.last_insert_rowid();
    Ok(Some((used + 1, row)))
}

/// Charge one attempt before dispatch, so a crash never refunds it. `None`
/// means the shared allowance is spent; otherwise return its number and the
/// ledger row to settle.
fn charge(
    store: &mut CollaborationStore,
    request: &ResolutionRequest,
    record: &RecordRef,
    resolver: &str,
) -> Result<Option<(u32, i64)>, ResolutionError> {
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    initialize_ledger_in_transaction(&transaction)?;
    let charge = charge_in_transaction(&transaction, request, record, resolver)?;
    transaction.commit()?;
    Ok(charge)
}

/// Initialize the shared resolution-attempt ledger inside a caller-owned
/// transaction. Managed Local admission uses this with its reservation and
/// immutable binding writes so all authority commits atomically.
pub(crate) fn initialize_ledger_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), ResolutionError> {
    transaction.execute_batch(LEDGER)?;
    Ok(())
}

fn settle(store: &mut CollaborationStore, row: i64, outcome: &str) -> Result<(), ResolutionError> {
    store.connection().execute(
        "UPDATE resolution_attempts SET outcome = ?2, async_state='settled'
         WHERE attempt_id = ?1",
        rusqlite::params![row, outcome],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ---------------------------------------------------------------- resolving

/// Who resolved: carried into the synthesized group record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverIdentity {
    pub name: String,
    pub version: String,
}

/// One file a proposal writes: its kind and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedFile {
    pub kind: EntryKind,
    pub bytes: Vec<u8>,
}

/// A resolver's proposed source: whole files by path, `None` to delete.
/// The proposal is not trusted: everything is checked before it is used.
#[derive(Debug, Clone, Default)]
pub struct Proposal {
    pub writes: BTreeMap<String, Option<ProposedFile>>,
    /// The resolver's compact decision brief (an archive record or digest).
    pub brief: Option<String>,
}

/// What a resolver returned.
#[derive(Debug, Clone)]
pub enum Response {
    Proposal(Proposal),
    /// The resolver found no acceptable resolution.
    Unresolved(String),
    /// The resolver could not tell.
    Inconclusive(String),
}

/// The narrow interface of a bounded resolver.
pub trait Resolver {
    fn identity(&self) -> ResolverIdentity;
    /// Propose a resolution. `Err` is an infrastructure failure: the
    /// attempt is still charged and the outcome is `infrastructure_deferred`.
    fn propose(
        &mut self,
        request: &ResolutionRequest,
        inputs: &ResolutionInputs<'_>,
    ) -> Result<Response, String>;
}

/// Read access to exactly a request's inputs: the scoped paths of its
/// baseline, accumulator, members' and contents' snapshots.
pub struct ResolutionInputs<'a> {
    store: &'a CollaborationStore,
    request: &'a ResolutionRequest,
}

impl ResolutionInputs<'_> {
    /// `path` in `snapshot`, or `None` when that snapshot has no such path.
    pub fn read(
        &self,
        snapshot: &SourceSnapshotId,
        path: &str,
    ) -> Result<Option<ProposedFile>, ResolutionError> {
        let request = self.request;
        let known = *snapshot == request.baseline.snapshot_id
            || *snapshot == request.accumulator.snapshot_id
            || request
                .members
                .iter()
                .chain(&request.contents)
                .any(|m| m.base == *snapshot || m.result == *snapshot);
        if !known || !request.scope.iter().any(|p| p == path) {
            return Err(ResolutionError::OutOfScopeRead {
                snapshot: snapshot.to_string(),
                path: path.to_string(),
            });
        }
        let entries = composer::snapshot_entries(self.store, snapshot)?;
        match entries.get(path.as_bytes()) {
            Some((kind, digest)) => Ok(Some(ProposedFile {
                kind: *kind,
                bytes: composer::blob(self.store, digest)?,
            })),
            None => Ok(None),
        }
    }
}

/// How a resolution failed (§7.5). None is success with reduced
/// requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionFailure {
    Unresolved,
    Inconclusive,
    ScopeViolation,
    InfrastructureDeferred,
    BudgetExhausted,
}

impl ResolutionFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Inconclusive => "inconclusive",
            Self::ScopeViolation => "scope_violation",
            Self::InfrastructureDeferred => "infrastructure_deferred",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

/// An accepted proposal, materialized as a new candidate.
#[derive(Debug, Clone)]
pub struct SynthesizedCandidate {
    /// Produced by [`Producer::Resolver`]; unverified like every candidate.
    pub candidate: Candidate,
    pub group_record: RecordRef,
}

#[derive(Debug, Clone)]
pub enum ResolutionOutcome {
    Synthesized(Box<SynthesizedCandidate>),
    Failed {
        state: ResolutionFailure,
        detail: String,
    },
}

impl ResolutionOutcome {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Synthesized(_) => "synthesized",
            Self::Failed { state, .. } => state.code(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub outcome: ResolutionOutcome,
    /// The attempt charged, or `None` when the allowance was already spent.
    pub attempt: Option<u32>,
    /// The decision's allowance after this attempt.
    pub allowance: Allowance,
}

/// Durable state of one asynchronous, session-bound synthesis attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionAttemptState {
    /// The attempt has been charged and its exact request exported. A caller
    /// must use a separate pre-dispatch transition before external dispatch.
    Exported,
    /// The attempt is marked for external dispatch. A caller must not dispatch
    /// it again; a missing response remains unknown until imported.
    Dispatching,
    /// The caller recorded that delivery or completion is uncertain. A late
    /// response for this same token may still be imported exactly once.
    OutcomeUnknown,
    /// One response digest is fixed and is being validated/materialized.
    Importing,
    /// The response was settled as a failure or an unverified candidate.
    Settled,
}

impl ResolutionAttemptState {
    pub fn code(self) -> &'static str {
        match self {
            Self::Exported => "exported",
            Self::Dispatching => "dispatching",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Importing => "importing",
            Self::Settled => "settled",
        }
    }

    fn parse(text: &str) -> Result<Self, ResolutionError> {
        match text {
            "exported" => Ok(Self::Exported),
            "dispatching" => Ok(Self::Dispatching),
            "outcome_unknown" => Ok(Self::OutcomeUnknown),
            "importing" => Ok(Self::Importing),
            "settled" => Ok(Self::Settled),
            _ => Err(invalid(format!(
                "unknown asynchronous attempt state {text}"
            ))),
        }
    }
}

/// Caller-supplied limits frozen into one async attempt before any request
/// leaves the broker. These bound metadata and exact scoped inputs; they are
/// not provider-token or paid-dispatch policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchLimits {
    pub max_manifest_bytes: u64,
    pub max_manifest_entries: u64,
    pub max_file_bytes: u64,
    pub max_total_input_bytes: u64,
    pub max_response_bytes: u64,
}

impl DispatchLimits {
    pub fn validate(self) -> Result<Self, ResolutionError> {
        if self.max_manifest_bytes == 0
            || self.max_manifest_bytes > collaboration_archive::MAX_METADATA_OBJECT_BYTES
            || self.max_manifest_entries == 0
            || self.max_manifest_entries > crate::execution_snapshot::MAX_FILES as u64
            || self.max_file_bytes == 0
            || self.max_file_bytes > crate::execution_snapshot::MAX_FILE_BYTES
            || self.max_total_input_bytes == 0
            || self.max_total_input_bytes > crate::execution_snapshot::MAX_INPUT_BYTES
            || self.max_response_bytes == 0
            || self.max_response_bytes > MAX_ASYNC_RESPONSE_BYTES as u64
            || usize::try_from(self.max_manifest_bytes)
                .ok()
                .and_then(|bytes| bytes.checked_add(1))
                .is_none()
            || usize::try_from(self.max_file_bytes)
                .ok()
                .and_then(|bytes| bytes.checked_add(1))
                .is_none()
        {
            return Err(invalid("dispatch size limits"));
        }
        Ok(self)
    }

    /// Canonical ledger encoding. A caller can use JSON for CLI input; this
    /// stable representation is the exact value persisted with the charge.
    pub fn canonical_ledger_value(self) -> String {
        format!(
            "v1:{}:{}:{}:{}:{}",
            self.max_manifest_bytes,
            self.max_manifest_entries,
            self.max_file_bytes,
            self.max_total_input_bytes,
            self.max_response_bytes
        )
    }

    pub fn from_canonical_ledger_value(value: &str) -> Result<Self, ResolutionError> {
        let mut fields = value.split(':');
        if fields.next() != Some("v1") {
            return Err(invalid("stored dispatch limits version"));
        }
        let mut next = || {
            fields
                .next()
                .ok_or_else(|| invalid("stored dispatch limits"))?
                .parse::<u64>()
                .map_err(|_| invalid("stored dispatch limits"))
        };
        let limits = Self {
            max_manifest_bytes: next()?,
            max_manifest_entries: next()?,
            max_file_bytes: next()?,
            max_total_input_bytes: next()?,
            max_response_bytes: next()?,
        }
        .validate()?;
        if fields.next().is_some() || limits.canonical_ledger_value() != value {
            return Err(invalid("stored dispatch limits are not canonical"));
        }
        Ok(limits)
    }
}

pub(crate) fn request_record_limit(limits: DispatchLimits) -> Result<usize, ResolutionError> {
    usize::try_from(limits.max_manifest_bytes.min(MAX_DOCUMENT_BYTES as u64))
        .map_err(|_| invalid("resolution request byte limit"))
}

/// One exact `(snapshot, path)` exposed to an external session. Missing paths
/// are explicit entries; a present file carries its archived size and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputManifestEntry {
    pub snapshot: SourceSnapshotId,
    pub path: String,
    pub kind: Option<EntryKind>,
    pub bytes: u64,
    pub sha256: Option<String>,
}

/// Durable bounded input manifest returned with a newly started attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionInputManifest {
    pub sha256: String,
    pub entries: Vec<InputManifestEntry>,
}

/// Recoverable view of one charged attempt. The request object and input
/// digest bind the session to the exact archived request and its retained
/// source snapshots; they are never reconstructed from caller-held data.
#[derive(Debug, Clone)]
pub struct ResolutionAttemptStatus {
    pub token: String,
    /// Provenance label only; this low-level ledger does not grant authority.
    pub session_label: String,
    pub attempt: u32,
    pub request: RecordRef,
    pub request_sha256: String,
    pub input_sha256: String,
    pub input_manifest_sha256: String,
    pub limits: DispatchLimits,
    pub response_sha256: Option<String>,
    pub state: ResolutionAttemptState,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub result: Option<RecordRef>,
}

/// A settled resolver result reconstructed from its durable ledger and
/// canonical group record. The candidate is still unverified and grants no
/// authority to skip the controller's fresh gates.
#[derive(Debug, Clone)]
pub struct UnverifiedSynthesis {
    pub status: ResolutionAttemptStatus,
    pub request: ResolutionRequest,
    pub candidate: Candidate,
    pub group_record: RecordRef,
    pub input_manifest: ResolutionInputManifest,
}

/// Historical provenance for an accepted synthesized source. It validates
/// archive and ledger bindings without a repository, provider query, or
/// authority grant. The candidate remains unverified; callers must bind it
/// to the exact accepted completion and retained Local selection.
#[derive(Debug, Clone)]
pub(crate) struct RetainedSynthesisProof {
    pub status: ResolutionAttemptStatus,
    pub request: ResolutionRequest,
    pub group_record: RecordRef,
    pub candidate_subject: SourceSnapshotId,
    pub candidate_commit: CommitOid,
    pub input_manifest: ResolutionInputManifest,
}

impl ResolutionAttemptStatus {
    /// Public, read-only status shape. `session_label` is explicitly
    /// provenance and never authenticates a caller or agent.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": "aethyme.resolution-attempt-status/experimental-v0",
            "token": self.token,
            "session_label": self.session_label,
            "session_label_kind": "provenance_only",
            "attempt": self.attempt,
            "request": {
                "record_id": self.request.id.as_str(),
                "object_sha256": self.request.object.hex(),
            },
            "request_sha256": self.request_sha256,
            "input_sha256": self.input_sha256,
            "input_manifest_sha256": self.input_manifest_sha256,
            "limits": self.limits,
            "response_sha256": self.response_sha256,
            "state": self.state.code(),
            "outcome": self.outcome,
            "detail": self.detail,
            "result": self.result.as_ref().map(|record| serde_json::json!({
                "record_id": record.id.as_str(),
                "object_sha256": record.object.hex(),
            })),
        })
    }
}

/// Result of beginning with a caller-stable idempotency key. Only `Started`
/// carries the request for explicit operator-mediated export. Repeating a key
/// returns `Existing` with status only, so it cannot redispatch the attempt.
#[derive(Debug, Clone)]
pub enum BeginResolutionAttempt {
    Started {
        status: ResolutionAttemptStatus,
        request: Box<ResolutionRequest>,
        input_manifest: ResolutionInputManifest,
    },
    Existing(ResolutionAttemptStatus),
    BudgetExhausted(Allowance),
}

type ExistingAttemptRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// Begin one charged asynchronous attempt. `session_label` is provenance only;
/// current-wave eligibility and agent appointment must be proved by a future
/// controller caller. This API exports no live authority or dispatch, and it
/// cannot prevent a caller from sharing a separately known request beforehand.
/// A newly prepared payload is returned only after the charge and its retained
/// roots commit. Source inputs and audit evidence remain rooted indefinitely
/// in schema 27; a later explicit release/reconciliation lifecycle must prove
/// no late import is possible before those roots can be released. `client_key`
/// makes command retries idempotent.
pub fn begin_attempt(
    store: &mut CollaborationStore,
    repo: &Path,
    record: &RecordRef,
    session_label: &str,
    client_key: &str,
    limits: DispatchLimits,
) -> Result<BeginResolutionAttempt, ResolutionError> {
    validate_attempt_binding(session_label, "session label")?;
    validate_attempt_binding(client_key, "client key")?;
    let limits = limits.validate()?;
    let _archive = crate::collaboration_gc::archive_use(store)
        .map_err(|error| invalid(format!("archive retention: {error}")))?;
    let request = load_request_bounded(store, repo, record, request_record_limit(limits)?)?;
    let request_sha256 = record.object.hex();
    let input_sha256 = resolution_input_digest(&request, &record.object);
    let (manifest_bytes, input_manifest) = build_input_manifest(store, &request, limits)?;
    let encoded_limits = limits.canonical_ledger_value();
    let manifest_digest = collaboration_archive::put_object(store, &manifest_bytes)?;
    if manifest_digest.hex() != input_manifest.sha256 {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: input_manifest.sha256,
        }));
    }

    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<ExistingAttemptRow> = transaction
        .query_row(
            "SELECT async_token, request_record_id, request_sha256, input_sha256,
                    input_manifest_sha256, dispatch_limits
             FROM resolution_attempts WHERE session_label=?1 AND client_key=?2",
            rusqlite::params![session_label, client_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    if let Some((
        token,
        existing_request,
        existing_sha,
        existing_input_sha,
        existing_manifest_sha,
        existing_limits,
    )) = existing
    {
        if token.is_empty()
            || existing_request != record.id.as_str()
            || existing_sha != request_sha256
            || existing_input_sha != input_sha256
            || existing_manifest_sha.as_deref() != Some(input_manifest.sha256.as_str())
            || existing_limits.as_deref() != Some(encoded_limits.as_str())
        {
            return Err(invalid("the idempotency key belongs to another request"));
        }
        let status = attempt_status_conn(&transaction, &token, session_label)?;
        transaction.commit()?;
        return Ok(BeginResolutionAttempt::Existing(status));
    }

    let used = comparable_attempts(&transaction, &request.decision)?;
    if used >= MAX_SYNTHESIS_ATTEMPTS {
        transaction.commit()?;
        return Ok(BeginResolutionAttempt::BudgetExhausted(Allowance {
            limit: MAX_SYNTHESIS_ATTEMPTS,
            used,
        }));
    }

    let token = OperationId::mint()
        .map_err(|error| invalid(format!("could not mint resolution token: {error}")))?
        .as_str()
        .to_owned();
    transaction.execute(
        "INSERT INTO resolution_attempts
             (decision_key, kind, members, paths, request_record_id, resolver,
              charged_ms, async_token, client_key, session_label, attempt_number,
              request_sha256, input_sha256, input_manifest_sha256, dispatch_limits,
              async_state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'external-session-label', ?6, ?7, ?8, ?9,
                 ?10, ?11, ?12, ?13, ?14, 'exported')",
        rusqlite::params![
            request.decision.key(),
            request.decision.kind,
            join_set(&request.decision.members),
            join_set(&request.decision.paths),
            record.id.as_str(),
            now_ms(),
            token,
            client_key,
            session_label,
            i64::from(used + 1),
            request_sha256,
            input_sha256,
            input_manifest.sha256,
            encoded_limits,
        ],
    )?;
    retain_attempt_inputs(&transaction, &token, record, &request)?;
    transaction.execute(
        "INSERT INTO object_pins(class,object_sha256,holder,until_ms,created_ms)
         VALUES('cited_evidence',?1,?2,NULL,?3)",
        rusqlite::params![
            manifest_digest.hex(),
            format!("resolution:{token}"),
            now_ms()
        ],
    )?;
    transaction.commit()?;

    let status = attempt_status_conn(store.read_connection(), &token, session_label)?;
    Ok(BeginResolutionAttempt::Started {
        status,
        request: Box::new(request),
        input_manifest,
    })
}

/// Return the durable state of an attempt, scoped to the session that began it.
pub fn attempt_status(
    store: &CollaborationStore,
    token: &str,
    session_label: &str,
) -> Result<ResolutionAttemptStatus, ResolutionError> {
    attempt_status_from_connection(store.read_connection(), token, session_label)
}

/// Read an attempt from a lifecycle snapshot without opening a writer,
/// applying migrations, or creating collaboration state.
pub(crate) fn attempt_status_from_connection(
    connection: &rusqlite::Connection,
    token: &str,
    session_label: &str,
) -> Result<ResolutionAttemptStatus, ResolutionError> {
    validate_attempt_binding(token, "attempt token")?;
    validate_attempt_binding(session_label, "session label")?;
    let actual: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let actual = actual
        .and_then(|version| version.parse::<i64>().ok())
        .unwrap_or_default();
    if actual < ASYNC_ATTEMPT_SCHEMA_VERSION {
        return Err(ResolutionError::UnsupportedSchema {
            required: ASYNC_ATTEMPT_SCHEMA_VERSION,
            actual,
        });
    }
    attempt_status_conn(connection, token, session_label)
}

/// Rebuild a packet only while the durable attempt remains exported. This
/// recovers a caller that crashed before dispatch while preventing a request
/// from being reissued after dispatch intent was recorded.
pub fn exported_attempt_packet(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    session_label: &str,
) -> Result<(ResolutionRequest, ResolutionInputManifest), ResolutionError> {
    let status = attempt_status(store, token, session_label)?;
    if status.state != ResolutionAttemptState::Exported {
        return Err(invalid("attempt is no longer exportable"));
    }
    let request = load_request_bounded(
        store,
        repo,
        &status.request,
        request_record_limit(status.limits)?,
    )?;
    verify_attempt_inputs(&status, &request)?;
    let manifest = load_attempt_manifest(store, &status, &request)?;
    Ok((request, manifest))
}

/// Record operator dispatch intent before the caller hands the exported
/// request to an external agent. This does not dispatch, prove appointment, or
/// authenticate `session_label`. A repeated call or process restart never
/// returns the payload again.
pub fn mark_attempt_dispatching(
    store: &mut CollaborationStore,
    token: &str,
    session_label: &str,
) -> Result<ResolutionAttemptStatus, ResolutionError> {
    validate_attempt_binding(token, "attempt token")?;
    validate_attempt_binding(session_label, "session label")?;
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = attempt_status_conn(&transaction, token, session_label)?;
    if current.state != ResolutionAttemptState::Exported {
        return Err(invalid("attempt is not in the exported state"));
    }
    transaction.execute(
        "UPDATE resolution_attempts SET async_state='dispatching'
         WHERE async_token=?1 AND session_label=?2 AND async_state='exported'",
        rusqlite::params![token, session_label],
    )?;
    let status = attempt_status_conn(&transaction, token, session_label)?;
    transaction.commit()?;
    Ok(status)
}

/// Mark delivery/completion as uncertain without clearing the charge or
/// permitting another dispatch. Importing a late response remains possible.
pub fn mark_attempt_unknown(
    store: &mut CollaborationStore,
    token: &str,
    session_label: &str,
    detail: &str,
) -> Result<ResolutionAttemptStatus, ResolutionError> {
    validate_attempt_binding(token, "attempt token")?;
    validate_attempt_binding(session_label, "session label")?;
    if detail.trim().is_empty() || detail.len() > 4096 {
        return Err(invalid("unknown-attempt detail"));
    }
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = attempt_status_conn(&transaction, token, session_label)?;
    match current.state {
        ResolutionAttemptState::Exported | ResolutionAttemptState::Dispatching => {
            transaction.execute(
                "UPDATE resolution_attempts SET async_state='outcome_unknown', detail=?2
                 WHERE async_token=?1 AND session_label=?3
                   AND async_state IN ('exported','dispatching')",
                rusqlite::params![token, detail, session_label],
            )?;
        }
        ResolutionAttemptState::OutcomeUnknown if current.detail.as_deref() == Some(detail) => {}
        _ => return Err(invalid("attempt is no longer exportable or dispatching")),
    }
    let status = attempt_status_conn(&transaction, token, session_label)?;
    transaction.commit()?;
    Ok(status)
}

/// Read one scoped file from the exact request attached to a live attempt.
/// The request is reloaded and its scope/protection is recomputed each time.
pub fn read_attempt_input(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    session_label: &str,
    snapshot: &SourceSnapshotId,
    path: &str,
) -> Result<Option<ProposedFile>, ResolutionError> {
    let status = attempt_status(store, token, session_label)?;
    if status.state == ResolutionAttemptState::Settled {
        return Err(invalid("attempt inputs are closed after settlement"));
    }
    let request = load_request_bounded(
        store,
        repo,
        &status.request,
        request_record_limit(status.limits)?,
    )?;
    verify_attempt_inputs(&status, &request)?;
    let manifest = load_attempt_manifest(store, &status, &request)?;
    let Some(entry) = manifest
        .entries
        .iter()
        .find(|entry| entry.snapshot == *snapshot && entry.path == path)
    else {
        return Err(ResolutionError::OutOfScopeRead {
            snapshot: snapshot.to_string(),
            path: path.to_string(),
        });
    };
    let (Some(kind), Some(digest)) = (entry.kind, entry.sha256.as_deref()) else {
        return Ok(None);
    };
    let digest = ObjectDigest::from_bytes(parse_digest_hex(digest)?);
    let bytes = collaboration_archive::read_object_bounded(
        store,
        &digest,
        usize::try_from(status.limits.max_file_bytes)
            .map_err(|_| invalid("stored input file byte limit"))?,
    )?;
    if bytes.len() as u64 != entry.bytes {
        return Err(invalid(
            "resolution input changed size after manifest capture",
        ));
    }
    Ok(Some(ProposedFile { kind, bytes }))
}

fn verify_attempt_inputs(
    status: &ResolutionAttemptStatus,
    request: &ResolutionRequest,
) -> Result<(), ResolutionError> {
    let found = resolution_input_digest(request, &status.request.object);
    if found != status.input_sha256 {
        return Err(ResolutionError::RecordMismatch {
            record: status.request.id.as_str().to_string(),
            detail: "the retained resolution inputs differ from the charged attempt".into(),
        });
    }
    Ok(())
}

fn validate_attempt_binding(value: &str, label: &str) -> Result<(), ResolutionError> {
    if OperationId::parse(value).is_err() {
        return Err(invalid(label));
    }
    Ok(())
}

type AttemptStatusRow = (
    String,
    Option<i64>,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn attempt_status_conn(
    connection: &rusqlite::Connection,
    token: &str,
    session_label: &str,
) -> Result<ResolutionAttemptStatus, ResolutionError> {
    let row: Option<AttemptStatusRow> = connection
        .query_row(
            "SELECT coalesce(async_token,''), attempt_number, request_record_id,
                    coalesce(request_sha256,''), response_sha256, async_state,
                    outcome, detail, result_record_id, result_sha256,
                    session_label, client_key, decision_key, input_sha256,
                    input_manifest_sha256, dispatch_limits
             FROM resolution_attempts WHERE async_token=?1",
            [token],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                ))
            },
        )
        .optional()?;
    let Some((
        stored_token,
        attempt,
        request_id,
        request_sha256,
        response_sha256,
        state,
        outcome,
        detail,
        result_id,
        result_sha256,
        stored_session,
        _client_key,
        _decision_key,
        input_sha256,
        input_manifest_sha256,
        dispatch_limits,
    )) = row
    else {
        return Err(invalid("unknown resolution attempt token"));
    };
    if stored_token != token || stored_session != session_label {
        return Err(invalid("resolution attempt provenance mismatch"));
    }
    let attempt = attempt
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid("resolution attempt number is malformed"))?;
    let request_id = RecordId::parse(&request_id)
        .map_err(|_| invalid("resolution attempt request id is malformed"))?;
    let request_digest = parse_digest_hex(&request_sha256)?;
    let request = RecordRef {
        id: request_id,
        object: ObjectDigest::from_bytes(request_digest),
    };
    let result = match (result_id, result_sha256) {
        (Some(id), Some(digest)) => Some(RecordRef {
            id: RecordId::parse(&id).map_err(|_| invalid("result record id is malformed"))?,
            object: ObjectDigest::from_bytes(parse_digest_hex(&digest)?),
        }),
        (None, None) => None,
        _ => return Err(invalid("result record reference is incomplete")),
    };
    let input_sha256 = input_sha256
        .filter(|digest| !digest.is_empty())
        .ok_or_else(|| invalid("resolution attempt input digest is missing"))?;
    parse_digest_hex(&input_sha256)?;
    let input_manifest_sha256 = input_manifest_sha256
        .filter(|digest| !digest.is_empty())
        .ok_or_else(|| invalid("resolution attempt manifest digest is missing"))?;
    parse_digest_hex(&input_manifest_sha256)?;
    let limits = DispatchLimits::from_canonical_ledger_value(
        dispatch_limits
            .as_deref()
            .ok_or_else(|| invalid("resolution attempt dispatch limits are missing"))?,
    )?;
    Ok(ResolutionAttemptStatus {
        token: stored_token,
        session_label: stored_session,
        attempt,
        request,
        request_sha256,
        input_sha256,
        input_manifest_sha256,
        limits,
        response_sha256,
        state: ResolutionAttemptState::parse(&state)?,
        outcome,
        detail,
        result,
    })
}

fn parse_digest_hex(text: &str) -> Result<[u8; 32], ResolutionError> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("archive digest is malformed"));
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("archive digest is malformed"))?;
    }
    Ok(bytes)
}

fn attempt_inputs(request: &ResolutionRequest) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut snapshots = BTreeSet::from([
        request.baseline.snapshot_id.to_string(),
        request.accumulator.snapshot_id.to_string(),
    ]);
    let mut contributions = BTreeSet::new();
    for member in request
        .decision_members
        .iter()
        .chain(&request.members)
        .chain(&request.contents)
    {
        contributions.insert(member.lineage.as_str().to_string());
        snapshots.insert(member.base.as_str().to_string());
        snapshots.insert(member.result.as_str().to_string());
    }
    (snapshots, contributions)
}

fn visible_input_snapshots(request: &ResolutionRequest) -> BTreeSet<SourceSnapshotId> {
    let mut snapshots = BTreeSet::from([
        request.baseline.snapshot_id.clone(),
        request.accumulator.snapshot_id.clone(),
    ]);
    for member in request.members.iter().chain(&request.contents) {
        snapshots.insert(member.base.clone());
        snapshots.insert(member.result.clone());
    }
    snapshots
}

fn account_manifest_entry(
    encoded_size: &mut usize,
    snapshot: &SourceSnapshotId,
    path: &str,
    present: bool,
    max_bytes: usize,
) -> Result<(), ResolutionError> {
    let size = 8usize
        .checked_add(snapshot.as_str().len())
        .and_then(|size| size.checked_add(8))
        .and_then(|size| size.checked_add(path.len()))
        .and_then(|size| size.checked_add(1 + 8 + if present { 32 } else { 0 }))
        .ok_or_else(|| invalid("resolution input manifest size overflow"))?;
    *encoded_size = encoded_size
        .checked_add(size)
        .ok_or_else(|| invalid("resolution input manifest size overflow"))?;
    if *encoded_size > max_bytes {
        return Err(invalid("resolution input manifest exceeds its byte limit"));
    }
    Ok(())
}

pub(crate) fn build_input_manifest(
    store: &CollaborationStore,
    request: &ResolutionRequest,
    limits: DispatchLimits,
) -> Result<(Vec<u8>, ResolutionInputManifest), ResolutionError> {
    let limits = limits.validate()?;
    let snapshots = visible_input_snapshots(request);
    let manifest_entries = snapshots
        .len()
        .checked_mul(request.scope.len())
        .ok_or_else(|| invalid("resolution input manifest entry count overflow"))?;
    if manifest_entries as u64 > limits.max_manifest_entries {
        return Err(invalid("resolution input manifest exceeds its entry limit"));
    }
    let manifest_bytes_limit = usize::try_from(limits.max_manifest_bytes)
        .map_err(|_| invalid("resolution input manifest byte limit"))?;
    let file_bytes_limit = usize::try_from(limits.max_file_bytes)
        .map_err(|_| invalid("resolution input file byte limit"))?;
    let mut entries = Vec::new();
    let mut unique_sizes = BTreeMap::<[u8; 32], u64>::new();
    let mut total_unique_bytes = 0u64;
    let mut encoded_size = ASYNC_INPUT_MANIFEST_MAGIC
        .len()
        .checked_add(32 + 5 * 8 + 4)
        .ok_or_else(|| invalid("resolution input manifest size overflow"))?;
    if encoded_size > manifest_bytes_limit {
        return Err(invalid("resolution input manifest exceeds its byte limit"));
    }

    for snapshot_id in snapshots {
        let manifest_digest = ObjectDigest::of_snapshot(&snapshot_id);
        let manifest_bytes = collaboration_archive::read_object_bounded(
            store,
            &manifest_digest,
            manifest_bytes_limit,
        )?;
        let snapshot = collaboration_archive::parse_manifest(&manifest_bytes)
            .filter(|snapshot| {
                snapshot.id() == snapshot_id && snapshot.manifest_bytes() == manifest_bytes
            })
            .ok_or_else(|| ArchiveError::CorruptObject {
                digest: manifest_digest.hex(),
            })?;
        if snapshot.entries().len() as u64 > limits.max_manifest_entries {
            return Err(invalid("source snapshot exceeds the manifest entry limit"));
        }

        for path in &request.scope {
            let entry = snapshot
                .entries()
                .iter()
                .find(|entry| entry.path() == path.as_bytes());
            match entry {
                None => {
                    account_manifest_entry(
                        &mut encoded_size,
                        &snapshot_id,
                        path,
                        false,
                        manifest_bytes_limit,
                    )?;
                    entries.push(InputManifestEntry {
                        snapshot: snapshot_id.clone(),
                        path: path.clone(),
                        kind: None,
                        bytes: 0,
                        sha256: None,
                    });
                }
                Some(entry) => {
                    account_manifest_entry(
                        &mut encoded_size,
                        &snapshot_id,
                        path,
                        true,
                        manifest_bytes_limit,
                    )?;
                    let digest = ObjectDigest::from_bytes(*entry.content_sha256());
                    let size = collaboration_archive::object_size(store, &digest)?;
                    if size > limits.max_file_bytes {
                        return Err(invalid(format!(
                            "resolution input {} in {} exceeds its per-file byte limit",
                            path, snapshot_id
                        )));
                    }
                    if let Some(previous_size) = unique_sizes.get(&digest.raw()) {
                        if *previous_size != size {
                            return Err(invalid("one input digest has inconsistent object sizes"));
                        }
                    } else {
                        total_unique_bytes = total_unique_bytes
                            .checked_add(size)
                            .ok_or_else(|| invalid("resolution input byte count overflow"))?;
                        if total_unique_bytes > limits.max_total_input_bytes {
                            return Err(invalid(
                                "resolution inputs exceed their total unique byte limit",
                            ));
                        }
                        let content = collaboration_archive::read_object_bounded(
                            store,
                            &digest,
                            file_bytes_limit,
                        )?;
                        if content.len() as u64 != size {
                            return Err(invalid("resolution input size changed during inspection"));
                        }
                        unique_sizes.insert(digest.raw(), size);
                    }
                    entries.push(InputManifestEntry {
                        snapshot: snapshot_id.clone(),
                        path: path.clone(),
                        kind: Some(entry.kind()),
                        bytes: size,
                        sha256: Some(digest.hex()),
                    });
                }
            }
        }
    }

    let encoded = encode_input_manifest(
        &request.record().object,
        limits,
        &entries,
        manifest_bytes_limit,
    )?;
    let sha256 = ObjectDigest::of(&encoded).hex();
    Ok((encoded, ResolutionInputManifest { sha256, entries }))
}

fn append_manifest_bytes(
    output: &mut Vec<u8>,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<(), ResolutionError> {
    if output
        .len()
        .checked_add(bytes.len())
        .is_none_or(|length| length > max_bytes)
    {
        return Err(invalid("bounded payload exceeds its byte limit"));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn append_manifest_string(
    output: &mut Vec<u8>,
    value: &str,
    max_bytes: usize,
) -> Result<(), ResolutionError> {
    let length = u64::try_from(value.len())
        .map_err(|_| invalid("resolution input manifest field is too large"))?;
    append_manifest_bytes(output, &length.to_be_bytes(), max_bytes)?;
    append_manifest_bytes(output, value.as_bytes(), max_bytes)
}

fn entry_kind_name(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Regular => "regular",
        EntryKind::Executable => "executable",
        EntryKind::Symlink => "symlink",
    }
}

fn encode_input_manifest(
    request_digest: &ObjectDigest,
    limits: DispatchLimits,
    entries: &[InputManifestEntry],
    max_bytes: usize,
) -> Result<Vec<u8>, ResolutionError> {
    if entries.len() as u64 > limits.max_manifest_entries {
        return Err(invalid("resolution input manifest exceeds its entry limit"));
    }
    let mut output = Vec::new();
    append_manifest_bytes(&mut output, ASYNC_INPUT_MANIFEST_MAGIC, max_bytes)?;
    append_manifest_bytes(&mut output, &request_digest.raw(), max_bytes)?;
    for value in [
        limits.max_manifest_bytes,
        limits.max_manifest_entries,
        limits.max_file_bytes,
        limits.max_total_input_bytes,
        limits.max_response_bytes,
    ] {
        append_manifest_bytes(&mut output, &value.to_be_bytes(), max_bytes)?;
    }
    let count = u32::try_from(entries.len())
        .map_err(|_| invalid("resolution input manifest has too many entries"))?;
    append_manifest_bytes(&mut output, &count.to_be_bytes(), max_bytes)?;
    for entry in entries {
        append_manifest_string(&mut output, entry.snapshot.as_str(), max_bytes)?;
        append_manifest_string(&mut output, &entry.path, max_bytes)?;
        match (entry.kind, entry.sha256.as_deref()) {
            (None, None) if entry.bytes == 0 => {
                append_manifest_bytes(&mut output, &[0], max_bytes)?
            }
            (Some(kind), Some(digest)) => {
                append_manifest_bytes(
                    &mut output,
                    &[match kind {
                        EntryKind::Regular => 1,
                        EntryKind::Executable => 2,
                        EntryKind::Symlink => 3,
                    }],
                    max_bytes,
                )?;
                append_manifest_bytes(&mut output, &entry.bytes.to_be_bytes(), max_bytes)?;
                append_manifest_bytes(&mut output, &parse_digest_hex(digest)?, max_bytes)?;
            }
            _ => return Err(invalid("resolution input manifest entry is incomplete")),
        }
        if entry.kind.is_none() {
            append_manifest_bytes(&mut output, &0u64.to_be_bytes(), max_bytes)?;
        }
    }
    Ok(output)
}

pub(crate) fn resolution_input_digest(
    request: &ResolutionRequest,
    request_object: &ObjectDigest,
) -> String {
    let (snapshots, contributions) = attempt_inputs(request);
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme async resolution inputs v1\0");
    hasher.update(request_object.raw());
    for snapshot in snapshots {
        hasher.update(b"\0s");
        hasher.update(snapshot.as_bytes());
    }
    for contribution in contributions {
        hasher.update(b"\0c");
        hasher.update(contribution.as_bytes());
    }
    hex(&hasher.finalize())
}

pub(crate) fn retain_attempt_inputs(
    transaction: &rusqlite::Transaction<'_>,
    token: &str,
    record: &RecordRef,
    request: &ResolutionRequest,
) -> Result<(), ResolutionError> {
    let holder = format!("resolution:{token}");
    let now = now_ms();
    let (snapshots, contributions) = attempt_inputs(request);
    for snapshot in snapshots {
        let retained: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM retained_snapshots WHERE snapshot_id=?1
                AND snapshot_id NOT IN (SELECT snapshot_id FROM reclaimed_snapshots))",
            [&snapshot],
            |row| row.get(0),
        )?;
        if !retained {
            return Err(invalid(format!(
                "resolution input snapshot {snapshot} is not retained"
            )));
        }
        transaction.execute(
            "INSERT INTO reader_leases(target_kind,target,holder,until_ms,created_ms)
             VALUES('snapshot',?1,?2,?3,?4)",
            rusqlite::params![snapshot, holder, i64::MAX, now],
        )?;
    }
    for contribution in contributions {
        let retained: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM retained_contributions WHERE lineage_record_id=?1
                AND lineage_record_id NOT IN (SELECT lineage_record_id FROM reclaimed_contributions))",
            [&contribution],
            |row| row.get(0),
        )?;
        if !retained {
            return Err(invalid(format!(
                "resolution contribution {contribution} is not retained"
            )));
        }
        transaction.execute(
            "INSERT INTO reader_leases(target_kind,target,holder,until_ms,created_ms)
             VALUES('contribution',?1,?2,?3,?4)",
            rusqlite::params![contribution, holder, i64::MAX, now],
        )?;
    }
    transaction.execute(
        "INSERT INTO object_pins(class,object_sha256,holder,until_ms,created_ms)
         VALUES('cited_evidence',?1,?2,NULL,?3)",
        rusqlite::params![record.object.hex(), holder, now],
    )?;
    Ok(())
}

/// A response packet whose label and exact request identity were checked
/// against one durable attempt before it can be imported.
#[derive(Debug, Clone)]
pub struct BoundResolutionResponse {
    token: String,
    session_label: String,
    request: RecordRef,
    input_manifest_sha256: String,
    response: Response,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptResponseEnvelope {
    schema: String,
    attempt_token: String,
    session_label: String,
    request_record_id: String,
    request_object_sha256: String,
    input_manifest_sha256: String,
    response: AttemptResponseBody,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptResponseBody {
    kind: String,
    detail: Option<String>,
    brief: Option<String>,
    writes: Option<Vec<AttemptResponseWrite>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptResponseWrite {
    path: String,
    #[serde(default)]
    delete: bool,
    kind: Option<String>,
    bytes_hex: Option<String>,
}

impl AttemptResponseBody {
    fn into_response(self) -> Result<Response, ResolutionError> {
        match self.kind.as_str() {
            "unresolved" => match (self.detail, self.brief, self.writes) {
                (Some(detail), None, None) => Ok(Response::Unresolved(detail)),
                _ => Err(invalid("unresolved response envelope fields")),
            },
            "inconclusive" => match (self.detail, self.brief, self.writes) {
                (Some(detail), None, None) => Ok(Response::Inconclusive(detail)),
                _ => Err(invalid("inconclusive response envelope fields")),
            },
            "proposal" => {
                let (None, brief, Some(writes)) = (self.detail, self.brief, self.writes) else {
                    return Err(invalid("proposal response envelope fields"));
                };
                if writes.len() > 4096 {
                    return Err(invalid("too many response paths"));
                }
                let mut proposal_writes = BTreeMap::new();
                for write in writes {
                    let file = if write.delete {
                        if write.kind.is_some() || write.bytes_hex.is_some() {
                            return Err(invalid("delete response path has file content"));
                        }
                        None
                    } else {
                        let kind = match write.kind.as_deref() {
                            Some("regular") => EntryKind::Regular,
                            Some("executable") => EntryKind::Executable,
                            Some("symlink") => EntryKind::Symlink,
                            _ => return Err(invalid("response file kind is missing or unknown")),
                        };
                        let bytes_hex = write
                            .bytes_hex
                            .as_deref()
                            .ok_or_else(|| invalid("response file bytes are missing"))?;
                        Some(ProposedFile {
                            kind,
                            bytes: decode_hex_bytes(bytes_hex)?,
                        })
                    };
                    if proposal_writes.insert(write.path, file).is_some() {
                        return Err(invalid("duplicate response path"));
                    }
                }
                Ok(Response::Proposal(Proposal {
                    writes: proposal_writes,
                    brief,
                }))
            }
            _ => Err(invalid("unknown response envelope kind")),
        }
    }
}

fn decode_hex_bytes(text: &str) -> Result<Vec<u8>, ResolutionError> {
    if !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid("response file bytes must be lowercase hexadecimal"));
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for index in (0..text.len()).step_by(2) {
        bytes.push(
            u8::from_str_radix(&text[index..index + 2], 16)
                .map_err(|_| invalid("response file bytes are malformed"))?,
        );
    }
    Ok(bytes)
}

/// Decode a versioned, strict JSON response envelope under the attempt's
/// persisted byte limit. The envelope binds the response to this exact token,
/// provenance label, archived request and input manifest.
pub fn decode_attempt_response_envelope(
    status: &ResolutionAttemptStatus,
    bytes: &[u8],
) -> Result<BoundResolutionResponse, ResolutionError> {
    if bytes.len() as u64 > status.limits.max_response_bytes {
        return Err(invalid(
            "response envelope exceeds its persisted byte limit",
        ));
    }
    let envelope: AttemptResponseEnvelope = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("response envelope JSON: {error}")))?;
    if envelope.schema != ASYNC_RESPONSE_ENVELOPE_SCHEMA
        || envelope.attempt_token != status.token
        || envelope.session_label != status.session_label
        || envelope.request_record_id != status.request.id.as_str()
        || envelope.request_object_sha256 != status.request.object.hex()
        || envelope.input_manifest_sha256 != status.input_manifest_sha256
    {
        return Err(invalid("response envelope attempt binding mismatch"));
    }
    let response = envelope.response.into_response()?;
    encode_response(
        &response,
        usize::try_from(status.limits.max_response_bytes)
            .map_err(|_| invalid("stored response byte limit"))?,
    )?;
    Ok(BoundResolutionResponse {
        token: status.token.clone(),
        session_label: status.session_label.clone(),
        request: status.request.clone(),
        input_manifest_sha256: status.input_manifest_sha256.clone(),
        response,
    })
}

/// Import a response envelope previously bound to the durable attempt. The
/// status is re-read immediately before import to reject stale or retargeted
/// packets; normal proposal validation and single-use settlement still apply.
/// A synthesized result remains an unverified candidate for the controller's
/// independent checks and acceptance gates.
pub fn finish_bound_attempt(
    store: &mut CollaborationStore,
    repo: &Path,
    bound: &BoundResolutionResponse,
) -> Result<Resolution, ResolutionError> {
    let current = attempt_status(store, &bound.token, &bound.session_label)?;
    if current.request != bound.request
        || current.input_manifest_sha256 != bound.input_manifest_sha256
    {
        return Err(invalid("response envelope attempt binding changed"));
    }
    finish_attempt(
        store,
        repo,
        &bound.token,
        &bound.session_label,
        &bound.response,
    )
}

/// Persist one response by content digest and then run the common proposal
/// checks. The response object is pinned before its status becomes importing;
/// retrying after a crash can only load that exact object, never dispatch again.
fn finish_attempt(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    session_label: &str,
    response: &Response,
) -> Result<Resolution, ResolutionError> {
    validate_attempt_binding(token, "attempt token")?;
    validate_attempt_binding(session_label, "session label")?;
    let existing = attempt_status(store, token, session_label)?;
    let encoded = encode_response(
        response,
        usize::try_from(existing.limits.max_response_bytes)
            .map_err(|_| invalid("stored response byte limit"))?,
    )?;
    let response_digest = ObjectDigest::of(&encoded);
    let response_sha256 = response_digest.hex();
    match existing.state {
        ResolutionAttemptState::Exported => {
            return Err(invalid("attempt must be marked dispatching before import"));
        }
        ResolutionAttemptState::Settled => {
            return Err(invalid("response was already imported"));
        }
        ResolutionAttemptState::Importing
            if existing.response_sha256.as_deref() != Some(response_sha256.as_str()) =>
        {
            return Err(invalid(
                "a different response is already bound to this attempt",
            ));
        }
        _ => {}
    }
    {
        let _archive = crate::collaboration_gc::archive_use(store)
            .map_err(|error| invalid(format!("archive retention: {error}")))?;
        let published = collaboration_archive::put_object(store, &encoded)?;
        if published != response_digest {
            return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
                digest: response_digest.hex(),
            }));
        }
        let transaction = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = attempt_status_conn(&transaction, token, session_label)?;
        match current.state {
            ResolutionAttemptState::Exported => {
                return Err(invalid("attempt must be marked dispatching before import"));
            }
            ResolutionAttemptState::Dispatching | ResolutionAttemptState::OutcomeUnknown => {
                transaction.execute(
                    "UPDATE resolution_attempts
                     SET async_state='importing', response_sha256=?2, detail=NULL
                     WHERE async_token=?1 AND session_label=?3
                       AND async_state IN ('dispatching','outcome_unknown')",
                    rusqlite::params![token, response_digest.hex(), session_label],
                )?;
                transaction.execute(
                    "INSERT INTO object_pins(class,object_sha256,holder,until_ms,created_ms)
                     VALUES('cited_evidence',?1,?2,NULL,?3)",
                    rusqlite::params![
                        response_digest.hex(),
                        format!("resolution:{token}"),
                        now_ms()
                    ],
                )?;
            }
            ResolutionAttemptState::Importing
                if current.response_sha256.as_deref() == Some(response_sha256.as_str()) => {}
            ResolutionAttemptState::Importing => {
                return Err(invalid(
                    "a different response is already bound to this attempt",
                ));
            }
            ResolutionAttemptState::Settled => {
                return Err(invalid("response was already imported"));
            }
        }
        transaction.commit()?;
    }
    resume_attempt_import(store, repo, token, session_label)
}

/// Resume only the locally persisted response of an importing attempt. This
/// is recovery of the same single-use import, not a second agent dispatch.
pub fn resume_attempt_import(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    session_label: &str,
) -> Result<Resolution, ResolutionError> {
    validate_attempt_binding(token, "attempt token")?;
    validate_attempt_binding(session_label, "session label")?;
    let _archive = crate::collaboration_gc::archive_use(store)
        .map_err(|error| invalid(format!("archive retention: {error}")))?;
    let status = attempt_status(store, token, session_label)?;
    if status.state != ResolutionAttemptState::Importing {
        return Err(invalid("attempt has no import to resume"));
    }
    let response_sha256 = status
        .response_sha256
        .as_deref()
        .ok_or_else(|| invalid("importing attempt has no response digest"))?;
    let digest = ObjectDigest::from_bytes(parse_digest_hex(response_sha256)?);
    let response_bytes = collaboration_archive::read_object_bounded(
        store,
        &digest,
        usize::try_from(status.limits.max_response_bytes)
            .map_err(|_| invalid("stored response byte limit"))?,
    )?;
    let response = decode_response(&response_bytes)?;
    let identity = ResolverIdentity {
        name: "external-session-label".into(),
        version: session_label.into(),
    };
    let request = match load_request_bounded(
        store,
        repo,
        &status.request,
        request_record_limit(status.limits)?,
    ) {
        Ok(request) => match verify_attempt_inputs(&status, &request)
            .and_then(|()| load_attempt_manifest(store, &status, &request).map(|_| ()))
        {
            Ok(()) => Some(request),
            Err(error) => {
                let outcome = failed(after_dispatch(&error), format!("{}: {error}", error.code()));
                settle_async_attempt(store, token, session_label, response_sha256, &outcome, None)?;
                return Ok(Resolution {
                    outcome,
                    attempt: Some(status.attempt),
                    allowance: Allowance {
                        limit: MAX_SYNTHESIS_ATTEMPTS,
                        used: status.attempt,
                    },
                });
            }
        },
        Err(error) => {
            let outcome = failed(after_dispatch(&error), format!("{}: {error}", error.code()));
            settle_async_attempt(store, token, session_label, response_sha256, &outcome, None)?;
            return Ok(Resolution {
                outcome,
                attempt: Some(status.attempt),
                allowance: Allowance {
                    limit: MAX_SYNTHESIS_ATTEMPTS,
                    used: status.attempt,
                },
            });
        }
    };
    let request = request.expect("successful load returned a request");
    let outcome = match apply_response(
        store,
        repo,
        &request,
        Ok(response),
        &identity,
        status.attempt,
    ) {
        Ok(outcome) => outcome,
        Err(error) => failed(after_dispatch(&error), format!("{}: {error}", error.code())),
    };
    let result = match &outcome {
        ResolutionOutcome::Synthesized(synthesized) => Some((
            synthesized.group_record.clone(),
            synthesized.candidate.subject.clone(),
        )),
        ResolutionOutcome::Failed { .. } => None,
    };
    settle_async_attempt(
        store,
        token,
        session_label,
        response_sha256,
        &outcome,
        result.as_ref(),
    )?;
    Ok(Resolution {
        outcome,
        attempt: Some(status.attempt),
        allowance: Allowance {
            limit: MAX_SYNTHESIS_ATTEMPTS,
            used: attempts_used(store, &request.decision)?,
        },
    })
}

fn settle_async_attempt(
    store: &mut CollaborationStore,
    token: &str,
    session_label: &str,
    response_sha256: &str,
    outcome: &ResolutionOutcome,
    result: Option<&(RecordRef, SourceSnapshotId)>,
) -> Result<(), ResolutionError> {
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = attempt_status_conn(&transaction, token, session_label)?;
    if current.state != ResolutionAttemptState::Importing
        || current.response_sha256.as_deref() != Some(response_sha256)
    {
        return Err(invalid("attempt import state changed during validation"));
    }
    if let Some((record, snapshot)) = result {
        let holder = format!("resolution:{token}");
        transaction.execute(
            "INSERT INTO reader_leases(target_kind,target,holder,until_ms,created_ms)
             VALUES('snapshot',?1,?2,?3,?4)",
            rusqlite::params![snapshot.as_str(), holder, i64::MAX, now_ms()],
        )?;
        transaction.execute(
            "INSERT INTO object_pins(class,object_sha256,holder,until_ms,created_ms)
             VALUES('cited_evidence',?1,?2,NULL,?3)",
            rusqlite::params![record.object.hex(), holder, now_ms()],
        )?;
    }
    let (result_id, result_sha) = result
        .map(|(record, _)| (Some(record.id.as_str()), Some(record.object.hex())))
        .unwrap_or((None, None));
    transaction.execute(
        "UPDATE resolution_attempts
         SET outcome=?2, detail=?3, result_record_id=?4, result_sha256=?5,
             async_state='settled'
         WHERE async_token=?1 AND async_state='importing' AND response_sha256=?6",
        rusqlite::params![
            token,
            outcome.code(),
            outcome_detail(outcome),
            result_id,
            result_sha,
            response_sha256,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

fn outcome_detail(outcome: &ResolutionOutcome) -> Option<&str> {
    match outcome {
        ResolutionOutcome::Synthesized(_) => None,
        ResolutionOutcome::Failed { detail, .. } => Some(detail),
    }
}

/// Encode a response under an explicit caller limit. Used by offline tools
/// to create the canonical internal payload without provider or dispatch APIs.
pub fn encode_response_bytes(
    response: &Response,
    max_bytes: u64,
) -> Result<Vec<u8>, ResolutionError> {
    let max_bytes =
        usize::try_from(max_bytes).map_err(|_| invalid("asynchronous response byte limit"))?;
    encode_response(response, max_bytes)
}

/// Decode the canonical internal response format under an explicit caller
/// limit. The attempt-bound JSON envelope should be preferred for CLI import.
pub fn decode_response_bytes(bytes: &[u8], max_bytes: u64) -> Result<Response, ResolutionError> {
    let max_bytes =
        usize::try_from(max_bytes).map_err(|_| invalid("asynchronous response byte limit"))?;
    if max_bytes == 0 || max_bytes > MAX_ASYNC_RESPONSE_BYTES || bytes.len() > max_bytes {
        return Err(invalid("asynchronous response byte limit"));
    }
    decode_response(bytes)
}

fn encode_response(response: &Response, max_bytes: usize) -> Result<Vec<u8>, ResolutionError> {
    if max_bytes == 0 || max_bytes > MAX_ASYNC_RESPONSE_BYTES {
        return Err(invalid("asynchronous response byte limit"));
    }
    let mut bytes = Vec::new();
    append_manifest_bytes(&mut bytes, ASYNC_RESPONSE_MAGIC, max_bytes)?;
    match response {
        Response::Unresolved(detail) => {
            append_manifest_bytes(&mut bytes, &[0], max_bytes)?;
            write_response_string(&mut bytes, detail, max_bytes)?;
        }
        Response::Inconclusive(detail) => {
            append_manifest_bytes(&mut bytes, &[1], max_bytes)?;
            write_response_string(&mut bytes, detail, max_bytes)?;
        }
        Response::Proposal(proposal) => {
            if proposal.writes.len() > 4096 {
                return Err(invalid("too many response paths"));
            }
            append_manifest_bytes(&mut bytes, &[2], max_bytes)?;
            match &proposal.brief {
                Some(brief) => {
                    append_manifest_bytes(&mut bytes, &[1], max_bytes)?;
                    write_response_string(&mut bytes, brief, max_bytes)?;
                }
                None => append_manifest_bytes(&mut bytes, &[0], max_bytes)?,
            }
            let count = u32::try_from(proposal.writes.len())
                .map_err(|_| invalid("too many response paths"))?;
            append_manifest_bytes(&mut bytes, &count.to_be_bytes(), max_bytes)?;
            for (path, file) in &proposal.writes {
                write_response_string(&mut bytes, path, max_bytes)?;
                match file {
                    None => append_manifest_bytes(&mut bytes, &[0], max_bytes)?,
                    Some(file) => {
                        append_manifest_bytes(&mut bytes, &[1], max_bytes)?;
                        append_manifest_bytes(
                            &mut bytes,
                            &[match file.kind {
                                EntryKind::Regular => 0,
                                EntryKind::Executable => 1,
                                EntryKind::Symlink => 2,
                            }],
                            max_bytes,
                        )?;
                        write_response_bytes(&mut bytes, &file.bytes, max_bytes)?;
                    }
                }
            }
        }
    }
    Ok(bytes)
}

fn write_response_string(
    bytes: &mut Vec<u8>,
    text: &str,
    max_bytes: usize,
) -> Result<(), ResolutionError> {
    write_response_bytes(bytes, text.as_bytes(), max_bytes)
}

fn write_response_bytes(
    bytes: &mut Vec<u8>,
    content: &[u8],
    max_bytes: usize,
) -> Result<(), ResolutionError> {
    let length =
        u64::try_from(content.len()).map_err(|_| invalid("response field is too large"))?;
    append_manifest_bytes(bytes, &length.to_be_bytes(), max_bytes)?;
    append_manifest_bytes(bytes, content, max_bytes)?;
    Ok(())
}

struct ResponseReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl ResponseReader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], ResolutionError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("response length overflow"))?;
        if end > self.bytes.len() {
            return Err(invalid("truncated asynchronous response"));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, ResolutionError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ResolutionError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, ResolutionError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, ResolutionError> {
        let length =
            usize::try_from(self.u64()?).map_err(|_| invalid("response field is too large"))?;
        Ok(self.take(length)?.to_vec())
    }

    fn string(&mut self) -> Result<String, ResolutionError> {
        String::from_utf8(self.bytes()?).map_err(|_| invalid("response string is not UTF-8"))
    }
}

fn decode_input_manifest(
    bytes: &[u8],
    request_digest: &ObjectDigest,
    limits: DispatchLimits,
) -> Result<ResolutionInputManifest, ResolutionError> {
    if bytes.len() as u64 > limits.max_manifest_bytes
        || !bytes.starts_with(ASYNC_INPUT_MANIFEST_MAGIC)
    {
        return Err(invalid(
            "unsupported or oversized resolution input manifest",
        ));
    }
    let mut reader = ResponseReader {
        bytes,
        offset: ASYNC_INPUT_MANIFEST_MAGIC.len(),
    };
    if reader.take(32)? != request_digest.raw() {
        return Err(invalid(
            "resolution input manifest request binding mismatch",
        ));
    }
    let stored_limits = DispatchLimits {
        max_manifest_bytes: reader.u64()?,
        max_manifest_entries: reader.u64()?,
        max_file_bytes: reader.u64()?,
        max_total_input_bytes: reader.u64()?,
        max_response_bytes: reader.u64()?,
    }
    .validate()?;
    if stored_limits != limits {
        return Err(invalid("resolution input manifest limits mismatch"));
    }
    let count = reader.u32()? as usize;
    if count as u64 > limits.max_manifest_entries {
        return Err(invalid("resolution input manifest exceeds its entry limit"));
    }
    // Even an absent entry needs a snapshot id (71 bytes), a path, two
    // lengths, a marker and a size. Reject impossible counts before reserving.
    if count > bytes.len().saturating_sub(reader.offset) / 97 {
        return Err(invalid("truncated resolution input manifest"));
    }
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let mut unique_sizes = BTreeMap::<[u8; 32], u64>::new();
    let mut total_unique_bytes = 0u64;
    for _ in 0..count {
        let snapshot_text = reader.string()?;
        let snapshot = SourceSnapshotId::parse(&snapshot_text)
            .map_err(|_| invalid("resolution input snapshot id is malformed"))?;
        let path = reader.string()?;
        let kind = match reader.byte()? {
            0 => None,
            1 => Some(EntryKind::Regular),
            2 => Some(EntryKind::Executable),
            3 => Some(EntryKind::Symlink),
            _ => return Err(invalid("resolution input entry kind is malformed")),
        };
        let size = reader.u64()?;
        let digest = if kind.is_some() {
            Some(reader.take(32)?.to_vec())
        } else {
            None
        };
        let sha256 = digest.map(|bytes| {
            let bytes: [u8; 32] = bytes.try_into().expect("digest is 32 bytes");
            hex(&bytes)
        });
        match (kind, sha256.as_deref()) {
            (None, None) if size == 0 => {}
            (Some(_), Some(digest)) if size <= limits.max_file_bytes => {
                let raw = parse_digest_hex(digest)?;
                if let Some(previous_size) = unique_sizes.get(&raw) {
                    if *previous_size != size {
                        return Err(invalid("input manifest has inconsistent digest sizes"));
                    }
                } else {
                    total_unique_bytes = total_unique_bytes
                        .checked_add(size)
                        .ok_or_else(|| invalid("input manifest byte count overflow"))?;
                    if total_unique_bytes > limits.max_total_input_bytes {
                        return Err(invalid("input manifest exceeds its total byte limit"));
                    }
                    unique_sizes.insert(raw, size);
                }
            }
            _ => return Err(invalid("input manifest file reference is incomplete")),
        }
        if !seen.insert((snapshot.clone(), path.clone())) {
            return Err(invalid("input manifest has duplicate snapshot paths"));
        }
        entries.push(InputManifestEntry {
            snapshot,
            path,
            kind,
            bytes: size,
            sha256,
        });
    }
    if reader.offset != bytes.len() {
        return Err(invalid("trailing bytes in resolution input manifest"));
    }
    Ok(ResolutionInputManifest {
        sha256: ObjectDigest::of(bytes).hex(),
        entries,
    })
}

fn validate_manifest_scope(
    request: &ResolutionRequest,
    manifest: &ResolutionInputManifest,
) -> Result<(), ResolutionError> {
    let expected: BTreeSet<_> = visible_input_snapshots(request)
        .into_iter()
        .flat_map(|snapshot| {
            request
                .scope
                .iter()
                .map(move |path| (snapshot.clone(), path.clone()))
        })
        .collect();
    let found: BTreeSet<_> = manifest
        .entries
        .iter()
        .map(|entry| (entry.snapshot.clone(), entry.path.clone()))
        .collect();
    if found != expected || found.len() != manifest.entries.len() {
        return Err(invalid(
            "resolution input manifest scope differs from request",
        ));
    }
    Ok(())
}

fn load_attempt_manifest(
    store: &CollaborationStore,
    status: &ResolutionAttemptStatus,
    request: &ResolutionRequest,
) -> Result<ResolutionInputManifest, ResolutionError> {
    let digest = ObjectDigest::from_bytes(parse_digest_hex(&status.input_manifest_sha256)?);
    let max_bytes = usize::try_from(status.limits.max_manifest_bytes)
        .map_err(|_| invalid("stored dispatch manifest byte limit"))?;
    let bytes = collaboration_archive::read_object_bounded(store, &digest, max_bytes)?;
    let manifest = decode_input_manifest(&bytes, &status.request.object, status.limits)?;
    if manifest.sha256 != status.input_manifest_sha256 {
        return Err(invalid("resolution input manifest digest mismatch"));
    }
    validate_manifest_scope(request, &manifest)?;
    Ok(manifest)
}

/// Read a settled synthesized result as an unverified candidate. This reloads
/// the charge, request, bounded manifest, every retained input object, and the
/// canonical result record; caller-held data cannot retarget the candidate.
pub fn read_synthesized_attempt_result(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    session_label: &str,
) -> Result<UnverifiedSynthesis, ResolutionError> {
    let _archive = crate::collaboration_gc::archive_use(store)
        .map_err(|error| invalid(format!("archive retention: {error}")))?;
    let _response = crate::collaboration_sensitive::response_use(store)
        .map_err(|error| invalid(format!("response access: {error}")))?;
    let status = attempt_status(store, token, session_label)?;
    if status.state != ResolutionAttemptState::Settled
        || status.outcome.as_deref() != Some("synthesized")
        || status.response_sha256.is_none()
    {
        return Err(invalid("attempt is not a settled synthesized result"));
    }
    if status.request_sha256 != status.request.object.hex() {
        return Err(ResolutionError::RecordMismatch {
            record: status.request.id.as_str().to_string(),
            detail: "charged request object digest changed".into(),
        });
    }
    let request = load_request_bounded(
        store,
        repo,
        &status.request,
        request_record_limit(status.limits)?,
    )?;
    verify_attempt_inputs(&status, &request)?;
    let input_manifest = load_attempt_manifest(store, &status, &request)?;
    verify_manifest_objects(store, &status, &input_manifest)?;
    let group_record = status
        .result
        .clone()
        .ok_or_else(|| invalid("synthesized attempt has no result record"))?;
    let bytes = collaboration_archive::read_object_bounded(
        store,
        &group_record.object,
        MAX_DOCUMENT_BYTES,
    )?;
    let group = Record::decode(&bytes, &[&SYNTHESIZED_GROUP_SCHEMA])
        .map_err(|error| invalid(format!("synthesized group record: {error}")))?;
    if group.id() != group_record.id || group.canonical_bytes() != bytes {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: group_record.object.hex(),
        }));
    }
    validate_synthesized_group_shape(&group, &status, &request)?;

    let text_field = |name: &str| match group.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(ResolutionError::RecordMismatch {
            record: group_record.id.as_str().to_string(),
            detail: format!("synthesized group field {name} is missing"),
        }),
    };
    let subject_text = text_field("candidate")?;
    let subject = SourceSnapshotId::parse(&subject_text)
        .map_err(|_| invalid("synthesized candidate snapshot id is malformed"))?;
    let commit_text = text_field("candidate_commit")?;
    let commit = CommitOid::parse(&commit_text)?;
    let snapshot = collaboration_archive::snapshot_of_commit(repo, &commit)?;
    if snapshot.id() != subject {
        return Err(ResolutionError::RecordMismatch {
            record: group_record.id.as_str().to_string(),
            detail: "synthesized candidate subject does not match its commit".into(),
        });
    }
    collaboration_archive::retained(store, &subject)?
        .ok_or_else(|| invalid("synthesized candidate snapshot is not retained"))?;
    let tree = collaboration_archive::raw_commit_tree(repo, &commit)?;
    let mut inputs: Vec<&GroupMember> = request.contents.iter().collect();
    for member in &request.members {
        if !inputs
            .iter()
            .any(|existing| existing.lineage == member.lineage)
        {
            inputs.push(member);
        }
    }
    let candidate = Candidate {
        subject,
        tree,
        commit,
        baseline: request.baseline.commit.clone(),
        baseline_source: COMPOSER_BASELINE_SOURCE,
        inputs: inputs
            .iter()
            .map(|member| CandidateInput {
                base: member.base_commit.clone(),
                result: member.result_commit.clone(),
                base_snapshot: Some(member.base.clone()),
                result_snapshot: Some(member.result.clone()),
            })
            .collect(),
        producer: Producer::Resolver {
            profile: SYNTHESIS_PROFILE.to_string(),
        },
        mode: CompositionMode::Synthesized,
    };
    Ok(UnverifiedSynthesis {
        status,
        request,
        candidate,
        group_record,
        input_manifest,
    })
}

/// Resolve a group only through the exact settled ledger row that binds both
/// its request and result record references.
pub(crate) fn read_synthesized_attempt_result_for_refs(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &RecordRef,
    result: &RecordRef,
) -> Result<UnverifiedSynthesis, ResolutionError> {
    let count: i64 = store.read_connection().query_row(
        "SELECT count(*) FROM resolution_attempts
         WHERE async_state='settled' AND outcome='synthesized'
           AND request_record_id=?1 AND request_sha256=?2
           AND result_record_id=?3 AND result_sha256=?4
           AND async_token IS NOT NULL AND session_label IS NOT NULL",
        rusqlite::params![
            request.id.as_str(),
            request.object.hex(),
            result.id.as_str(),
            result.object.hex(),
        ],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(invalid(
            "synthesized result is not bound to one settled request attempt",
        ));
    }
    let (token, session_label): (String, String) = store.read_connection().query_row(
        "SELECT async_token, session_label FROM resolution_attempts
         WHERE async_state='settled' AND outcome='synthesized'
           AND request_record_id=?1 AND request_sha256=?2
           AND result_record_id=?3 AND result_sha256=?4
           AND async_token IS NOT NULL AND session_label IS NOT NULL",
        rusqlite::params![
            request.id.as_str(),
            request.object.hex(),
            result.id.as_str(),
            result.object.hex(),
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let synthesis = read_synthesized_attempt_result(store, repo, &token, &session_label)?;
    if synthesis.status.request != *request || synthesis.group_record != *result {
        return Err(invalid("synthesized result references changed during read"));
    }
    Ok(synthesis)
}

fn historical_synthesis_trigger_is_valid(
    trigger: &Trigger,
    accumulator: &SourceSnapshotId,
) -> bool {
    match trigger {
        Trigger::Conflict(conflicts) => !conflicts.is_empty(),
        Trigger::FailedChecks { candidate, checks } => {
            !checks.is_empty() && candidate == accumulator
        }
    }
}

/// Revalidate the immutable synthesis ledger and its complete archived input
/// closure for historical baseline provenance. This deliberately omits Git
/// tree inspection and does not return a candidate accepted for execution;
/// the completion reader must bind the subject/commit and selected lineages
/// to the exact historical completion separately.
pub(crate) fn read_retained_synthesized_result_for_refs(
    store: &CollaborationStore,
    request_ref: &RecordRef,
    result_ref: &RecordRef,
) -> Result<RetainedSynthesisProof, ResolutionError> {
    let _archive = crate::collaboration_gc::archive_use(store)
        .map_err(|error| invalid(format!("archive retention: {error}")))?;
    let _response = crate::collaboration_sensitive::response_use(store)
        .map_err(|error| invalid(format!("response access: {error}")))?;
    let (token, session_label): (String, String) = {
        let connection = store.read_connection();
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM resolution_attempts
             WHERE async_state='settled' AND outcome='synthesized'
               AND request_record_id=?1 AND request_sha256=?2
               AND result_record_id=?3 AND result_sha256=?4
               AND async_token IS NOT NULL AND session_label IS NOT NULL",
            rusqlite::params![
                request_ref.id.as_str(),
                request_ref.object.hex(),
                result_ref.id.as_str(),
                result_ref.object.hex(),
            ],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(invalid(
                "historical synthesis is not bound to one settled request attempt",
            ));
        }
        connection.query_row(
            "SELECT async_token,session_label FROM resolution_attempts
             WHERE async_state='settled' AND outcome='synthesized'
               AND request_record_id=?1 AND request_sha256=?2
               AND result_record_id=?3 AND result_sha256=?4
               AND async_token IS NOT NULL AND session_label IS NOT NULL",
            rusqlite::params![
                request_ref.id.as_str(),
                request_ref.object.hex(),
                result_ref.id.as_str(),
                result_ref.object.hex(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
    };
    let status = attempt_status(store, &token, &session_label)?;
    if status.state != ResolutionAttemptState::Settled
        || status.outcome.as_deref() != Some("synthesized")
        || status.response_sha256.is_none()
        || status.request != *request_ref
        || status.result.as_ref() != Some(result_ref)
        || status.request_sha256 != request_ref.object.hex()
    {
        return Err(invalid("historical synthesis ledger binding changed"));
    }

    let request_bytes = collaboration_archive::read_object_bounded(
        store,
        &request_ref.object,
        request_record_limit(status.limits)?,
    )?;
    let request = load_request_from_bytes(store, None, request_ref, &request_bytes)?;
    verify_attempt_inputs(&status, &request)?;
    if request.record() != request_ref
        || !historical_synthesis_trigger_is_valid(
            request.trigger(),
            &request.accumulator().snapshot_id,
        )
    {
        return Err(invalid("historical synthesis request identity or trigger"));
    }
    let input_manifest = load_attempt_manifest(store, &status, &request)?;
    verify_manifest_objects(store, &status, &input_manifest)?;

    let group_bytes =
        collaboration_archive::read_object_bounded(store, &result_ref.object, MAX_DOCUMENT_BYTES)?;
    let group = Record::decode(&group_bytes, &[&SYNTHESIZED_GROUP_SCHEMA])
        .map_err(|error| invalid(format!("synthesized group record: {error}")))?;
    if group.id() != result_ref.id || group.canonical_bytes() != group_bytes {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: result_ref.object.hex(),
        }));
    }
    validate_synthesized_group_shape(&group, &status, &request)?;
    let subject_text = match group.get("candidate") {
        Some(Value::String(value)) => value,
        _ => return Err(invalid("historical synthesized candidate subject")),
    };
    let candidate_subject = SourceSnapshotId::parse(subject_text)
        .map_err(|_| invalid("historical synthesized candidate subject"))?;
    let candidate_commit = match group.get("candidate_commit") {
        Some(Value::String(value)) => CommitOid::parse(value)?,
        _ => return Err(invalid("historical synthesized candidate commit")),
    };
    collaboration_archive::retained(store, &candidate_subject)?
        .ok_or_else(|| invalid("historical synthesized candidate is not retained"))?;

    Ok(RetainedSynthesisProof {
        status,
        request,
        group_record: result_ref.clone(),
        candidate_subject,
        candidate_commit,
        input_manifest,
    })
}

/// Read the exact synthesized result settled for one request and async token.
/// The result reference comes from the immutable ledger row; caller-provided
/// token/request data can only select that row, never retarget its group.
pub(crate) fn read_synthesized_attempt_result_for_token_request(
    store: &mut CollaborationStore,
    repo: &Path,
    token: &str,
    request: &RecordRef,
) -> Result<UnverifiedSynthesis, ResolutionError> {
    let result_count: i64 = store.read_connection().query_row(
        "SELECT count(*) FROM resolution_attempts
         WHERE async_token=?1 AND request_record_id=?2 AND request_sha256=?3
           AND async_state='settled' AND outcome='synthesized'
           AND result_record_id IS NOT NULL AND result_sha256 IS NOT NULL
           AND session_label IS NOT NULL",
        rusqlite::params![token, request.id.as_str(), request.object.hex()],
        |row| row.get(0),
    )?;
    if result_count != 1 {
        return Err(invalid(
            "token and request do not identify one settled synthesized result",
        ));
    }
    let (result_id, result_sha256): (String, String) = store.read_connection().query_row(
        "SELECT result_record_id,result_sha256 FROM resolution_attempts
         WHERE async_token=?1 AND request_record_id=?2 AND request_sha256=?3
           AND async_state='settled' AND outcome='synthesized'
           AND result_record_id IS NOT NULL AND result_sha256 IS NOT NULL
           AND session_label IS NOT NULL",
        rusqlite::params![token, request.id.as_str(), request.object.hex()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let result = RecordRef {
        id: RecordId::parse(&result_id).map_err(|_| invalid("result record id is malformed"))?,
        object: ObjectDigest::from_bytes(parse_digest_hex(&result_sha256)?),
    };
    let synthesis = read_synthesized_attempt_result_for_refs(store, repo, request, &result)?;
    if synthesis.status.token != token || synthesis.status.request != *request {
        return Err(invalid(
            "settled result token or request changed during read",
        ));
    }
    Ok(synthesis)
}

fn verify_manifest_objects(
    store: &CollaborationStore,
    status: &ResolutionAttemptStatus,
    manifest: &ResolutionInputManifest,
) -> Result<(), ResolutionError> {
    let max_file_bytes = usize::try_from(status.limits.max_file_bytes)
        .map_err(|_| invalid("stored input file byte limit"))?;
    let mut seen = BTreeSet::new();
    for entry in &manifest.entries {
        let Some(digest_text) = entry.sha256.as_deref() else {
            if entry.kind.is_some() || entry.bytes != 0 {
                return Err(invalid("absent input entry has file metadata"));
            }
            continue;
        };
        let digest = ObjectDigest::from_bytes(parse_digest_hex(digest_text)?);
        if !seen.insert(digest.raw()) {
            continue;
        }
        let bytes = collaboration_archive::read_object_bounded(store, &digest, max_file_bytes)?;
        if bytes.len() as u64 != entry.bytes || ObjectDigest::of(&bytes) != digest {
            return Err(invalid("resolution input manifest object changed"));
        }
    }
    Ok(())
}

fn validate_synthesized_group_shape(
    group: &Record,
    status: &ResolutionAttemptStatus,
    request: &ResolutionRequest,
) -> Result<(), ResolutionError> {
    let fail = |detail: &str| ResolutionError::RecordMismatch {
        record: status
            .result
            .as_ref()
            .map(|record| record.id.as_str().to_string())
            .unwrap_or_else(|| status.request.id.as_str().to_string()),
        detail: detail.into(),
    };
    let bytes = group.canonical_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("synthesized group JSON: {error}")))?;
    let object = payload
        .as_object()
        .ok_or_else(|| fail("synthesized group is not an object"))?;
    let allowed = [
        "schema",
        "request",
        "decision",
        "constituents",
        "contents",
        "derived_from",
        "resolver",
        "brief",
        "attempt",
        "baseline",
        "accumulator",
        "candidate",
        "candidate_commit",
        "profile",
        "requires_verification",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || payload["schema"].as_str() != Some(SYNTHESIZED_GROUP_SCHEMA_NAME)
        || group.get("request") != Some(&text(status.request.id.as_str()))
        || group.get("decision") != Some(&text(&request.decision.key()))
        || group.get("attempt") != Some(&Value::Integer(i64::from(status.attempt)))
        || group.get("baseline") != Some(&text(request.baseline.snapshot_id.as_str()))
        || group.get("accumulator") != Some(&text(request.accumulator.snapshot_id.as_str()))
        || group.get("profile") != Some(&text(SYNTHESIS_PROFILE))
        || group.get("requires_verification") != Some(&Value::Bool(true))
    {
        return Err(fail("synthesized group bindings or schema fields changed"));
    }

    let constituents = Value::Array(request.members.iter().map(member_value).collect());
    let mut contents: Vec<&GroupMember> = request.contents.iter().collect();
    for member in &request.members {
        if !contents
            .iter()
            .any(|existing| existing.lineage == member.lineage)
        {
            contents.push(member);
        }
    }
    let expected_contents =
        Value::Array(contents.iter().map(|member| member_value(member)).collect());
    let derived_from = Value::Array(contents.iter().map(|member| text(&member.id)).collect());
    if group.get("constituents") != Some(&constituents)
        || group.get("contents") != Some(&expected_contents)
        || group.get("derived_from") != Some(&derived_from)
    {
        return Err(fail(
            "synthesized group contributions differ from its request",
        ));
    }

    match group.get("resolver") {
        Some(Value::Object(resolver))
            if resolver.len() == 2
                && ["name", "version"].iter().all(|key| {
                    matches!(resolver.get(key), Some(Value::String(value))
                        if !value.trim().is_empty()
                            && value.len() <= 256
                            && !value.bytes().any(|byte| byte.is_ascii_control()))
                }) => {}
        _ => return Err(fail("synthesized group resolver identity is malformed")),
    }
    if let Some(brief) = group.get("brief")
        && !matches!(brief, Value::String(_))
    {
        return Err(fail("synthesized group brief is malformed"));
    }
    Ok(())
}

fn decode_response(bytes: &[u8]) -> Result<Response, ResolutionError> {
    if bytes.len() > MAX_ASYNC_RESPONSE_BYTES || !bytes.starts_with(ASYNC_RESPONSE_MAGIC) {
        return Err(invalid("unsupported asynchronous response encoding"));
    }
    let mut reader = ResponseReader {
        bytes,
        offset: ASYNC_RESPONSE_MAGIC.len(),
    };
    let response = match reader.byte()? {
        0 => Response::Unresolved(reader.string()?),
        1 => Response::Inconclusive(reader.string()?),
        2 => {
            let brief = match reader.byte()? {
                0 => None,
                1 => Some(reader.string()?),
                _ => return Err(invalid("malformed response brief marker")),
            };
            let count = reader.u32()? as usize;
            if count > 4096 {
                return Err(invalid("too many response paths"));
            }
            let mut writes = BTreeMap::new();
            for _ in 0..count {
                let path = reader.string()?;
                let file = match reader.byte()? {
                    0 => None,
                    1 => {
                        let kind = match reader.byte()? {
                            0 => EntryKind::Regular,
                            1 => EntryKind::Executable,
                            2 => EntryKind::Symlink,
                            _ => return Err(invalid("malformed response entry kind")),
                        };
                        Some(ProposedFile {
                            kind,
                            bytes: reader.bytes()?,
                        })
                    }
                    _ => return Err(invalid("malformed response file marker")),
                };
                if writes.insert(path, file).is_some() {
                    return Err(invalid("duplicate response path"));
                }
            }
            Response::Proposal(Proposal { writes, brief })
        }
        _ => return Err(invalid("malformed asynchronous response kind")),
    };
    if reader.offset != bytes.len() {
        return Err(invalid("trailing bytes in asynchronous response"));
    }
    Ok(response)
}

/// Dispatch the request recorded at `record` to `resolver` once, within
/// its decision's allowance, and accept its proposal only if it stays in
/// scope and drops nothing.
pub fn resolve(
    store: &mut CollaborationStore,
    repo: &Path,
    record: &RecordRef,
    resolver: &mut dyn Resolver,
) -> Result<Resolution, ResolutionError> {
    let request = load_request(store, repo, record)?;
    let identity = resolver.identity();
    let label = format!("{} {}", identity.name, identity.version);
    let allowance = |store: &CollaborationStore| -> Result<Allowance, ResolutionError> {
        Ok(Allowance {
            limit: MAX_SYNTHESIS_ATTEMPTS,
            used: attempts_used(store, &request.decision)?,
        })
    };
    let Some((attempt, row)) = charge(store, &request, record, &label)? else {
        return Ok(Resolution {
            outcome: failed(
                ResolutionFailure::BudgetExhausted,
                format!(
                    "decision {} already used its {MAX_SYNTHESIS_ATTEMPTS} synthesis attempts",
                    request.decision.key()
                ),
            ),
            attempt: None,
            allowance: allowance(store)?,
        });
    };
    // Whatever happens after dispatch is a recorded outcome, never a bare
    // error that leaves the attempt unsettled.
    let outcome = match attempt_once(store, repo, &request, resolver, &identity, attempt) {
        Ok(outcome) => outcome,
        Err(error) => failed(after_dispatch(&error), format!("{}: {error}", error.code())),
    };
    settle(store, row, outcome.code())?;
    Ok(Resolution {
        outcome,
        attempt: Some(attempt),
        allowance: allowance(store)?,
    })
}

/// A proposal the archive refuses to name is out of scope; anything else
/// after dispatch is the infrastructure's.
fn after_dispatch(error: &ResolutionError) -> ResolutionFailure {
    let archive = match error {
        ResolutionError::Archive(error)
        | ResolutionError::Compose(ComposeError::Archive(error)) => Some(error),
        _ => None,
    };
    match archive {
        Some(
            ArchiveError::UnsupportedFilter { .. }
            | ArchiveError::UnsupportedEntry { .. }
            | ArchiveError::InvalidSnapshot(_),
        ) => ResolutionFailure::ScopeViolation,
        _ => ResolutionFailure::InfrastructureDeferred,
    }
}

fn failed(state: ResolutionFailure, detail: impl Into<String>) -> ResolutionOutcome {
    ResolutionOutcome::Failed {
        state,
        detail: detail.into(),
    }
}

fn attempt_once(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &ResolutionRequest,
    resolver: &mut dyn Resolver,
    identity: &ResolverIdentity,
    attempt: u32,
) -> Result<ResolutionOutcome, ResolutionError> {
    let response = {
        let inputs = ResolutionInputs { store, request };
        resolver.propose(request, &inputs)
    };
    apply_response(store, repo, request, response, identity, attempt)
}

fn apply_response(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &ResolutionRequest,
    response: Result<Response, String>,
    identity: &ResolverIdentity,
    attempt: u32,
) -> Result<ResolutionOutcome, ResolutionError> {
    let proposal = match response {
        Ok(Response::Proposal(proposal)) => proposal,
        Ok(Response::Unresolved(detail)) => {
            return Ok(failed(ResolutionFailure::Unresolved, detail));
        }
        Ok(Response::Inconclusive(detail)) => {
            return Ok(failed(ResolutionFailure::Inconclusive, detail));
        }
        Err(detail) => return Ok(failed(ResolutionFailure::InfrastructureDeferred, detail)),
    };

    let accumulator = composer::snapshot_entries(store, &request.accumulator.snapshot_id)?;
    let violations = scope_violations(request, &accumulator, &proposal);
    if !violations.is_empty() {
        return Ok(failed(
            ResolutionFailure::ScopeViolation,
            violations.join("; "),
        ));
    }

    let mut entries: Entries = accumulator.clone();
    let mut blobs: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
    for (path, file) in &proposal.writes {
        match file {
            Some(file) => {
                let digest: [u8; 32] = Sha256::digest(&file.bytes).into();
                blobs.insert(digest, file.bytes.clone());
                entries.insert(path.as_bytes().to_vec(), (file.kind, digest));
            }
            None => {
                entries.remove(path.as_bytes());
            }
        }
    }
    // Keeping one side, or dropping a change, is unresolved even when it
    // amounts to writing nothing at all.
    let mut contents = Contents {
        store,
        blobs: &blobs,
        manifests: HashMap::new(),
    };
    if let Some(detail) = keeps_one_side(&mut contents, request, &accumulator, &entries)? {
        return Ok(failed(ResolutionFailure::Unresolved, detail));
    }
    if let Some(detail) = drops_a_change(&mut contents, request, &accumulator, &entries)? {
        return Ok(failed(ResolutionFailure::Unresolved, detail));
    }
    if entries == accumulator {
        return Ok(failed(
            ResolutionFailure::Inconclusive,
            "the proposal changes nothing",
        ));
    }

    let (subject, tree, commit) =
        composer::materialize_candidate(store, repo, &request.baseline, &entries, blobs)?;
    // The candidate holds everything in the accumulator plus the members
    // resolved into it, and names every one.
    let mut inputs: Vec<&GroupMember> = request.contents.iter().collect();
    for member in &request.members {
        if !inputs.iter().any(|m| m.lineage == member.lineage) {
            inputs.push(member);
        }
    }
    let candidate = Candidate {
        subject,
        tree,
        commit,
        baseline: request.baseline.commit.clone(),
        baseline_source: COMPOSER_BASELINE_SOURCE,
        inputs: inputs
            .iter()
            .map(|m| CandidateInput {
                base: m.base_commit.clone(),
                result: m.result_commit.clone(),
                base_snapshot: Some(m.base.clone()),
                result_snapshot: Some(m.result.clone()),
            })
            .collect(),
        producer: Producer::Resolver {
            profile: SYNTHESIS_PROFILE.to_string(),
        },
        mode: CompositionMode::Synthesized,
    };
    let mut fields = vec![
        ("schema", text(SYNTHESIZED_GROUP_SCHEMA_NAME)),
        ("request", text(request.record().id.as_str())),
        ("decision", text(&request.decision.key())),
        (
            "constituents",
            Value::Array(request.members.iter().map(member_value).collect()),
        ),
        (
            "contents",
            Value::Array(inputs.iter().map(|m| member_value(m)).collect()),
        ),
        (
            "derived_from",
            Value::Array(inputs.iter().map(|m| text(&m.id)).collect()),
        ),
        (
            "resolver",
            object(vec![
                ("name", text(&identity.name)),
                ("version", text(&identity.version)),
            ]),
        ),
        ("attempt", Value::Integer(i64::from(attempt))),
        ("baseline", text(request.baseline.snapshot_id.as_str())),
        (
            "accumulator",
            text(request.accumulator.snapshot_id.as_str()),
        ),
        ("candidate", text(candidate.subject.as_str())),
        ("candidate_commit", text(candidate.commit.as_str())),
        ("profile", text(SYNTHESIS_PROFILE)),
        ("requires_verification", Value::Bool(true)),
    ];
    if let Some(brief) = &proposal.brief {
        fields.push(("brief", text(brief)));
    }
    let group_record = store_record(store, &SYNTHESIZED_GROUP_SCHEMA, fields)?;
    Ok(ResolutionOutcome::Synthesized(Box::new(
        SynthesizedCandidate {
            candidate,
            group_record,
        },
    )))
}

/// Every way `proposal` leaves its scope (T23).
fn scope_violations(
    request: &ResolutionRequest,
    accumulator: &Entries,
    proposal: &Proposal,
) -> Vec<String> {
    let mut violations = Vec::new();
    for (path, file) in &proposal.writes {
        if let Some(reason) = request.protection.reason(path) {
            violations.push(reason);
            continue;
        }
        if !request.scope.iter().any(|scoped| scoped == path) {
            violations.push(format!("{path} is outside the permitted paths"));
            continue;
        }
        let before = accumulator.get(path.as_bytes()).map(|(kind, _)| *kind);
        if before == Some(EntryKind::Symlink) {
            violations.push(format!("{path} is a symlink; its target cannot change"));
            continue;
        }
        if let Some(file) = file {
            match file.kind {
                EntryKind::Symlink => {
                    violations.push(format!("{path} would become a symlink"));
                }
                EntryKind::Executable if before != Some(EntryKind::Executable) => {
                    violations.push(format!("{path} would become executable"));
                }
                _ => {}
            }
        }
    }
    violations
}

/// Content by entry, read once, for comparisons that ignore whitespace and
/// line endings.
struct Contents<'a> {
    store: &'a CollaborationStore,
    blobs: &'a HashMap<[u8; 32], Vec<u8>>,
    manifests: HashMap<SourceSnapshotId, Entries>,
}

impl Contents<'_> {
    fn entries(&mut self, id: &SourceSnapshotId) -> Result<&Entries, ResolutionError> {
        if !self.manifests.contains_key(id) {
            let entries = composer::snapshot_entries(self.store, id)?;
            self.manifests.insert(id.clone(), entries);
        }
        Ok(&self.manifests[id])
    }

    /// The entry, normalized: its kind and its bytes with trailing
    /// whitespace dropped from every line, CRLF read as LF, and trailing
    /// blank lines ignored. Binary content is compared exactly.
    fn normalized(
        &self,
        entry: Option<Entry>,
    ) -> Result<Option<(EntryKind, Vec<u8>)>, ResolutionError> {
        let Some((kind, digest)) = entry else {
            return Ok(None);
        };
        let bytes = match self.blobs.get(&digest) {
            Some(bytes) => bytes.clone(),
            None => composer::blob(self.store, &digest)?,
        };
        if bytes.iter().take(8000).any(|b| *b == 0) {
            return Ok(Some((kind, bytes)));
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text
            .split('\n')
            .map(|line| line.trim_end_matches(['\r', ' ', '\t']))
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        Ok(Some((kind, lines.join("\n").into_bytes())))
    }

    fn same(&self, a: Option<Entry>, b: Option<Entry>) -> Result<bool, ResolutionError> {
        if a == b {
            return Ok(true);
        }
        Ok(self.normalized(a)? == self.normalized(b)?)
    }
}

/// For a conflict with no authorized preference, a proposal that leaves a
/// conflicting path as one side had it, up to whitespace and line endings,
/// drops the other side's change: a last writer, not a resolution.
fn keeps_one_side(
    contents: &mut Contents<'_>,
    request: &ResolutionRequest,
    accumulator: &Entries,
    candidate: &Entries,
) -> Result<Option<String>, ResolutionError> {
    let Trigger::Conflict(conflicts) = &request.trigger else {
        return Ok(None);
    };
    let preferred = |id: &str| {
        request
            .preference
            .as_ref()
            .is_some_and(|preference| preference.prefer == id)
    };
    for conflict in conflicts {
        let Some(member) = request
            .members
            .iter()
            .find(|m| m.result_commit == conflict.input)
        else {
            continue;
        };
        let path = conflict.path.as_bytes();
        let proposed = candidate.get(path).copied();
        let ours = accumulator.get(path).copied();
        let theirs = contents.entries(&member.result)?.get(path).copied();
        let is_theirs = contents.same(proposed, theirs)?;
        let is_ours = contents.same(proposed, ours)?;
        if is_theirs && !is_ours && !preferred(&member.id) {
            return Ok(Some(format!(
                "{} keeps only {}'s version: no authorized preference chooses it",
                conflict.path, member.id
            )));
        }
        let earlier_preferred = request
            .members
            .iter()
            .any(|m| m.id != member.id && preferred(&m.id));
        if is_ours && !is_theirs && !earlier_preferred {
            return Ok(Some(format!(
                "{} drops {}'s change: no authorized preference chooses the other side",
                conflict.path, member.id
            )));
        }
    }
    Ok(None)
}

/// A proposal may rewrite a contribution's change, but not remove it: on
/// any path a member or an accumulated contribution changed, outside the
/// conflicting paths themselves, the candidate must not be back at that
/// contribution's base where the change was present.
fn drops_a_change(
    contents: &mut Contents<'_>,
    request: &ResolutionRequest,
    accumulator: &Entries,
    candidate: &Entries,
) -> Result<Option<String>, ResolutionError> {
    let conflicting: BTreeSet<&str> = match &request.trigger {
        Trigger::Conflict(conflicts) => conflicts.iter().map(|c| c.path.as_str()).collect(),
        Trigger::FailedChecks { .. } => BTreeSet::new(),
    };
    let mut seen = BTreeSet::new();
    for member in request.contents.iter().chain(&request.members) {
        if !seen.insert(member.lineage.clone()) {
            continue;
        }
        let accumulated = request.contents.iter().any(|m| m.lineage == member.lineage);
        let base = contents.entries(&member.base)?.clone();
        let result = contents.entries(&member.result)?.clone();
        let paths: BTreeSet<&Vec<u8>> = base
            .keys()
            .chain(result.keys())
            .filter(|path| base.get(*path) != result.get(*path))
            .collect();
        for path in paths {
            let shown = String::from_utf8_lossy(path);
            if conflicting.contains(shown.as_ref()) {
                continue;
            }
            let before = base.get(path).copied();
            if contents.same(result.get(path).copied(), before)? {
                continue;
            }
            // Present before the proposal: in the accumulator for what it
            // holds; owed by the proposal for a member not yet applied.
            let present = !accumulated || !contents.same(accumulator.get(path).copied(), before)?;
            if present && contents.same(candidate.get(path).copied(), before)? {
                return Ok(Some(format!(
                    "{shown} drops {}'s change: the candidate has its base version",
                    member.id
                )));
            }
        }
    }
    Ok(None)
}

// ------------------------------------------------------------------ records

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn object(members: Vec<(&str, Value)>) -> Value {
    Value::Object(
        Object::new(
            members
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
        .expect("distinct keys"),
    )
}

fn store_record(
    store: &CollaborationStore,
    schema: &'static RecordSchema,
    members: Vec<(&str, Value)>,
) -> Result<RecordRef, ResolutionError> {
    let bytes = object(members).to_canonical_bytes();
    let id = Record::decode(&bytes, &[schema])
        .map_err(|error| invalid(format!("{}: {error}", schema.name)))?
        .id();
    let object = collaboration_archive::put_object(store, &bytes)?;
    Ok(RecordRef { id, object })
}

/// Read back a record this module wrote, refusing one whose bytes do not
/// decode to its ID.
pub fn read_record(
    store: &CollaborationStore,
    record: &RecordRef,
) -> Result<Record, ResolutionError> {
    let bytes = collaboration_archive::read_object(store, &record.object)?;
    let decoded = Record::decode(
        &bytes,
        &[
            &CANDIDATE_MANIFEST_SCHEMA,
            &RESOLUTION_REQUEST_SCHEMA,
            &SYNTHESIZED_GROUP_SCHEMA,
        ],
    )
    .map_err(|error| invalid(format!("record: {error}")))?;
    if decoded.id() != record.id {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: record.object.hex(),
        }));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_synthesis_trigger_requires_nonempty_checks_and_matching_candidate() {
        let accumulator = SourceSnapshotId::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        let other_candidate =
            SourceSnapshotId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap();
        let input = CommitOid::parse(&"a".repeat(40)).unwrap();
        let conflict = ConflictPath {
            path: "src/lib.rs".into(),
            input,
            reason: "content".into(),
        };
        assert!(historical_synthesis_trigger_is_valid(
            &Trigger::Conflict(vec![conflict]),
            &accumulator,
        ));
        assert!(!historical_synthesis_trigger_is_valid(
            &Trigger::Conflict(Vec::new()),
            &accumulator,
        ));
        assert!(historical_synthesis_trigger_is_valid(
            &Trigger::FailedChecks {
                candidate: accumulator.clone(),
                checks: vec![FailedCheck {
                    check: "native-gate:unit".into(),
                }],
            },
            &accumulator,
        ));
        assert!(!historical_synthesis_trigger_is_valid(
            &Trigger::FailedChecks {
                candidate: accumulator.clone(),
                checks: Vec::new(),
            },
            &accumulator,
        ));
        assert!(!historical_synthesis_trigger_is_valid(
            &Trigger::FailedChecks {
                candidate: other_candidate,
                checks: vec![FailedCheck {
                    check: "native-gate:unit".into(),
                }],
            },
            &accumulator,
        ));
    }

    #[test]
    fn dispatch_limits_stay_within_existing_local_io_caps() {
        let limits = DispatchLimits {
            max_manifest_bytes: collaboration_archive::MAX_METADATA_OBJECT_BYTES,
            max_manifest_entries: crate::execution_snapshot::MAX_FILES as u64,
            max_file_bytes: crate::execution_snapshot::MAX_FILE_BYTES,
            max_total_input_bytes: crate::execution_snapshot::MAX_INPUT_BYTES,
            max_response_bytes: MAX_ASYNC_RESPONSE_BYTES as u64,
        };
        assert_eq!(limits.validate().unwrap(), limits);
        assert_eq!(request_record_limit(limits).unwrap(), MAX_DOCUMENT_BYTES);

        assert!(
            DispatchLimits {
                max_manifest_bytes: collaboration_archive::MAX_METADATA_OBJECT_BYTES + 1,
                ..limits
            }
            .validate()
            .is_err()
        );
        assert!(
            DispatchLimits {
                max_manifest_entries: crate::execution_snapshot::MAX_FILES as u64 + 1,
                ..limits
            }
            .validate()
            .is_err()
        );
        assert!(
            DispatchLimits {
                max_file_bytes: crate::execution_snapshot::MAX_FILE_BYTES + 1,
                ..limits
            }
            .validate()
            .is_err()
        );
        assert!(
            DispatchLimits {
                max_total_input_bytes: crate::execution_snapshot::MAX_INPUT_BYTES + 1,
                ..limits
            }
            .validate()
            .is_err()
        );
        for max_response_bytes in [MAX_ASYNC_RESPONSE_BYTES as u64 + 1, u64::MAX] {
            assert!(
                DispatchLimits {
                    max_response_bytes,
                    ..limits
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn read_only_attempt_status_rejects_pre_v27_schema_typed() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
                 INSERT INTO meta(key,value) VALUES('schema_version','26');",
            )
            .unwrap();
        let error = attempt_status_from_connection(&connection, "attempt:test", "session:test")
            .unwrap_err();
        assert_eq!(error.code(), "unsupported_schema");
        assert!(matches!(
            &error,
            ResolutionError::UnsupportedSchema {
                required: 27,
                actual: 26
            }
        ));
    }

    fn protection() -> Protection {
        Protection {
            harness: [fold_path("pkg/check.sh"), fold_path("harness")]
                .into_iter()
                .collect(),
            gate_files: BTreeSet::new(),
            security_patterns: Vec::new(),
            security: None,
        }
    }

    #[test]
    fn protected_paths_are_matched_under_any_spelling_and_depth() {
        let p = protection();
        for path in [
            ".aethyme/gates.toml",
            ".AETHYME/gates.toml",
            ".Aethyme./gates.toml",
            ".aethy\u{200c}me/gates.toml",
            "sub/.github/workflows/ci.yml",
            ".cargo/config.toml",
            "packages/x/.cargo/config.toml",
            "packages/aethyme/rust/.config/nextest.toml",
            "pkg/.gitattributes",
            "deep/sub/.GITMODULES",
            "rust-toolchain",
            "x/rust-toolchain.toml",
            "packages/aethyme/rust/Cargo.toml",
            "crates/x/build.rs",
            "Cargo.lock",
            "pkg/check.sh",
            "PKG/Check.sh",
            "harness/run.sh",
            ".aethyme",
        ] {
            assert!(p.reason(path).is_some(), "{path}");
        }
        for path in [
            "src/main.rs",
            "app.html",
            "aethyme/gates.toml",
            "pkg/check.sh.txt",
            "harnessed/x",
        ] {
            assert!(p.reason(path).is_none(), "{path}");
        }
    }

    #[test]
    fn requirement_paths_have_one_spelling() {
        for (path, normal) in [
            ("./pkg/check.sh", "pkg/check.sh"),
            ("/pkg/check.sh", "pkg/check.sh"),
            ("pkg//check.sh", "pkg/check.sh"),
            ("pkg/", "pkg"),
            ("pkg", "pkg"),
        ] {
            assert_eq!(normalize_path(path).unwrap(), normal, "{path}");
        }
        assert!(normalize_path("../x").is_err());
        assert!(normalize_path("/").is_err());
    }

    #[test]
    fn a_decision_shares_its_allowance_with_larger_and_smaller_sets() {
        let set = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let decision = Decision {
            kind: "failed_checks",
            members: set(&["a", "b"]),
            paths: BTreeSet::new(),
        };
        assert!(decision.comparable(&set(&["a", "b"]), &BTreeSet::new()));
        assert!(decision.comparable(&set(&["a"]), &BTreeSet::new()));
        assert!(decision.comparable(&set(&["a", "b", "u"]), &BTreeSet::new()));
        assert!(!decision.comparable(&set(&["a", "c"]), &BTreeSet::new()));
        let conflict = Decision {
            kind: "conflict",
            members: set(&["a", "b"]),
            paths: set(&["x"]),
        };
        assert!(!conflict.comparable(&set(&["a", "b"]), &set(&["y"])));
    }

    fn managed_decision_request(members: &[&str], paths: &[&str]) -> ResolutionRequest {
        let set = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        let baseline = RetainedSnapshot {
            snapshot_id: SourceSnapshotId::parse(&format!("sha256:{}", "0".repeat(64))).unwrap(),
            record_id: RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
            commit: CommitOid::parse(&"a".repeat(40)).unwrap(),
            entry_count: 0,
            content_bytes: 0,
        };
        ResolutionRequest {
            record: None,
            decision: Decision {
                kind: "conflict",
                members: set(members),
                paths: set(paths),
            },
            decision_members: Vec::new(),
            members: Vec::new(),
            contents: Vec::new(),
            trigger: Trigger::Conflict(Vec::new()),
            baseline: baseline.clone(),
            accumulator: baseline,
            requirements: Vec::new(),
            scope: Vec::new(),
            protection: Protection {
                harness: BTreeSet::new(),
                gate_files: BTreeSet::new(),
                security_patterns: Vec::new(),
                security: None,
            },
            preference: None,
            allowance: Allowance { limit: 3, used: 0 },
        }
    }

    #[test]
    fn managed_decision_fields_match_canonical_ledger_identity() {
        let request = managed_decision_request(&["b", "a", "b"], &["src/z", "src/a"]);
        let (key, kind, members, paths) = request.managed_decision_fields();
        assert_eq!(key, request.decision_key());
        assert_eq!(kind, "conflict");
        assert_eq!(members, "a\nb");
        assert_eq!(paths, "src/a\nsrc/z");
        assert!(managed_attempt_decision_matches(
            &request, &key, kind, &members, &paths,
        ));
    }

    #[test]
    fn managed_attempt_rows_match_exact_bindings_and_hold_subset_or_superset_decisions() {
        let request = managed_decision_request(&["a", "b"], &["src/a", "src/b"]);
        let key = request.decision.key();
        assert!(managed_attempt_decision_matches(
            &request,
            &key,
            "conflict",
            "a\nb",
            "src/a\nsrc/b",
        ));
        assert!(!managed_attempt_decision_matches(
            &request,
            &key,
            "conflict",
            "b\na",
            "src/a\nsrc/b",
        ));

        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE resolution_attempts (
                    attempt_id INTEGER PRIMARY KEY,
                    decision_key TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    members TEXT NOT NULL,
                    paths TEXT NOT NULL,
                    resolver TEXT NOT NULL,
                    session_label TEXT,
                    outcome TEXT,
                    async_state TEXT
                );",
            )
            .unwrap();

        let set = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        for (members, paths) in [
            (vec!["a"], vec!["src/a"]),
            (vec!["a", "b", "c"], vec!["src/a", "src/b", "src/c"]),
        ] {
            let stored = Decision {
                kind: "conflict",
                members: set(&members),
                paths: set(&paths),
            };
            connection
                .execute(
                    "INSERT INTO resolution_attempts
                     (decision_key,kind,members,paths,resolver,session_label,outcome,async_state)
                     VALUES(?1,?2,?3,?4,'local-managed-resolution',NULL,NULL,'outcome_unknown')",
                    rusqlite::params![
                        stored.key(),
                        stored.kind,
                        join_set(&stored.members),
                        join_set(&stored.paths),
                    ],
                )
                .unwrap();
            assert!(
                has_unresolved_managed_comparable_in_transaction(&connection, &request).unwrap()
            );
            connection
                .execute("DELETE FROM resolution_attempts", [])
                .unwrap();
        }

        let unrelated = Decision {
            kind: "conflict",
            members: set(&["a", "b"]),
            paths: set(&["src/c"]),
        };
        connection
            .execute(
                "INSERT INTO resolution_attempts
                 (decision_key,kind,members,paths,resolver,session_label,outcome,async_state)
                 VALUES(?1,?2,?3,?4,'local-managed-resolution',NULL,NULL,'dispatching')",
                rusqlite::params![
                    unrelated.key(),
                    unrelated.kind,
                    join_set(&unrelated.members),
                    join_set(&unrelated.paths),
                ],
            )
            .unwrap();
        assert!(!has_unresolved_managed_comparable_in_transaction(&connection, &request).unwrap());
    }

    #[test]
    fn asynchronous_response_encoding_is_deterministic_and_round_trips() {
        let response = Response::Proposal(Proposal {
            writes: BTreeMap::from([
                (
                    "src/main.rs".into(),
                    Some(ProposedFile {
                        kind: EntryKind::Regular,
                        bytes: b"fn main() {}\n".to_vec(),
                    }),
                ),
                ("src/old.rs".into(), None),
            ]),
            brief: Some("combine both changes".into()),
        });
        let encoded = encode_response(&response, MAX_ASYNC_RESPONSE_BYTES).unwrap();
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(
            encode_response(&decoded, MAX_ASYNC_RESPONSE_BYTES).unwrap(),
            encoded
        );
        assert_eq!(
            ObjectDigest::of(&encoded),
            ObjectDigest::of(&encode_response(&decoded, MAX_ASYNC_RESPONSE_BYTES).unwrap())
        );
    }

    #[test]
    fn asynchronous_response_decoder_rejects_truncation_and_trailing_bytes() {
        let encoded = encode_response(
            &Response::Unresolved("no safe merge".into()),
            MAX_ASYNC_RESPONSE_BYTES,
        )
        .unwrap();
        assert!(decode_response(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode_response(&trailing).is_err());
    }
}
