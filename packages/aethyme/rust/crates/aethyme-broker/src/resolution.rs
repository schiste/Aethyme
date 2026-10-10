//! Bounded resolution requests and exact candidate manifests (L4, #665;
//! plan v3 §7.3 steps 6-9, §7.4, §7.5). Provisional until E1 (#650).
//!
//! Three records, all canonical (#653) and kept in the archive:
//!
//! - A **candidate manifest** ([`write_manifest`]) names exactly what one
//!   composition used and produced: every retained input (lineage record,
//!   base and result snapshots), the baseline, the profile, the recipe, the
//!   output subject and the actual mode, plus what is still undecided. A
//!   composition that produced no candidate gets the same record without a
//!   subject, so an attempt is never success-shaped.
//! - A **resolution request** ([`request_resolution`]) turns a conflict, or
//!   a candidate whose independent checks failed, into a bounded decision:
//!   the exact inputs as retained snapshots, references to the independent
//!   requirements, the paths a resolver may write, and the allowance left.
//! - A **synthesized group** ([`resolve`]) records a resolver's accepted
//!   proposal: its constituents, the resolver, the brief, `derived_from`,
//!   and the new candidate it became. That candidate is marked
//!   `requires_verification`: it is checked again as its own candidate, and
//!   nothing about its constituents' analyses carries over.
//!
//! [`resolve`] enforces, in order and mechanically: the allowance (at most
//! [`MAX_SYNTHESIS_ATTEMPTS`] per group, charged before dispatch and keyed
//! by the group's content, so new candidate IDs, contribution names, a
//! rewritten brief or another resolver never replenish it); then scope
//! (writes outside the permitted paths, to protected harness or policy
//! paths, or that widen permissions are `scope_violation`); then, for a
//! conflict with no authorized preference, that the proposal does not just
//! keep one side (`unresolved`, never a last writer). Only then is the
//! proposal materialized as a new candidate through the composer's own
//! path. Every failure is one of `unresolved`, `inconclusive`,
//! `scope_violation`, `infrastructure_deferred` or `budget_exhausted`.
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
use rusqlite::{OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::collaboration_archive::{self, ArchiveError, CommitOid, ObjectDigest, RetainedSnapshot};
use crate::collaboration_state::CollaborationStore;
use crate::composer::{
    self, COMPOSER_BASELINE_SOURCE, ComposeError, Composition, CompositionRequest, Entries, Entry,
};
use crate::composition::{
    Candidate, CandidateInput, CompositionConflict, CompositionMode, CompositionOutcome, Producer,
};

pub const CANDIDATE_MANIFEST_SCHEMA_NAME: &str = "aethyme.candidate-manifest/experimental-v0";
pub const RESOLUTION_REQUEST_SCHEMA_NAME: &str = "aethyme.resolution-request/experimental-v0";
pub const SYNTHESIZED_GROUP_SCHEMA_NAME: &str = "aethyme.synthesized-group/experimental-v0";

/// The producer profile of a candidate a resolver wrote.
pub const SYNTHESIS_PROFILE: &str = "aethyme.resolve.bounded/provisional-v0";

/// Automatic synthesis attempts per resolution group (plan §7.5's
/// provisional experiment; D15 sets the real caps before paid dispatch).
pub const MAX_SYNTHESIS_ATTEMPTS: u32 = 2;

/// Paths no resolver may write, whatever the scope says: the broker's own
/// policy and gate configuration, CI, and Git settings that change how
/// content is read. A path ending in `/` covers everything under it.
pub const PROTECTED_PATHS: &[&str] = &[".aethyme/", ".github/", ".gitattributes", ".gitmodules"];

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
        field("group", FieldKind::String, true),
        field("members", FieldKind::Opaque, true),
        field("trigger", FieldKind::Opaque, true),
        field("baseline", FieldKind::String, true),
        field("baseline_commit", FieldKind::String, true),
        field("accumulator", FieldKind::String, true),
        field("accumulator_commit", FieldKind::String, true),
        field("requirements", FieldKind::Opaque, true),
        field("scope", FieldKind::Opaque, true),
        field("protected", FieldKind::Opaque, true),
        field("preference", FieldKind::Opaque, false),
        field("allowance", FieldKind::Opaque, true),
    ],
    capabilities: &[],
};

