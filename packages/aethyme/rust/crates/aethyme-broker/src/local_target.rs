//! Provisional Local L5 target model (#666). A trusted local operator appoints
//! an issuer; its guard is scoped to one project and target and holds an OS
//! process lock. This is cooperative same-user authority, not authentication
//! across machines or protection against arbitrary direct SQLite writers.
//!
//! Selection produces an immutable canonical record, with retained inputs.
//! Incoming captures cannot change the active revision. Execution scheduling,
//! verified evidence, provider acceptance and restore authority are separate
//! packages; this model never manufactures a delivered disposition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_archive::{self as archive, ArchiveError, ObjectDigest};
use crate::collaboration_gc::archive_use;
use crate::collaboration_state::CollaborationStore;
use crate::file_lock::{ExclusiveFileLock, open_lock_file};

pub const TARGET_REVISION_SCHEMA_NAME: &str = "aethyme.target-revision/provisional-v0";

pub static TARGET_REVISION_SCHEMA: RecordSchema = RecordSchema {
    name: TARGET_REVISION_SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "target",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "issuer",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "spec",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
    capabilities: &[],
};

/// Exact retained lineage plus the provisional composer's dependency semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedContribution {
    pub id: String,
    pub lineage: String,
    pub requires: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atomic_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_of: Option<String>,
    pub derived_from: Vec<String>,
}

/// Immutable issuer-authorized input. Record references use #653's exact ID
/// encoding; the acceptance policy and inherited requirements are pinned,
/// not a mutable path. Selection order remains significant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    pub objective_authorization: String,
    pub objective: String,
    pub baseline: String,
    pub contributions: Vec<RequestedContribution>,
    pub selected: Vec<String>,
    pub requirements: Vec<String>,
    pub acceptance_policy: String,
    pub composer_profile: String,
}

