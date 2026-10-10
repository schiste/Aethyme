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

use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{
    EntryKind, FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use rusqlite::{OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::collaboration_archive::{self, ArchiveError, CommitOid, ObjectDigest, RetainedSnapshot};
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
    /// The contributions the resolver decides between: for a conflict, those
    /// applied up to and including the conflicting step that touch a
    /// conflicting path; for failed checks, every contribution.
    pub fn members(&self) -> &[GroupMember] {
        &self.members
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
    let decoded = Record::decode(&bytes, &[&RESOLUTION_REQUEST_SCHEMA])
        .map_err(|error| invalid(format!("resolution request record: {error}")))?;
    let mismatch = |detail: String| ResolutionError::RecordMismatch {
        record: record.id.as_str().to_string(),
        detail,
    };
    if decoded.id() != record.id {
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
    let baseline = composer::retain_commit(store, repo, &commit(&string("baseline_commit")?)?)?;
    if baseline.snapshot_id.as_str() != string("baseline")? {
        return Err(mismatch(
            "the baseline commit no longer names its snapshot".into(),
        ));
    }
    let accumulator_id = snapshot(&string("accumulator")?)?;
    let accumulator_commit = commit(&string("accumulator_commit")?)?;
    let retained = collaboration_archive::retained(store, &accumulator_id)?
        .ok_or_else(|| mismatch("the accumulator is no longer retained".into()))?;
    if collaboration_archive::snapshot_of_commit(repo, &accumulator_commit)?.id() != accumulator_id
    {
        return Err(mismatch(
            "the accumulator commit does not name its snapshot".into(),
        ));
    }
    let accumulator = RetainedSnapshot {
        commit: accumulator_commit,
        ..retained
    };
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
        if let Some(gates) = read(".aethyme/gates.toml")?
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
        if let Some(config) = read(".aethyme/config.toml")?
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

/// The ledger is created on first use, outside the numbered migrations:
/// it only adds a table no other reader consults, and the L3 stack claims
/// the next schema numbers until the stacks merge. That migration must
/// adopt a table this statement already created.
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

/// Charge one attempt to `decision` before dispatch, so a crash never
/// refunds it. `None` when the allowance is spent; otherwise the attempt
/// number and the ledger row to settle.
fn charge(
    store: &mut CollaborationStore,
    decision: &Decision,
    request: &RecordId,
    resolver: &str,
) -> Result<Option<(u32, i64)>, ResolutionError> {
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(LEDGER)?;
    let used = comparable_attempts(&transaction, decision)?;
    if used >= MAX_SYNTHESIS_ATTEMPTS {
        return Ok(None);
    }
    transaction.execute(
        "INSERT INTO resolution_attempts
             (decision_key, kind, members, paths, request_record_id, resolver, charged_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            decision.key(),
            decision.kind,
            join_set(&decision.members),
            join_set(&decision.paths),
            request.as_str(),
            resolver,
            now_ms()
        ],
    )?;
    let row = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(Some((used + 1, row)))
}

fn settle(store: &mut CollaborationStore, row: i64, outcome: &str) -> Result<(), ResolutionError> {
    store.connection().execute(
        "UPDATE resolution_attempts SET outcome = ?2 WHERE attempt_id = ?1",
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
    let Some((attempt, row)) = charge(store, &request.decision, &record.id, &label)? else {
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
}