pub static SYNTHESIZED_GROUP_SCHEMA: RecordSchema = RecordSchema {
    name: SYNTHESIZED_GROUP_SCHEMA_NAME,
    fields: &[
        field("request", FieldKind::String, true),
        field("group", FieldKind::String, true),
        field("constituents", FieldKind::Opaque, true),
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
            Self::OutOfScopeRead { .. } => "out_of_scope_read",
        }
    }
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

/// Write the manifest of `composition`, which `request` produced in
/// `repo`. `resolutions` are requests already raised for it; they and any
/// conflicts are its unresolved decisions.
pub fn write_manifest(
    store: &CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    composition: &Composition,
    resolutions: &[&ResolutionRequest],
) -> Result<CandidateManifest, ResolutionError> {
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
    let baseline = collaboration_archive::snapshot_of_commit(repo, &request.baseline)
        .ok()
        .map(|snapshot| snapshot.id());
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
            ("record", text(resolution.record.id.as_str())),
        ]));
    }
    let candidate = composition.outcome.candidate();
    let mut members = vec![
        ("schema", text(CANDIDATE_MANIFEST_SCHEMA_NAME)),
        ("outcome", text(composition.outcome.code())),
        ("profile", text(composer::PROVISIONAL_TEXT_PROFILE)),
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
    if let Some(baseline) = &baseline {
        members.push(("baseline", text(baseline.as_str())));
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

/// A contribution in a resolution group, exactly as retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub id: String,
    pub lineage: RecordId,
    pub base: SourceSnapshotId,
    pub result: SourceSnapshotId,
    pub base_commit: CommitOid,
    pub result_commit: CommitOid,
}

/// The contributions one decision concerns. `key` names the group by its
/// content (each member's base and result snapshot), so the same change
/// under another name, commit or operation is the same group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionGroup {
    pub key: String,
    pub members: Vec<GroupMember>,
}

/// A check that failed on a complete candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    pub check: String,
    pub detail: String,
}

/// What raised the request.
#[derive(Debug, Clone)]
pub enum Trigger {
    /// The composition conflicted; these are its conflicts.
    Conflict(Vec<CompositionConflict>),
    /// The candidate composed, but these independent checks failed on it.
    FailedChecks {
        candidate: SourceSnapshotId,
        checks: Vec<FailedCheck>,
    },
}

/// An independent requirement the result must meet. A requirement with a
/// `path` is harness: no resolver may write it.
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

/// Synthesis attempts charged to a group, of [`MAX_SYNTHESIS_ATTEMPTS`].
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

/// One bounded decision.
#[derive(Debug, Clone)]
pub struct ResolutionRequest {
    pub record: RecordRef,
    pub group: ResolutionGroup,
    pub trigger: Trigger,
    pub baseline: RetainedSnapshot,
    /// The state the group's last change was applied to: the baseline plus
    /// every earlier change, for a conflict; the candidate itself, for
    /// failed checks.
    pub accumulator: RetainedSnapshot,
    pub requirements: Vec<RequirementRef>,
    /// The paths a resolver may write: those the group's changes touch.
    pub scope: Vec<String>,
    /// Paths no resolver may write, scope or not.
    pub protected: Vec<String>,
    pub preference: Option<Preference>,
    /// The allowance when the request was raised.
    pub allowance: Allowance,
}