impl TargetSpec {
    pub(crate) fn catalog(&self) -> Result<Vec<crate::composer::ContributionSpec>, TargetError> {
        self.contributions
            .iter()
            .map(|c| {
                Ok(crate::composer::ContributionSpec {
                    id: c.id.clone(),
                    lineage: Some(record_id(&c.lineage)?),
                    requires: c.requires.clone(),
                    atomic_group: c.atomic_group.clone(),
                    revision_of: c.revision_of.clone(),
                    derived_from: c.derived_from.clone(),
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRevision {
    pub id: RecordId,
    pub target: String,
    pub issuer: String,
    pub spec: TargetSpec,
}

/// The full model, including later acceptance phases. The current runtime
/// refuses to enter acceptance phases without the future trusted integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Disabled,
    Idle,
    Admitting,
    WaveFrozen,
    Composing,
    Verifying,
    ReadyForAcceptance,
    AwaitingProviderOrReview,
    Accepted,
    PublicationConfirmed,
    WaitingForInput,
    WaitingForCapacity,
    WaitingForBudget,
    BlockedRequirement,
    BlockedTargetDrift,
    StopRequested,
    Stopped,
    OutcomeUnknown,
}

impl Phase {
    fn code(self) -> String {
        serde_json::to_value(self)
            .expect("enum serializes")
            .as_str()
            .expect("string enum")
            .into()
    }

    fn parse(code: &str) -> Result<Self, TargetError> {
        serde_json::from_value(serde_json::Value::String(code.into()))
            .map_err(|_| TargetError::Corrupt(format!("unknown controller phase {code}")))
    }

    fn working(self) -> bool {
        matches!(
            self,
            Self::Admitting
                | Self::WaveFrozen
                | Self::Composing
                | Self::Verifying
                | Self::ReadyForAcceptance
                | Self::AwaitingProviderOrReview
        )
    }

    fn waiting(self) -> bool {
        matches!(
            self,
            Self::WaitingForInput | Self::WaitingForCapacity | Self::WaitingForBudget
        )
    }

    /// State-model relation only, not authorization or a verification receipt.
    pub fn allows(self, next: Self) -> bool {
        use Phase::*;
        matches!(
            (self, next),
            (Disabled, Idle)
                | (Idle, Admitting)
                | (Admitting, WaveFrozen)
                | (WaveFrozen, Composing)
                | (Composing, Verifying)
                | (Verifying, ReadyForAcceptance)
                | (ReadyForAcceptance, AwaitingProviderOrReview)
                | (AwaitingProviderOrReview, Accepted)
                | (Accepted, PublicationConfirmed)
                | (PublicationConfirmed, Idle)
                | (StopRequested, Stopped)
        ) || (self.working()
            && (next.waiting()
                || matches!(
                    next,
                    BlockedRequirement | BlockedTargetDrift | StopRequested | OutcomeUnknown
                )))
            || ((self.waiting()
                || matches!(
                    self,
                    Idle | BlockedRequirement | BlockedTargetDrift | OutcomeUnknown
                ))
                && next == StopRequested)
    }
}

/// These are accounting states, never verification or acceptance claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Pending,
    InWave,
    Blocked,
    Unsupported,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionDisposition {
    pub contribution: String,
    pub status: Disposition,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerStatus {
    pub revision: Option<RecordId>,
    pub phase: Phase,
    pub resume_phase: Option<Phase>,
    pub version: i64,
}

/// Not clonable. Dropping the guard or process releases the target lock.
#[derive(Debug)]
pub struct ControllerOwner {
    project_dir: PathBuf,
    target: String,
    issuer: String,
    _lock: ExclusiveFileLock,
    epoch: crate::local_authority::Epoch,
}

impl ControllerOwner {
    pub fn authority_epoch(&self) -> &crate::local_authority::Epoch {
        &self.epoch
    }
    pub(crate) fn target(&self) -> &str {
        &self.target
    }
    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("invalid target: {0}")]
    Invalid(String),
    #[error("another local controller holds this target")]
    ControllerBusy,
    #[error("issuer or project does not match the appointed target owner")]
    Unauthorized,
    #[error("target or revision is missing")]
    Missing,
    #[error("active target is frozen; propose a new revision after it stops or completes")]
    Frozen,
    #[error("transition is not allowed from {from:?} to {to:?}")]
    Transition { from: Phase, to: Phase },
    #[error("controller state changed since the caller observed it")]
    StaleVersion,
    #[error("trusted verification/provider integration is unavailable")]
    VerificationUnavailable,
    #[error("released revisions cannot be selected; authorize a new revision")]
    Released,
    #[error("corrupt target state: {0}")]
    Corrupt(String),
    #[error("target selection refused: {reason:?}: {detail}")]
    SelectionRefused {
        reason: crate::composition::Refusal,
        detail: String,
    },
    #[error(transparent)]
    Authority(#[from] crate::local_authority::AuthorityError),
    #[error(transparent)]
    Compose(#[from] crate::composer::ComposeError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn nonempty(value: &str, label: &str) -> Result<(), TargetError> {
    if value.trim().is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(TargetError::Invalid(label.into()));
    }
    Ok(())
}

fn record_id(value: &str) -> Result<RecordId, TargetError> {
    RecordId::parse(value).map_err(|_| TargetError::Invalid(format!("record ID {value}")))
}

/// Appoint on first use, or re-acquire the existing appointment. This entry
/// point is for a trusted local operator, never a contribution listener.
pub fn acquire_controller(
    store: &mut CollaborationStore,
    target: &str,
    issuer: &str,
) -> Result<ControllerOwner, TargetError> {
    let (epoch, _authority) = crate::local_authority::acquire(store)?;
    nonempty(target, "target")?;
    nonempty(issuer, "issuer")?;
    let locks = store.project_dir().join("controller-locks");
    std::fs::create_dir_all(&locks)?;
    crate::host_state::protect_host_state_path(&locks, true)?;
    let lock = ExclusiveFileLock::try_acquire(open_lock_file(&locks.join(format!(
        "{}.lock",
        ObjectDigest::of(target.as_bytes()).hex()
    )))?)?
    .ok_or(TargetError::ControllerBusy)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR IGNORE INTO local_targets (target, issuer) VALUES (?1, ?2)",
        (target, issuer),
    )?;
    let appointed: String = tx.query_row(
        "SELECT issuer FROM local_targets WHERE target = ?1",
        [target],
        |r| r.get(0),
    )?;
    if appointed != issuer {
        return Err(TargetError::Unauthorized);
    }
    tx.commit()?;
    Ok(ControllerOwner {
        project_dir: store.project_dir().into(),
        target: target.into(),
        issuer: issuer.into(),
        _lock: lock,
        epoch,
    })
}

pub(crate) fn authorize(
    store: &CollaborationStore,
    owner: &ControllerOwner,
) -> Result<crate::local_authority::Guard, TargetError> {
    let guard = crate::local_authority::guard(store, &owner.epoch)?;
    if store.project_dir() != owner.project_dir {
        return Err(TargetError::Unauthorized);
    }
    let issuer: Option<String> = store
        .read_connection()
        .query_row(
            "SELECT issuer FROM local_targets WHERE target = ?1",
            [&owner.target],
            |r| r.get(0),
        )
        .optional()?;
    if issuer.as_deref() != Some(owner.issuer.as_str()) {
        return Err(TargetError::Unauthorized);
    }
    Ok(guard)
}

fn normalize(mut spec: TargetSpec) -> Result<TargetSpec, TargetError> {
    nonempty(&spec.objective, "objective")?;
    nonempty(&spec.composer_profile, "composer profile")?;
    record_id(&spec.objective_authorization)?;
    record_id(&spec.acceptance_policy)?;
    SourceSnapshotId::parse(&spec.baseline).map_err(|_| TargetError::Invalid("baseline".into()))?;
    if spec.contributions.is_empty() || spec.contributions.len() > 1024 || spec.selected.is_empty()
    {
        return Err(TargetError::Invalid(
            "require 1..1024 contributions and a nonempty selection".into(),
        ));
    }
    fn set(values: &mut [String]) -> Result<(), TargetError> {
        values.sort();
        if values.windows(2).any(|p| p[0] == p[1]) {
            return Err(TargetError::Invalid("duplicate set member".into()));
        }
        Ok(())
    }
    set(&mut spec.requirements)?;
    for r in &spec.requirements {
        record_id(r)?;
    }
    spec.contributions.sort_by(|a, b| a.id.cmp(&b.id));
    for c in &mut spec.contributions {
        nonempty(&c.id, "contribution")?;
        record_id(&c.lineage)?;
        if let Some(group) = &c.atomic_group {
            nonempty(group, "atomic group")?;
        }
        set(&mut c.requires)?;
        set(&mut c.derived_from)?;
    }
    let catalog: BTreeMap<_, _> = spec
        .contributions
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();
    let selected: BTreeSet<_> = spec.selected.iter().map(String::as_str).collect();
    if catalog.len() != spec.contributions.len() || selected.len() != spec.selected.len() {
        return Err(TargetError::Invalid(
            "duplicate contribution or selection".into(),
        ));
    }
    // Different aliases for the same exact input must not grant two obligations.
    let lineages: BTreeSet<_> = spec.contributions.iter().map(|c| &c.lineage).collect();
    if lineages.len() != catalog.len() {
        return Err(TargetError::Invalid("duplicate lineage alias".into()));
    }
    for id in &selected {
        if !catalog.contains_key(id) {
            return Err(TargetError::Invalid(format!("unknown selection {id}")));
        }
    }
    if serde_json::to_vec(&spec)?.len() > 1024 * 1024 {
        return Err(TargetError::Invalid("target too large".into()));
    }
    Ok(spec)
}

fn require_snapshot(store: &CollaborationStore, value: &str) -> Result<(), TargetError> {
    let id = SourceSnapshotId::parse(value).map_err(|_| TargetError::Invalid("snapshot".into()))?;
    let scratch = tempfile::tempdir()?;
    archive::reconstruct(store, &id, scratch.path())?;
    Ok(())
}

/// Persist a proposal and retention roots in one transaction. It does not
/// select it or change a running target. Repeating identical input is idempotent.
pub fn propose_revision(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    spec: TargetSpec,
) -> Result<TargetRevision, TargetError> {
    let _authority = authorize(store, owner)?;
    let spec = normalize(spec)?;
    let _archive = archive_use(store)?;
    require_snapshot(store, &spec.baseline)?;
    for c in &spec.contributions {
        let lineage = record_id(&c.lineage)?;
        let retained = archive::retained_contribution(store, &lineage)?.ok_or_else(|| {
            ArchiveError::NotRetained {
                id: c.lineage.clone(),
            }
        })?;
        require_snapshot(store, retained.base.snapshot_id.as_str())?;
        require_snapshot(store, retained.result.snapshot_id.as_str())?;
    }
    let baseline_id = SourceSnapshotId::parse(&spec.baseline)
        .map_err(|_| TargetError::Invalid("baseline".into()))?;
    let baseline = archive::retained(store, &baseline_id)?.ok_or(TargetError::Missing)?;
    let catalog = spec.catalog()?;
    if let Err((reason, detail)) = crate::composer::validate_retained_selection(
        store,
        baseline,
        catalog,
        spec.selected.clone(),
    )? {
        return Err(TargetError::SelectionRefused { reason, detail });
    }
    let record = Record::decode(&serde_json::to_vec(&serde_json::json!({
        "schema": TARGET_REVISION_SCHEMA_NAME, "target": owner.target, "issuer": owner.issuer, "spec": spec,
    }))?, &[&TARGET_REVISION_SCHEMA]).map_err(|e| TargetError::Invalid(e.to_string()))?;
    let id = record.id();
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let released: Option<bool> = tx
        .query_row(
            "SELECT released FROM local_target_revisions WHERE revision = ?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    if released == Some(true) {
        return Err(TargetError::Released);
    }
    tx.execute(
        "INSERT OR IGNORE INTO local_target_revisions (revision, target, body) VALUES (?1, ?2, ?3)",
        params![id.as_str(), owner.target, record.canonical_bytes()],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'snapshot', ?2)",
        (id.as_str(), &spec.baseline),
    )?;
    for c in &spec.contributions {
        tx.execute(
            "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'contribution', ?2)",
            (id.as_str(), &c.lineage),
        )?;
        let status = if spec.selected.contains(&c.id) {
            "pending"
        } else {
            "excluded"
        };
        tx.execute(
            "INSERT OR IGNORE INTO local_target_dispositions VALUES (?1, ?2, ?3, NULL)",
            (id.as_str(), &c.id, status),
        )?;
    }
    tx.commit()?;
    Ok(TargetRevision {
        id,
        target: owner.target.clone(),
        issuer: owner.issuer.clone(),
        spec,
    })
}

/// Re-read the immutable record and verify its identity before using it.
pub fn load_revision(
    store: &CollaborationStore,
    id: &RecordId,
) -> Result<TargetRevision, TargetError> {
    let (target, body): (String, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT target, body FROM local_target_revisions WHERE revision = ?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(TargetError::Missing)?;
    let record = Record::decode(&body, &[&TARGET_REVISION_SCHEMA])
        .map_err(|e| TargetError::Corrupt(e.to_string()))?;
    if record.id() != *id || record.canonical_bytes() != body {
        return Err(TargetError::Corrupt("revision digest".into()));
    }
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    if json["target"].as_str() != Some(&target) {
        return Err(TargetError::Corrupt("revision target".into()));
    }
    let issuer = json["issuer"]
        .as_str()
        .ok_or_else(|| TargetError::Corrupt("issuer".into()))?
        .to_string();
    let spec: TargetSpec = serde_json::from_value(json["spec"].clone())?;
    if normalize(spec.clone())? != spec {
        return Err(TargetError::Corrupt("unnormalized target".into()));
    }
    Ok(TargetRevision {
        id: id.clone(),
        target,
        issuer,
        spec,
    })
}

pub fn status(store: &CollaborationStore, target: &str) -> Result<ControllerStatus, TargetError> {
    let (id, phase, resume, version): (Option<String>, String, Option<String>, i64) = store.read_connection().query_row(
        "SELECT active_revision, phase, resume_phase, version FROM local_targets WHERE target = ?1", [target],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?.ok_or(TargetError::Missing)?;
    Ok(ControllerStatus {
        revision: id.as_deref().map(record_id).transpose()?,
        phase: Phase::parse(&phase)?,
        resume_phase: resume.as_deref().map(Phase::parse).transpose()?,
        version,
    })
}

/// Explicit operator selection. A proposal never supersedes an active wave.
pub fn select_revision(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    id: &RecordId,
) -> Result<(), TargetError> {
    let _authority = authorize(store, owner)?;
    let revision = load_revision(store, id)?;
    if revision.target != owner.target || revision.issuer != owner.issuer {
        return Err(TargetError::Unauthorized);
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active_wave: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM local_waves WHERE target=?1 AND phase IN ('frozen','composing','verifying','outcome_unknown'))",
        [&owner.target], |r| r.get(0),
    )?;
    let budget_held: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM local_budget_reservations WHERE target=?1 AND phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown'))", [owner.target()], |r|r.get(0))?;
    if active_wave || budget_held {
        return Err(TargetError::Frozen);
    }
    let released: bool = tx.query_row(
        "SELECT released FROM local_target_revisions WHERE revision = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    if released {
        return Err(TargetError::Released);
    }
    let phase: String = tx.query_row(
        "SELECT phase FROM local_targets WHERE target = ?1",
        [&owner.target],
        |r| r.get(0),
    )?;
    if !matches!(
        Phase::parse(&phase)?,
        Phase::Disabled
            | Phase::Idle
            | Phase::Stopped
            | Phase::BlockedRequirement
            | Phase::BlockedTargetDrift
    ) {
        return Err(TargetError::Frozen);
    }
    tx.execute("UPDATE local_targets SET active_revision = ?2, phase = 'idle', resume_phase = NULL, version = version + 1 WHERE target = ?1", (&owner.target, id.as_str()))?;
    tx.commit()?;
    Ok(())
}

/// Compare-and-swap transitions reject delayed listener work. Acceptance
/// transitions require a future trusted adapter and are refused here.
pub fn transition(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    expected_version: i64,
    next: Phase,
) -> Result<ControllerStatus, TargetError> {
    let _authority = authorize(store, owner)?;
    let observed = status(store, &owner.target)?;
    if observed.version != expected_version {
        return Err(TargetError::StaleVersion);
    }
    if !observed.phase.allows(next) {
        return Err(TargetError::Transition {
            from: observed.phase,
            to: next,
        });
    }
    if matches!(
        next,
        Phase::ReadyForAcceptance
            | Phase::AwaitingProviderOrReview
            | Phase::Accepted
            | Phase::PublicationConfirmed
    ) {
        return Err(TargetError::VerificationUnavailable);
    }
    if observed.revision.is_none() {
        return Err(TargetError::Missing);
    }
    let resume = next.waiting().then(|| observed.phase.code());
    let changed = store.connection().execute(
        "UPDATE local_targets SET phase = ?3, resume_phase = ?4, version = version + 1 WHERE target = ?1 AND version = ?2",
        params![owner.target, expected_version, next.code(), resume],
    )?;
    if changed != 1 {
        return Err(TargetError::StaleVersion);
    }
    status(store, &owner.target)
}

/// Resume only the exact phase that was held, never skip verification.
pub fn resume(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    expected_version: i64,
) -> Result<ControllerStatus, TargetError> {
    let _authority = authorize(store, owner)?;
    let observed = status(store, &owner.target)?;
    if observed.version != expected_version {
        return Err(TargetError::StaleVersion);
    }
    let previous = observed
        .resume_phase
        .filter(|p| p.working())
        .ok_or(TargetError::Frozen)?;
    if !observed.phase.waiting() {
        return Err(TargetError::Frozen);
    }
    let changed = store.connection().execute(
        "UPDATE local_targets SET phase = ?3, resume_phase = NULL, version = version + 1 WHERE target = ?1 AND version = ?2",
        params![owner.target, expected_version, previous.code()],
    )?;
    if changed != 1 {
        return Err(TargetError::StaleVersion);
    }
    status(store, &owner.target)
}

pub fn dispositions(
    store: &CollaborationStore,
    id: &RecordId,
) -> Result<Vec<ContributionDisposition>, TargetError> {
    load_revision(store, id)?;
    let raw: Vec<(String, String, Option<String>)> = store.read_connection().prepare(
        "SELECT contribution, status, detail FROM local_target_dispositions WHERE revision = ?1 ORDER BY contribution",
    )?.query_map([id.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    raw.into_iter()
        .map(|(contribution, code, detail)| {
            Ok(ContributionDisposition {
                contribution,
                status: serde_json::from_value(serde_json::Value::String(code))?,
                detail,
            })
        })
        .collect()
}

/// Attach exact candidate snapshots and record objects to this revision.
/// Callers supply every referenced snapshot; an object root alone is not a
/// promise to retain source named inside an arbitrary record.
pub fn retain_artifacts(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    id: &RecordId,
    snapshots: &[SourceSnapshotId],
    objects: &[ObjectDigest],
) -> Result<(), TargetError> {
    let _authority = authorize(store, owner)?;
    let revision = load_revision(store, id)?;
    if revision.target != owner.target {
        return Err(TargetError::Unauthorized);
    }
    let _archive = archive_use(store)?;
    for snapshot in snapshots {
        require_snapshot(store, snapshot.as_str())?;
    }
    for object in objects {
        archive::read_object(store, object)?;
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let released: bool = tx.query_row(
        "SELECT released FROM local_target_revisions WHERE revision = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    if released {
        return Err(TargetError::Released);
    }
    for snapshot in snapshots {
        tx.execute(
            "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'snapshot', ?2)",
            (id.as_str(), snapshot.as_str()),
        )?;
    }
    for object in objects {
        tx.execute(
            "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'object', ?2)",
            (id.as_str(), object.hex()),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Explicitly end an inactive proposal's retention. Worker death and stop
/// never release source. The historical revision remains readable.
pub fn release_revision(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    id: &RecordId,
) -> Result<(), TargetError> {
    let _authority = authorize(store, owner)?;
    let revision = load_revision(store, id)?;
    if revision.target != owner.target {
        return Err(TargetError::Unauthorized);
    }
    let _archive = archive_use(store)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active_wave: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM local_waves WHERE revision=?1 AND phase IN ('frozen','composing','verifying','outcome_unknown'))",
        [id.as_str()], |r| r.get(0),
    )?;
    let budget_held: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM local_budget_reservations WHERE revision=?1 AND phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown'))", [revision.id.as_str()], |r|r.get(0))?;
    if active_wave || budget_held {
        return Err(TargetError::Frozen);
    }
    let active: Option<String> = tx.query_row(
        "SELECT active_revision FROM local_targets WHERE target = ?1",
        [&owner.target],
        |r| r.get(0),
    )?;
    if active.as_deref() == Some(id.as_str()) {
        return Err(TargetError::Frozen);
    }
    tx.execute(
        "DELETE FROM local_target_roots WHERE revision = ?1",
        [id.as_str()],
    )?;
    tx.execute(
        "UPDATE local_target_revisions SET released = 1 WHERE revision = ?1",
        [id.as_str()],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