/// Raise a resolution request for `composition`, which `request` produced
/// in `repo`: from its conflicts, or, for a candidate, from `checks` that
/// failed on it. Anything else has nothing to resolve.
pub fn request_resolution(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    composition: &Composition,
    checks: &[FailedCheck],
    requirements: &[RequirementRef],
    preference: Option<Preference>,
) -> Result<ResolutionRequest, ResolutionError> {
    let members = members_of(store, request, &composition.order)?;
    let mut changed = Vec::with_capacity(members.len());
    for member in &members {
        changed.push(changed_paths(store, member)?);
    }
    let baseline = composer::retain_commit(store, repo, &request.baseline)?;
    let (trigger, group, accumulator) = match &composition.outcome {
        CompositionOutcome::Conflict { conflicts, .. } => {
            if !checks.is_empty() {
                return Err(ResolutionError::InvalidRequest(
                    "a conflict has no candidate for checks to fail on".into(),
                ));
            }
            let Some(first) = conflicts.first() else {
                return Err(ResolutionError::InvalidRequest(
                    "a conflict outcome with no conflicts".into(),
                ));
            };
            // The step that conflicted, and every earlier change to a
            // conflicting path: the contributions the decision is between.
            let Some(step) = members
                .iter()
                .position(|member| member.result_commit == first.input)
            else {
                return Err(ResolutionError::InvalidRequest(format!(
                    "conflicting input {} is not in the composition order",
                    first.input.as_str()
                )));
            };
            let paths: BTreeSet<&str> = conflicts.iter().map(|c| c.path.as_str()).collect();
            let mut group: Vec<usize> = changed[..step]
                .iter()
                .enumerate()
                .filter(|(_, touched)| touched.iter().any(|p| paths.contains(p.as_str())))
                .map(|(earlier, _)| earlier)
                .collect();
            group.push(step);
            let accumulator = accumulator_before(store, repo, request, &composition.order, step)?
                .unwrap_or_else(|| baseline.clone());
            (Trigger::Conflict(conflicts.clone()), group, accumulator)
        }
        CompositionOutcome::Candidate(candidate) => {
            if checks.is_empty() {
                return Err(ResolutionError::InvalidRequest(
                    "a candidate with no failed checks has nothing to resolve".into(),
                ));
            }
            // Without a qualified analysis nothing narrows the interaction,
            // so the group is every contribution in the candidate.
            let accumulator = retained_candidate(store, candidate)?;
            (
                Trigger::FailedChecks {
                    candidate: candidate.subject.clone(),
                    checks: checks.to_vec(),
                },
                (0..members.len()).collect(),
                accumulator,
            )
        }
        other => {
            return Err(ResolutionError::InvalidRequest(format!(
                "a {} outcome has nothing to resolve",
                other.code()
            )));
        }
    };
    let scope: Vec<String> = group
        .iter()
        .flat_map(|&index| changed[index].iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let group_members: Vec<GroupMember> = group.iter().map(|&i| members[i].clone()).collect();
    let group = ResolutionGroup {
        key: group_key(&group_members),
        members: group_members,
    };
    let mut protected: BTreeSet<String> = PROTECTED_PATHS.iter().map(|p| p.to_string()).collect();
    protected.extend(requirements.iter().filter_map(|r| r.path.clone()));
    let protected: Vec<String> = protected.into_iter().collect();
    let allowance = Allowance {
        limit: MAX_SYNTHESIS_ATTEMPTS,
        used: attempts_used(store, &group.key)?,
    };

    let trigger_value = match &trigger {
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
                                ("reason", text(c.reason.code())),
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
                Value::Array(
                    checks
                        .iter()
                        .map(|c| {
                            object(vec![("check", text(&c.check)), ("detail", text(&c.detail))])
                        })
                        .collect(),
                ),
            ),
        ]),
    };
    let mut fields = vec![
        ("schema", text(RESOLUTION_REQUEST_SCHEMA_NAME)),
        ("group", text(&group.key)),
        (
            "members",
            Value::Array(group.members.iter().map(member_value).collect()),
        ),
        ("trigger", trigger_value),
        ("baseline", text(baseline.snapshot_id.as_str())),
        ("baseline_commit", text(baseline.commit.as_str())),
        ("accumulator", text(accumulator.snapshot_id.as_str())),
        ("accumulator_commit", text(accumulator.commit.as_str())),
        (
            "requirements",
            Value::Array(
                requirements
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
        (
            "scope",
            Value::Array(scope.iter().map(|p| text(p)).collect()),
        ),
        (
            "protected",
            Value::Array(protected.iter().map(|p| text(p)).collect()),
        ),
        (
            "allowance",
            object(vec![
                ("limit", Value::Integer(i64::from(allowance.limit))),
                ("used", Value::Integer(i64::from(allowance.used))),
            ]),
        ),
    ];
    if let Some(preference) = &preference {
        fields.push((
            "preference",
            object(vec![
                ("authorized_by", text(&preference.authorized_by)),
                ("prefer", text(&preference.prefer)),
            ]),
        ));
    }
    let record = store_record(store, &RESOLUTION_REQUEST_SCHEMA, fields)?;
    Ok(ResolutionRequest {
        record,
        group,
        trigger,
        baseline,
        accumulator,
        requirements: requirements.to_vec(),
        scope,
        protected,
        preference,
        allowance,
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
            .ok_or_else(|| {
                ResolutionError::InvalidRequest(format!("{id} has no lineage record"))
            })?;
        let retained = collaboration_archive::retained_contribution(store, &lineage)?
            .ok_or_else(|| ResolutionError::InvalidRequest(format!("{id} is not retained")))?;
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
                ResolutionError::InvalidRequest(format!(
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
        other => Err(ResolutionError::InvalidRequest(format!(
            "rebuilding the accumulator before step {step} gave {}",
            other.code()
        ))),
    }
}

fn retained_candidate(
    store: &CollaborationStore,
    candidate: &Candidate,
) -> Result<RetainedSnapshot, ResolutionError> {
    let retained =
        collaboration_archive::retained(store, &candidate.subject)?.ok_or_else(|| {
            ResolutionError::InvalidRequest(format!(
                "candidate {} is not retained",
                candidate.subject
            ))
        })?;
    Ok(RetainedSnapshot {
        commit: candidate.commit.clone(),
        ..retained
    })
}

/// The group's name by content: each member's base and result, sorted.
fn group_key(members: &[GroupMember]) -> String {
    let pairs: BTreeSet<String> = members
        .iter()
        .map(|m| format!("{}\0{}", m.base.as_str(), m.result.as_str()))
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme resolution group v0\0");
    for pair in pairs {
        hasher.update(pair.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ----------------------------------------------------------- the allowance

/// The ledger is created on first use, outside the numbered migrations:
/// it only adds a table no other reader consults, and the L2 and L3 stacks
/// claim the next schema numbers until they merge.
const LEDGER: &str = "CREATE TABLE IF NOT EXISTS resolution_attempts (
     group_key TEXT NOT NULL,
     attempt INTEGER NOT NULL,
     request_record_id TEXT NOT NULL,
     resolver TEXT NOT NULL,
     outcome TEXT,
     charged_ms INTEGER NOT NULL,
     PRIMARY KEY (group_key, attempt)
 ) STRICT;";

fn attempts_used(store: &CollaborationStore, key: &str) -> Result<u32, ResolutionError> {
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
    let used: i64 = connection.query_row(
        "SELECT count(*) FROM resolution_attempts WHERE group_key = ?1",
        [key],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(used).unwrap_or(u32::MAX))
}

/// Charge one attempt to `key` before dispatch, so a crash never refunds
/// it. `None` when the allowance is spent.
fn charge(
    store: &mut CollaborationStore,
    key: &str,
    request: &RecordId,
    resolver: &str,
) -> Result<Option<u32>, ResolutionError> {
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(LEDGER)?;
    let used: i64 = transaction.query_row(
        "SELECT count(*) FROM resolution_attempts WHERE group_key = ?1",
        [key],
        |row| row.get(0),
    )?;
    if used >= i64::from(MAX_SYNTHESIS_ATTEMPTS) {
        return Ok(None);
    }
    let attempt = used + 1;
    transaction.execute(
        "INSERT INTO resolution_attempts
             (group_key, attempt, request_record_id, resolver, charged_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![key, attempt, request.as_str(), resolver, now_ms()],
    )?;
    transaction.commit()?;
    Ok(Some(u32::try_from(attempt).unwrap_or(u32::MAX)))
}

fn settle(
    store: &mut CollaborationStore,
    key: &str,
    attempt: u32,
    outcome: &str,
) -> Result<(), ResolutionError> {
    store.connection().execute(
        "UPDATE resolution_attempts SET outcome = ?3 WHERE group_key = ?1 AND attempt = ?2",
        rusqlite::params![key, attempt, outcome],
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
/// baseline, accumulator and members' snapshots.
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
                .group
                .members
                .iter()
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
    pub candidate: Candidate,
    pub group_record: RecordRef,
    /// Always true: a resolver's output is a proposal, verified only by the
    /// trusted checks of the complete candidate.
    pub requires_verification: bool,
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
    /// The allowance after this attempt.
    pub allowance: Allowance,
}

/// Dispatch `request` to `resolver` once, within the group's allowance, and
/// accept its proposal only if it stays in scope.
pub fn resolve(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &ResolutionRequest,
    resolver: &mut dyn Resolver,
) -> Result<Resolution, ResolutionError> {
    let identity = resolver.identity();
    let label = format!("{} {}", identity.name, identity.version);
    let key = &request.group.key;
    let Some(attempt) = charge(store, key, &request.record.id, &label)? else {
        return Ok(Resolution {
            outcome: ResolutionOutcome::Failed {
                state: ResolutionFailure::BudgetExhausted,
                detail: format!(
                    "group {key} already used its {MAX_SYNTHESIS_ATTEMPTS} synthesis attempts"
                ),
            },
            attempt: None,
            allowance: Allowance {
                limit: MAX_SYNTHESIS_ATTEMPTS,
                used: attempts_used(store, key)?,
            },
        });
    };
    let outcome = attempt_once(store, repo, request, resolver, &identity, attempt)?;
    settle(store, key, attempt, outcome.code())?;
    Ok(Resolution {
        outcome,
        attempt: Some(attempt),
        allowance: Allowance {
            limit: MAX_SYNTHESIS_ATTEMPTS,
            used: attempts_used(store, key)?,
        },
    })
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
    if let Some(detail) = keeps_one_side(store, request, &accumulator, &proposal)? {
        return Ok(failed(ResolutionFailure::Unresolved, detail));
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
    if entries == accumulator {
        return Ok(failed(
            ResolutionFailure::Inconclusive,
            "the proposal changes nothing",
        ));
    }
    let (subject, tree, commit) =
        composer::materialize_candidate(store, repo, &request.baseline, &entries, blobs)?;
    let candidate = Candidate {
        subject,
        tree,
        commit,
        baseline: request.baseline.commit.clone(),
        baseline_source: COMPOSER_BASELINE_SOURCE,
        inputs: request
            .group
            .members
            .iter()
            .map(|m| CandidateInput {
                base: m.base_commit.clone(),
                result: m.result_commit.clone(),
                base_snapshot: Some(m.base.clone()),
                result_snapshot: Some(m.result.clone()),
            })
            .collect(),
        producer: Producer::Composer {
            profile: SYNTHESIS_PROFILE.to_string(),
        },
        mode: CompositionMode::Synthesized,
    };
    let mut fields = vec![
        ("schema", text(SYNTHESIZED_GROUP_SCHEMA_NAME)),
        ("request", text(request.record.id.as_str())),
        ("group", text(&request.group.key)),
        (
            "constituents",
            Value::Array(request.group.members.iter().map(member_value).collect()),
        ),
        (
            "derived_from",
            Value::Array(request.group.members.iter().map(|m| text(&m.id)).collect()),
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
            requires_verification: true,
        },
    )))
}

fn is_protected(request: &ResolutionRequest, path: &str) -> bool {
    request
        .protected
        .iter()
        .any(|protected| match protected.strip_suffix('/') {
            Some(directory) => path == directory || path.starts_with(protected.as_str()),
            None => path == protected,
        })
}

/// Every way `proposal` leaves its scope (T23).
fn scope_violations(
    request: &ResolutionRequest,
    accumulator: &Entries,
    proposal: &Proposal,
) -> Vec<String> {
    let mut violations = Vec::new();
    for (path, file) in &proposal.writes {
        if is_protected(request, path) {
            violations.push(format!("{path} is protected harness or policy"));
            continue;
        }
        if !request.scope.iter().any(|scoped| scoped == path) {
            violations.push(format!("{path} is outside the permitted paths"));
            continue;
        }
        if let Some(file) = file {
            let before = accumulator.get(path.as_bytes()).map(|(kind, _)| *kind);
            let widens = match file.kind {
                EntryKind::Executable | EntryKind::Symlink => before != Some(file.kind),
                _ => false,
            };
            if widens {
                violations.push(format!(
                    "{path} would become {} where it was {}",
                    file.kind.git_mode(),
                    before.map_or("absent", |kind| kind.git_mode())
                ));
            }
        }
    }
    violations
}

/// For a conflict with no authorized preference, a proposal that leaves a
/// conflicting path exactly as one side had it drops the other side's
/// change: that is a last writer, not a resolution.
fn keeps_one_side(
    store: &CollaborationStore,
    request: &ResolutionRequest,
    accumulator: &Entries,
    proposal: &Proposal,
) -> Result<Option<String>, ResolutionError> {
    let Trigger::Conflict(conflicts) = &request.trigger else {
        return Ok(None);
    };
    for conflict in conflicts {
        let Some(member) = request
            .group
            .members
            .iter()
            .find(|m| m.result_commit == conflict.input)
        else {
            continue;
        };
        let path = conflict.path.as_bytes();
        let proposed: Option<Entry> = match proposal.writes.get(&conflict.path) {
            Some(Some(file)) => Some((file.kind, Sha256::digest(&file.bytes).into())),
            Some(None) => None,
            None => accumulator.get(path).copied(),
        };
        let ours = accumulator.get(path).copied();
        let theirs = composer::snapshot_entries(store, &member.result)?
            .get(path)
            .copied();
        let preferred = |id: &str| {
            request
                .preference
                .as_ref()
                .is_some_and(|preference| preference.prefer == id)
        };
        if proposed == theirs && proposed != ours && !preferred(&member.id) {
            return Ok(Some(format!(
                "{} keeps only {}'s version: no authorized preference chooses it",
                conflict.path, member.id
            )));
        }
        let earlier_preferred = request
            .group
            .members
            .iter()
            .any(|m| m.id != member.id && preferred(&m.id));
        if proposed == ours && proposed != theirs && !earlier_preferred {
            return Ok(Some(format!(
                "{} drops {}'s change: no authorized preference chooses the other side",
                conflict.path, member.id
            )));
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
        .map_err(|error| ResolutionError::InvalidRequest(format!("{}: {error}", schema.name)))?
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
    .map_err(|error| ResolutionError::InvalidRequest(format!("record: {error}")))?;
    if decoded.id() != record.id {
        return Err(ResolutionError::Archive(ArchiveError::CorruptObject {
            digest: record.object.hex(),
        }));
    }
    Ok(decoded)
}
