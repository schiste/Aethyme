//! Read-only adapter from explicitly approved local project bindings to
//! retained, replay-verified file identity state (experimental D10 profile).
//!
//! ProjectKey names this local archive only. A typed binding can be minted
//! only from the persisted confirmation row plus the exact archived
//! descriptor and anchor. Caller-supplied ProjectIds and FileId maps never
//! establish this association.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionId, ContributionRevision, EntryKind, FileId,
    FileIdentityTransition, FileOperation, IdentityRelation, ProjectDescriptor,
    ProjectFileIdentityState, ProjectId, RecordId, SourceSnapshot, SourceSnapshotId,
    advance_file_identities,
    brief::Brief,
    canonical_json::{Object, Value as CanonicalValue},
    contribution_revision::MAX_CONTRIBUTION_REVISION_BYTES,
    project_identity::MAX_DESCRIPTOR_BYTES,
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use thiserror::Error;

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_capture::OperationId;
use crate::collaboration_state::{CollaborationStore, ProjectKey};

const REQUIRED_SCHEMA: i64 = 26;
const MAX_REPLAY_REVISIONS: usize = 10_000;
const MAX_SNAPSHOT_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

/// An approved mapping for this exact local store. Fields are private so
/// callers cannot manufacture authority from a descriptor or ProjectId.
#[derive(Debug, Clone)]
pub struct ApprovedProjectBinding {
    store_project: ProjectKey,
    store_dir: std::path::PathBuf,
    project_id: ProjectId,
    descriptor: ProjectDescriptor,
    descriptor_record_id: RecordId,
    descriptor_object: ObjectDigest,
    anchor_commit: CommitOid,
    committed_config_commit: CommitOid,
}

impl ApprovedProjectBinding {
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn descriptor(&self) -> &ProjectDescriptor {
        &self.descriptor
    }

    pub fn descriptor_record_id(&self) -> &RecordId {
        &self.descriptor_record_id
    }

    pub fn descriptor_object(&self) -> ObjectDigest {
        self.descriptor_object
    }

    pub fn anchor_commit(&self) -> &CommitOid {
        &self.anchor_commit
    }

    pub fn committed_config_commit(&self) -> &CommitOid {
        &self.committed_config_commit
    }

    fn belongs_to(&self, store: &CollaborationStore) -> bool {
        self.store_project == *store.project() && self.store_dir == store.project_dir()
    }
}

/// Where an exact FileId map came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityStateProvenance {
    DescriptorAnchor {
        descriptor_record_id: RecordId,
        anchor_commit: CommitOid,
        committed_config_commit: CommitOid,
    },
    Replayed {
        revision_record_ids: Vec<RecordId>,
    },
    /// A specific retained revision's candidate result map. This is trusted
    /// for that exact transition from its unique base, but does not assert
    /// that the snapshot has one globally unambiguous FileId map.
    RevisionTransition {
        revision_record_id: RecordId,
        base_revision_record_ids: Vec<RecordId>,
    },
}

/// One exact snapshot and its verified identity map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProjectFileIdentityState {
    store_project: ProjectKey,
    store_dir: std::path::PathBuf,
    descriptor_record_id: RecordId,
    snapshot: SourceSnapshotId,
    state: ProjectFileIdentityState,
    provenance: IdentityStateProvenance,
}

impl ResolvedProjectFileIdentityState {
    pub fn snapshot_id(&self) -> &SourceSnapshotId {
        &self.snapshot
    }

    pub fn descriptor_record_id(&self) -> &RecordId {
        &self.descriptor_record_id
    }

    pub fn state(&self) -> &ProjectFileIdentityState {
        &self.state
    }

    pub fn provenance(&self) -> &IdentityStateProvenance {
        &self.provenance
    }

    fn belongs_to(&self, binding: &ApprovedProjectBinding) -> bool {
        self.store_project == binding.store_project
            && self.store_dir == binding.store_dir
            && self.descriptor_record_id == binding.descriptor_record_id
            && self.snapshot == *self.state.snapshot()
            && self.state.project_id() == &binding.project_id
    }
}

/// A typed non-success outcome for a snapshot that does not have one
/// unambiguous path from the approved descriptor anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityStateUnknownReason {
    SnapshotNotRetained,
    NoValidatedHistory,
    CompetingHistories,
    HistoryDependsOnUnretainedCapture,
}

/// Resolution never treats an unknown or conflicting map as an empty map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityStateResolution {
    Anchor(ResolvedProjectFileIdentityState),
    Replayed(ResolvedProjectFileIdentityState),
    Unknown {
        snapshot: SourceSnapshotId,
        reason: IdentityStateUnknownReason,
    },
}

/// Inputs for appending one immutable revision to an explicitly selected
/// capture operation. A missing file operation list reuses the current head's
/// validated source changes when revising rationale; a first revision uses
/// the bounded inferred profile.
pub struct ContributionRevisionRequest<'a> {
    pub operation_id: &'a OperationId,
    pub file_operations: Option<&'a [FileOperation]>,
    pub brief: Option<&'a Brief>,
    pub depends_on: Option<&'a [RecordId]>,
}

/// An exact retained D10 revision, validated against the approved binding and
/// replayed identity history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContributionRevision {
    revision: ContributionRevision,
    record_id: RecordId,
    operation_id: Option<String>,
    object_digest: ObjectDigest,
    brief_object_digest: Option<ObjectDigest>,
    brief: Option<Brief>,
    capture_record_id: RecordId,
    base_state: ResolvedProjectFileIdentityState,
    result_state: ResolvedProjectFileIdentityState,
    identity_transition: FileIdentityTransition,
}

impl ResolvedContributionRevision {
    pub fn revision(&self) -> &ContributionRevision {
        &self.revision
    }

    pub fn record_id(&self) -> &RecordId {
        &self.record_id
    }

    /// The local capture operation that minted this logical ContributionId;
    /// absent only for a byte-validated received packet not yet indexed here.
    pub fn capture_operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    pub fn object_digest(&self) -> ObjectDigest {
        self.object_digest
    }

    pub fn brief_object_digest(&self) -> Option<ObjectDigest> {
        self.brief_object_digest
    }

    pub fn brief_digest(&self) -> Option<ObjectDigest> {
        self.brief_object_digest
    }

    /// The exact validated immutable rationale attached to this revision.
    pub fn brief(&self) -> Option<&Brief> {
        self.brief.as_ref()
    }

    pub fn capture_record_id(&self) -> &RecordId {
        &self.capture_record_id
    }

    pub fn capture_lineage(&self) -> &RecordId {
        &self.capture_record_id
    }

    pub fn base_state(&self) -> &ResolvedProjectFileIdentityState {
        &self.base_state
    }

    pub fn result_state(&self) -> &ResolvedProjectFileIdentityState {
        &self.result_state
    }

    /// The opaque transition recomputed from this approved binding and the
    /// exact retained base/result snapshots. It can be passed directly to
    /// validate_file_proposals without rebuilding a caller-controlled map.
    pub fn identity_transition(&self) -> &FileIdentityTransition {
        &self.identity_transition
    }
}

#[derive(Debug, Error)]
pub enum IdentityStoreError {
    #[error(
        "local collaboration store schema {actual} is below the required schema {REQUIRED_SCHEMA}"
    )]
    UnsupportedSchema { actual: i64 },
    #[error("local project has not been explicitly bound")]
    Unbound,
    #[error("approved project binding belongs to a different local store")]
    ForeignStore,
    #[error("local project binding or identity history is corrupt: {0}")]
    Corrupt(String),
    #[error("bounded identity replay is limited to {MAX_REPLAY_REVISIONS} revisions")]
    ReplayLimit,
    #[error("revision {0} is not retained in the approved local project")]
    UnknownRevision(String),
    #[error("capture operation {0} has no retained committed receipt")]
    UnknownCaptureOperation(String),
    #[error("legacy capture lineage {0} is ambiguous across logical contributions")]
    AmbiguousCaptureLineage(String),
    #[error(
        "the capture operation or revision head changed while the revision was prepared; retry the same operation"
    )]
    ConcurrentRevision,
    #[error("could not mint a logical ContributionId: {0}")]
    Randomness(#[source] std::io::Error),
    #[error("snapshot identity is unknown: {snapshot}: {reason:?}")]
    UnknownSnapshot {
        snapshot: String,
        reason: IdentityStateUnknownReason,
    },
    #[error("archive error: {0}")]
    Archive(#[from] archive::ArchiveError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sensitive response refused: {0}")]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error("project identity error: {0}")]
    Project(#[from] aethyme_contracts::experimental_v0::ProjectIdentityError),
    #[error("contribution revision error: {0}")]
    Revision(#[from] aethyme_contracts::experimental_v0::ContributionRevisionError),
    #[error("brief record error: {0}")]
    Brief(#[from] aethyme_contracts::experimental_v0::brief::BriefErrors),
    #[error("source snapshot error: {0}")]
    Snapshot(#[from] aethyme_contracts::experimental_v0::SourceSnapshotError),
}

#[derive(Debug, Clone)]
struct RevisionRow {
    record_id: RecordId,
    operation_id: String,
    object_digest: ObjectDigest,
    lineage_record_id: RecordId,
    revision: ContributionRevision,
    brief_object_digest: Option<ObjectDigest>,
    brief: Option<Brief>,
    source_retained: bool,
}

#[derive(Debug, Clone)]
struct SnapshotState {
    state: ProjectFileIdentityState,
    provenance: BTreeSet<RecordId>,
}

#[derive(Debug, Default)]
struct ReplayIndex {
    states: BTreeMap<SourceSnapshotId, SnapshotState>,
    conflicts: BTreeSet<SourceSnapshotId>,
    rows: BTreeMap<RecordId, RevisionRow>,
    transitions: BTreeMap<RecordId, FileIdentityTransition>,
    applied: BTreeSet<RecordId>,
    blocked_by_unretained: BTreeSet<SourceSnapshotId>,
}

/// Resolve only an explicitly confirmed descriptor binding already persisted
/// by the public project-bind command. The local ProjectKey is never promoted
/// to the logical ProjectId.
pub fn resolve_approved_project_binding(
    store: &CollaborationStore,
) -> Result<Option<ApprovedProjectBinding>, IdentityStoreError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    require_schema(store)?;
    let row: Option<(String, String, String, String, String, String, String)> = store
        .read_connection()
        .query_row(
            "SELECT project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
                    anchor_snapshot,confirmed_descriptor_record_id,committed_config_commit
             FROM local_project_bindings WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        project_text,
        descriptor_text,
        descriptor_hex,
        anchor_commit_text,
        anchor_snapshot_text,
        confirmed_text,
        config_commit_text,
    )) = row
    else {
        return Ok(None);
    };

    let project_id = ProjectId::parse(&project_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let descriptor_record_id = RecordId::parse(&descriptor_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    if confirmed_text != descriptor_text {
        return Err(IdentityStoreError::Corrupt(
            "descriptor confirmation does not match the stored descriptor RecordId".into(),
        ));
    }
    let descriptor_object = archive::parse_digest(&descriptor_hex)
        .ok_or_else(|| IdentityStoreError::Corrupt("invalid descriptor object digest".into()))?;
    let anchor_commit = CommitOid::parse(&anchor_commit_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let committed_config_commit = CommitOid::parse(&config_commit_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let descriptor_bytes =
        archive::read_object_bounded(store, &descriptor_object, MAX_DESCRIPTOR_BYTES)?;
    let (descriptor, observed_id) = ProjectDescriptor::from_record(&descriptor_bytes)?;
    if observed_id != descriptor_record_id
        || descriptor.project_id() != &project_id
        || descriptor.anchor_commit() != anchor_commit.as_str()
        || descriptor.anchor_snapshot().as_str() != anchor_snapshot_text
    {
        return Err(IdentityStoreError::Corrupt(
            "archived descriptor differs from the confirmed local binding row".into(),
        ));
    }

    let retained_anchor = archive::retained(store, descriptor.anchor_snapshot())?
        .ok_or_else(|| IdentityStoreError::Corrupt("descriptor anchor is not retained".into()))?;
    if retained_anchor.commit != anchor_commit {
        return Err(IdentityStoreError::Corrupt(
            "retained anchor commit differs from the descriptor".into(),
        ));
    }
    let anchor_source = read_retained_snapshot(store, descriptor.anchor_snapshot())?;
    if anchor_source != descriptor.anchor_source()? {
        return Err(IdentityStoreError::Corrupt(
            "retained anchor manifest differs from the descriptor file inventory".into(),
        ));
    }
    validate_anchor_file_rows(store, &descriptor)?;

    Ok(Some(ApprovedProjectBinding {
        store_project: store.project().clone(),
        store_dir: store.project_dir().to_path_buf(),
        project_id,
        descriptor,
        descriptor_record_id,
        descriptor_object,
        anchor_commit,
        committed_config_commit,
    }))
}

/// Resolve the exact identity map for a retained snapshot by replaying
/// immutable D10 revisions from the approved descriptor anchor. Ambiguous
/// competing maps and histories that depend on released captures remain
/// explicit Unknown results.
pub fn resolve_project_file_identity_state(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    snapshot: &SourceSnapshotId,
) -> Result<IdentityStateResolution, IdentityStoreError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_binding_store(store, binding)?;
    require_schema(store)?;
    if archive::retained(store, snapshot)?.is_none() {
        return Ok(IdentityStateResolution::Unknown {
            snapshot: snapshot.clone(),
            reason: IdentityStateUnknownReason::SnapshotNotRetained,
        });
    }
    let replay = replay_history(store, binding)?;
    validate_cached_maps(store, &replay)?;
    if replay.conflicts.contains(snapshot) {
        return Ok(IdentityStateResolution::Unknown {
            snapshot: snapshot.clone(),
            reason: if replay.blocked_by_unretained.contains(snapshot) {
                IdentityStateUnknownReason::HistoryDependsOnUnretainedCapture
            } else {
                IdentityStateUnknownReason::CompetingHistories
            },
        });
    }
    let Some(state) = replay.states.get(snapshot) else {
        return Ok(IdentityStateResolution::Unknown {
            snapshot: snapshot.clone(),
            reason: IdentityStateUnknownReason::NoValidatedHistory,
        });
    };
    let resolved = resolved_state(binding, snapshot, state);
    if snapshot == binding.descriptor.anchor_snapshot() {
        Ok(IdentityStateResolution::Anchor(resolved))
    } else {
        Ok(IdentityStateResolution::Replayed(resolved))
    }
}

/// Resolve a target-pinned immutable revision only after validating the exact
/// archive object, brief reference, capture lineage, project binding,
/// ContributionId seed, and the base-to-result identity transition.
pub fn resolve_bound_contribution_revision(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    revision_record_id: &RecordId,
    expected_revision_object: ObjectDigest,
    expected_contribution: &ContributionId,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_binding_store(store, binding)?;
    require_schema(store)?;
    let replay = replay_history(store, binding)?;
    validate_cached_maps(store, &replay)?;
    let Some(row) = replay.rows.get(revision_record_id) else {
        return Err(IdentityStoreError::UnknownRevision(
            revision_record_id.as_str().to_string(),
        ));
    };
    if row.object_digest != expected_revision_object
        || row.revision.contribution_id() != expected_contribution
    {
        return Err(IdentityStoreError::Corrupt(
            "target revision pin differs from the retained revision object".into(),
        ));
    }
    let base = unique_state(&replay, row.revision.base())?;
    if !row.source_retained
        || archive::retained(store, row.revision.base())?.is_none()
        || archive::retained(store, row.revision.result())?.is_none()
    {
        return Err(IdentityStoreError::UnknownSnapshot {
            snapshot: row.revision.result().as_str().to_string(),
            reason: IdentityStateUnknownReason::SnapshotNotRetained,
        });
    }
    let transition = replay
        .transitions
        .get(revision_record_id)
        .cloned()
        .ok_or_else(|| IdentityStoreError::UnknownSnapshot {
            snapshot: row.revision.result().as_str().to_string(),
            reason: if replay.blocked_by_unretained.contains(row.revision.result()) {
                IdentityStateUnknownReason::HistoryDependsOnUnretainedCapture
            } else {
                IdentityStateUnknownReason::NoValidatedHistory
            },
        })?;
    let base_revision_record_ids = match &base.provenance {
        IdentityStateProvenance::DescriptorAnchor { .. } => Vec::new(),
        IdentityStateProvenance::Replayed {
            revision_record_ids,
        } => revision_record_ids.clone(),
        IdentityStateProvenance::RevisionTransition {
            revision_record_id,
            base_revision_record_ids,
        } => {
            let mut ids = base_revision_record_ids.clone();
            ids.push(revision_record_id.clone());
            ids.sort();
            ids.dedup();
            ids
        }
    };
    let result_state = ResolvedProjectFileIdentityState {
        store_project: binding.store_project.clone(),
        store_dir: binding.store_dir.clone(),
        descriptor_record_id: binding.descriptor_record_id.clone(),
        snapshot: row.revision.result().clone(),
        state: transition.result_identity_state(),
        provenance: IdentityStateProvenance::RevisionTransition {
            revision_record_id: row.record_id.clone(),
            base_revision_record_ids,
        },
    };
    Ok(ResolvedContributionRevision {
        revision: row.revision.clone(),
        record_id: row.record_id.clone(),
        operation_id: Some(row.operation_id.clone()),
        object_digest: row.object_digest,
        brief_object_digest: row.brief_object_digest,
        brief: row.brief.clone(),
        capture_record_id: row.lineage_record_id.clone(),
        base_state: resolved_state(binding, row.revision.base(), base),
        result_state,
        identity_transition: transition,
    })
}

/// Validate exact received revision bytes without requiring a local
/// `contribution_revisions` row. The base state must first be resolved from
/// this store's approved descriptor/history; both source snapshots must be
/// retained in this store. This is suitable for validating an imported D10
/// packet before indexing its immutable revision record.
#[allow(clippy::too_many_arguments)]
pub fn validate_bound_contribution_revision_bytes(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    base_state: &ResolvedProjectFileIdentityState,
    expected_record_id: &RecordId,
    expected_object_digest: ObjectDigest,
    expected_contribution: &ContributionId,
    expected_capture_lineage: Option<&RecordId>,
    revision_bytes: &[u8],
    brief_bytes: Option<&[u8]>,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_binding_store(store, binding)?;
    require_schema(store)?;
    if !base_state.belongs_to(binding) {
        return Err(IdentityStoreError::ForeignStore);
    }

    let replay = replay_history(store, binding)?;
    validate_cached_maps(store, &replay)?;
    let trusted_base = unique_state(&replay, base_state.snapshot_id())?;
    if trusted_base.state != *base_state.state() {
        return Err(IdentityStoreError::Corrupt(
            "received revision base state differs from replay-verified local history".into(),
        ));
    }
    let verified_base = resolved_state(binding, base_state.snapshot_id(), trusted_base);

    if ObjectDigest::of(revision_bytes) != expected_object_digest {
        return Err(IdentityStoreError::Corrupt(
            "received revision bytes differ from the pinned object digest".into(),
        ));
    }
    let (revision, observed_record_id) = ContributionRevision::from_record(revision_bytes)?;
    if observed_record_id != *expected_record_id
        || revision.project_id() != binding.project_id()
        || revision.contribution_id() != expected_contribution
        || expected_capture_lineage.is_some_and(|lineage| revision.capture_record_id() != lineage)
    {
        return Err(IdentityStoreError::Corrupt(
            "received revision differs from its pinned record, project, contribution, or lineage"
                .into(),
        ));
    }
    if revision.base() != base_state.snapshot_id() {
        return Err(IdentityStoreError::Corrupt(
            "received revision base snapshot differs from the trusted identity state".into(),
        ));
    }

    let (brief_object_digest, brief) = match (revision.brief(), brief_bytes) {
        (None, None) => (None, None),
        (Some(reference), Some(bytes)) => {
            let digest = ObjectDigest::of(bytes);
            if digest.hex().as_str() != reference.object_sha256.as_str() {
                return Err(IdentityStoreError::Corrupt(
                    "received brief bytes differ from the immutable revision digest".into(),
                ));
            }
            let (brief, observed_brief_id) = Brief::from_record(bytes)?;
            if observed_brief_id != reference.record_id {
                return Err(IdentityStoreError::Corrupt(
                    "received brief bytes differ from the immutable revision RecordId".into(),
                ));
            }
            (Some(digest), Some(brief))
        }
        _ => {
            return Err(IdentityStoreError::Corrupt(
                "received revision brief reference and exact brief bytes disagree".into(),
            ));
        }
    };

    for snapshot in [revision.base(), revision.result()] {
        if archive::retained(store, snapshot)?.is_none() {
            return Err(IdentityStoreError::UnknownSnapshot {
                snapshot: snapshot.as_str().to_string(),
                reason: IdentityStateUnknownReason::SnapshotNotRetained,
            });
        }
    }
    let base = read_retained_snapshot(store, revision.base())?;
    let result = read_retained_snapshot(store, revision.result())?;
    let operations = explicit_operations(&revision, verified_base.state())?;
    let transition = advance_file_identities(
        binding.project_id(),
        verified_base.state(),
        revision.contribution_id().as_str(),
        &base,
        &result,
        &operations,
    )
    .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let computed: Vec<_> = transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    if computed.as_slice() != revision.source_changes() {
        return Err(IdentityStoreError::Corrupt(
            "received source changes or FileIds differ from the validated transition".into(),
        ));
    }

    let base_revision_record_ids: Vec<_> = provenance_ids(verified_base.provenance())
        .into_iter()
        .collect();
    let result_state = ResolvedProjectFileIdentityState {
        store_project: binding.store_project.clone(),
        store_dir: binding.store_dir.clone(),
        descriptor_record_id: binding.descriptor_record_id.clone(),
        snapshot: revision.result().clone(),
        state: transition.result_identity_state(),
        provenance: IdentityStateProvenance::RevisionTransition {
            revision_record_id: observed_record_id.clone(),
            base_revision_record_ids,
        },
    };
    let capture_record_id = revision.capture_record_id().clone();
    Ok(ResolvedContributionRevision {
        revision,
        record_id: observed_record_id,
        operation_id: None,
        object_digest: expected_object_digest,
        brief_object_digest,
        brief,
        capture_record_id,
        base_state: verified_base,
        result_state,
        identity_transition: transition,
    })
}

/// Append an immutable D10 revision for one exact retained capture operation.
/// The capture OperationId is the durable retry selector; its first revision
/// atomically persists the fresh ContributionId mapping with the revision and
/// current head. Content-derived capture lineage is never used to mint or
/// choose a logical ContributionId.
pub fn revise_contribution_for_operation(
    store: &mut CollaborationStore,
    binding: &ApprovedProjectBinding,
    request: ContributionRevisionRequest<'_>,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_binding_store(store, binding)?;
    require_schema(store)?;

    let operation_id = request.operation_id.as_str();
    let capture: Option<(String, String, String, String)> = store
        .read_connection()
        .query_row(
            "SELECT r.lineage_record_id,r.base_snapshot,r.result_snapshot,o.state
             FROM capture_receipts r
             JOIN capture_operations o ON o.operation_id=r.operation_id
             WHERE r.operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((lineage_text, base_text, result_text, operation_state)) = capture else {
        return Err(IdentityStoreError::UnknownCaptureOperation(
            operation_id.to_string(),
        ));
    };
    if !matches!(operation_state.as_str(), "committed" | "acknowledged") {
        return Err(IdentityStoreError::UnknownCaptureOperation(
            operation_id.to_string(),
        ));
    }
    let lineage_record_id = RecordId::parse(&lineage_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let base_snapshot = SourceSnapshotId::parse(&base_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let result_snapshot = SourceSnapshotId::parse(&result_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let retained = archive::retained_contribution(store, &lineage_record_id)?
        .ok_or_else(|| IdentityStoreError::UnknownCaptureOperation(operation_id.to_string()))?;
    if retained.base.snapshot_id != base_snapshot || retained.result.snapshot_id != result_snapshot
    {
        return Err(IdentityStoreError::Corrupt(
            "capture receipt and retained lineage snapshots disagree".into(),
        ));
    }

    let base_state = match resolve_project_file_identity_state(store, binding, &base_snapshot)? {
        IdentityStateResolution::Anchor(state) | IdentityStateResolution::Replayed(state) => state,
        IdentityStateResolution::Unknown { snapshot, reason } => {
            return Err(IdentityStoreError::UnknownSnapshot {
                snapshot: snapshot.as_str().to_string(),
                reason,
            });
        }
    };
    let base_source = read_retained_snapshot(store, &base_snapshot)?;
    let result_source = read_retained_snapshot(store, &result_snapshot)?;

    let existing_mapping: Option<(String, String, String)> = store
        .read_connection()
        .query_row(
            "SELECT contribution_id,lineage_record_id,project_id
             FROM local_logical_contributions WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let contribution_id = if let Some((text, mapped_lineage, mapped_project)) = &existing_mapping {
        let parsed = ContributionId::parse(text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        if mapped_lineage != lineage_record_id.as_str()
            || mapped_project != binding.project_id().as_str()
        {
            return Err(IdentityStoreError::Corrupt(
                "capture operation is already bound to a different logical contribution".into(),
            ));
        }
        parsed
    } else {
        mint_contribution_id()?
    };

    let current_head: Option<(String, String)> = store
        .read_connection()
        .query_row(
            "SELECT h.revision_record_id,r.record_sha256
             FROM contribution_revision_heads h
             JOIN contribution_revisions r
               ON r.contribution_id=h.contribution_id
              AND r.revision_record_id=h.revision_record_id
             WHERE h.contribution_id=?1",
            [contribution_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let previous = if let Some((head_text, digest_text)) = &current_head {
        if existing_mapping.is_none() {
            return Err(IdentityStoreError::Corrupt(
                "new ContributionId unexpectedly has a revision head".into(),
            ));
        }
        let head = RecordId::parse(head_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        let digest = archive::parse_digest(digest_text)
            .ok_or_else(|| IdentityStoreError::Corrupt("invalid current revision digest".into()))?;
        let resolved =
            resolve_bound_contribution_revision(store, binding, &head, digest, &contribution_id)?;
        Some(resolved.revision().clone())
    } else {
        None
    };

    let operations = request.file_operations.unwrap_or(&[]);
    let source_changes = if request.file_operations.is_none()
        && let Some(previous) = &previous
        && previous.base() == &base_snapshot
        && previous.result() == &result_snapshot
    {
        previous.source_changes().to_vec()
    } else {
        let transition = advance_file_identities(
            binding.project_id(),
            base_state.state(),
            contribution_id.as_str(),
            &base_source,
            &result_source,
            operations,
        )
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        transition
            .changes()
            .iter()
            .map(|change| change.to_value())
            .collect()
    };
    let depends_on = request
        .depends_on
        .map(<[RecordId]>::to_vec)
        .or_else(|| {
            previous
                .as_ref()
                .map(|revision| revision.depends_on().to_vec())
        })
        .unwrap_or_default();

    let mut brief_value = None;
    let mut brief_digest = None;
    let mut brief_record_id = None;
    if let Some(brief) = request.brief {
        let errors = brief.validate();
        if !errors.is_empty() {
            return Err(IdentityStoreError::Brief(
                aethyme_contracts::experimental_v0::brief::BriefErrors(errors),
            ));
        }
        let (bytes, id) = brief.to_record();
        let digest = archive::put_object(store, &bytes)?;
        brief_value = Some(BriefReference::new(id.clone(), digest.hex())?);
        brief_digest = Some(digest);
        brief_record_id = Some(id);
    }
    let revision = ContributionRevision::new(
        binding.project_id().clone(),
        contribution_id.clone(),
        lineage_record_id.clone(),
        base_snapshot.clone(),
        result_snapshot.clone(),
        source_changes,
        brief_value,
        depends_on,
    )?;
    let (revision_bytes, revision_record_id) = revision.to_record();
    let revision_digest = archive::put_object(store, &revision_bytes)?;
    let transition = advance_file_identities(
        binding.project_id(),
        base_state.state(),
        contribution_id.as_str(),
        &base_source,
        &result_source,
        &explicit_operations(&revision, base_state.state())?,
    )
    .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let computed: Vec<_> = transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    if computed.as_slice() != revision.source_changes() {
        return Err(IdentityStoreError::Corrupt(
            "prepared revision changes do not match its validated identity transition".into(),
        ));
    }

    let now = crate::clock::epoch_ms();
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_mapping: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT contribution_id,lineage_record_id,project_id
             FROM local_logical_contributions WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if current_mapping != existing_mapping {
        return Err(IdentityStoreError::ConcurrentRevision);
    }
    let transaction_head: Option<(String, String)> = transaction
        .query_row(
            "SELECT h.revision_record_id,r.record_sha256
             FROM contribution_revision_heads h
             JOIN contribution_revisions r
               ON r.contribution_id=h.contribution_id
              AND r.revision_record_id=h.revision_record_id
             WHERE h.contribution_id=?1",
            [contribution_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if transaction_head != current_head {
        return Err(IdentityStoreError::ConcurrentRevision);
    }
    if existing_mapping.is_none() {
        transaction.execute(
            "INSERT INTO local_logical_contributions
                 (operation_id,contribution_id,lineage_record_id,project_id,created_ms)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                operation_id,
                contribution_id.as_str(),
                lineage_record_id.as_str(),
                binding.project_id().as_str(),
                now
            ],
        )?;
    }
    let brief_id_text = brief_record_id.as_ref().map(RecordId::as_str);
    let brief_digest_text = brief_digest.as_ref().map(ObjectDigest::hex);
    let changed = transaction.execute(
        "INSERT INTO contribution_revisions
             (revision_record_id,operation_id,contribution_id,lineage_record_id,
              record_sha256,brief_record_id,brief_object_sha256,created_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(revision_record_id) DO NOTHING",
        rusqlite::params![
            revision_record_id.as_str(),
            operation_id,
            contribution_id.as_str(),
            lineage_record_id.as_str(),
            revision_digest.hex(),
            brief_id_text,
            brief_digest_text,
            now
        ],
    )?;
    if changed == 0 {
        let stored: Option<(String, String, String, Option<String>, Option<String>)> = transaction
            .query_row(
                "SELECT operation_id,contribution_id,lineage_record_id,brief_record_id,
                        brief_object_sha256
                 FROM contribution_revisions WHERE revision_record_id=?1",
                [revision_record_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        if stored
            != Some((
                operation_id.to_string(),
                contribution_id.as_str().to_string(),
                lineage_record_id.as_str().to_string(),
                brief_id_text.map(str::to_string),
                brief_digest_text,
            ))
        {
            return Err(IdentityStoreError::Corrupt(
                "revision RecordId is already indexed with different logical-operation data".into(),
            ));
        }
    }
    transaction.execute(
        "INSERT INTO contribution_revision_heads (contribution_id,revision_record_id)
         VALUES (?1,?2)
         ON CONFLICT(contribution_id) DO UPDATE
         SET revision_record_id=excluded.revision_record_id",
        rusqlite::params![contribution_id.as_str(), revision_record_id.as_str()],
    )?;

    let logical_count: i64 = transaction.query_row(
        "SELECT count(*) FROM local_logical_contributions WHERE lineage_record_id=?1",
        [lineage_record_id.as_str()],
        |row| row.get(0),
    )?;
    if logical_count == 1 {
        if let (Some(brief_record_id), Some(brief_digest)) = (&brief_record_id, &brief_digest) {
            transaction.execute(
                "INSERT INTO contribution_briefs
                     (lineage_record_id,brief_record_id,brief_sha256,attached_ms,revision_record_id)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(lineage_record_id) DO UPDATE SET
                     brief_record_id=excluded.brief_record_id,
                     brief_sha256=excluded.brief_sha256,
                     attached_ms=excluded.attached_ms,
                     revision_record_id=excluded.revision_record_id",
                rusqlite::params![
                    lineage_record_id.as_str(),
                    brief_record_id.as_str(),
                    brief_digest.hex(),
                    now,
                    revision_record_id.as_str()
                ],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM contribution_briefs
                 WHERE lineage_record_id=?1 AND revision_record_id IS NOT NULL",
                [lineage_record_id.as_str()],
            )?;
        }
        crate::collaboration_context::forget(&transaction, lineage_record_id.as_str())?;
    } else {
        let removed = transaction.execute(
            "DELETE FROM contribution_briefs
             WHERE lineage_record_id=?1 AND revision_record_id IS NOT NULL",
            [lineage_record_id.as_str()],
        )?;
        if removed > 0 {
            crate::collaboration_context::forget(&transaction, lineage_record_id.as_str())?;
        }
    }
    transaction.commit()?;

    resolve_bound_contribution_revision(
        store,
        binding,
        &revision_record_id,
        revision_digest,
        &contribution_id,
    )
}

fn mint_contribution_id() -> Result<ContributionId, IdentityStoreError> {
    use std::io::Read;

    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(IdentityStoreError::Randomness)?;
    let encoded = base32_lower(&bytes);
    ContributionId::parse(&format!("contrib:{encoded}"))
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn require_schema(store: &CollaborationStore) -> Result<(), IdentityStoreError> {
    if store.schema_version() < REQUIRED_SCHEMA {
        return Err(IdentityStoreError::UnsupportedSchema {
            actual: store.schema_version(),
        });
    }
    Ok(())
}

fn validate_binding_store(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
) -> Result<(), IdentityStoreError> {
    if binding.belongs_to(store) {
        Ok(())
    } else {
        Err(IdentityStoreError::ForeignStore)
    }
}

fn validate_anchor_file_rows(
    store: &CollaborationStore,
    descriptor: &ProjectDescriptor,
) -> Result<(), IdentityStoreError> {
    let mut statement = store.read_connection().prepare(
        "SELECT path,file_id,mode,content_sha256 FROM local_project_file_ids
         WHERE snapshot_id=?1 ORDER BY path",
    )?;
    let rows = statement.query_map([descriptor.anchor_snapshot().as_str()], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let actual: Vec<_> = rows.collect::<Result<_, _>>()?;
    let expected: Vec<_> = descriptor
        .files()
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.file_id.as_str().to_string(),
                file.kind.git_mode().to_string(),
                hex::encode(file.content_sha256),
            )
        })
        .collect();
    if actual != expected {
        return Err(IdentityStoreError::Corrupt(
            "stored anchor FileId rows differ from the validated descriptor".into(),
        ));
    }
    Ok(())
}

fn read_retained_snapshot(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
) -> Result<SourceSnapshot, IdentityStoreError> {
    let digest = ObjectDigest::of_snapshot(id);
    let bytes = archive::read_object_bounded(store, &digest, MAX_SNAPSHOT_MANIFEST_BYTES)?;
    let snapshot = archive::parse_manifest(&bytes).ok_or_else(|| {
        IdentityStoreError::Corrupt("retained snapshot manifest is invalid".into())
    })?;
    if snapshot.id() != *id
        || snapshot.entries().len()
            > aethyme_contracts::experimental_v0::project_identity::MAX_DESCRIPTOR_FILES
    {
        return Err(IdentityStoreError::Corrupt(
            "retained snapshot manifest does not match its bounded ID".into(),
        ));
    }
    Ok(snapshot)
}

fn load_history(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
) -> Result<BTreeMap<RecordId, RevisionRow>, IdentityStoreError> {
    let count: i64 = store.read_connection().query_row(
        "SELECT count(*) FROM contribution_revisions",
        [],
        |row| row.get(0),
    )?;
    if count < 0 || count as usize > MAX_REPLAY_REVISIONS {
        return Err(IdentityStoreError::ReplayLimit);
    }
    let raw_rows = {
        let mut statement = store.read_connection().prepare(
            "SELECT r.revision_record_id,r.operation_id,r.contribution_id,
                    r.lineage_record_id,r.record_sha256,r.brief_record_id,
                    r.brief_object_sha256,l.project_id
             FROM contribution_revisions r
             JOIN local_logical_contributions l
               ON l.operation_id=r.operation_id
              AND l.contribution_id=r.contribution_id
              AND l.lineage_record_id=r.lineage_record_id
             ORDER BY r.created_ms,r.revision_record_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if raw_rows.len() != count as usize {
        return Err(IdentityStoreError::Corrupt(
            "a contribution revision has no exact persisted logical-operation binding".into(),
        ));
    }

    let mut history = BTreeMap::new();
    let mut contribution_owners = BTreeMap::<ContributionId, (String, RecordId)>::new();

    for (
        record_text,
        operation_id,
        contribution_text,
        lineage_text,
        object_hex,
        brief_record_text,
        brief_object_hex,
        owner_project_text,
    ) in raw_rows
    {
        let record_id = RecordId::parse(&record_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        let contribution_id = ContributionId::parse(&contribution_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        let lineage_record_id = RecordId::parse(&lineage_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        let owner_project_id = ProjectId::parse(&owner_project_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        if owner_project_id != *binding.project_id() {
            return Err(IdentityStoreError::Corrupt(
                "logical capture operation belongs to a different approved project".into(),
            ));
        }
        let object_digest = archive::parse_digest(&object_hex)
            .ok_or_else(|| IdentityStoreError::Corrupt("invalid revision object digest".into()))?;
        let bytes =
            archive::read_object_bounded(store, &object_digest, MAX_CONTRIBUTION_REVISION_BYTES)?;
        let (revision, observed_id) = ContributionRevision::from_record(&bytes)?;
        if observed_id != record_id
            || revision.project_id() != binding.project_id()
            || revision.contribution_id() != &contribution_id
            || revision.capture_record_id() != &lineage_record_id
        {
            return Err(IdentityStoreError::Corrupt(
                "revision record identity, project, contribution, or capture lineage differs from its row".into(),
            ));
        }
        let (brief_object_digest, brief) =
            match (revision.brief(), brief_record_text, brief_object_hex) {
                (None, None, None) => (None, None),
                (
                    Some(BriefReference {
                        record_id: expected_id,
                        object_sha256,
                    }),
                    Some(row_id),
                    Some(row_digest),
                ) if row_id.as_str() == expected_id.as_str()
                    && row_digest.as_str() == object_sha256.as_str() =>
                {
                    let digest = archive::parse_digest(&object_sha256).ok_or_else(|| {
                        IdentityStoreError::Corrupt("invalid brief object digest".into())
                    })?;
                    let brief_bytes = archive::read_object_bounded(
                        store,
                        &digest,
                        aethyme_contracts::experimental_v0::brief::MAX_BRIEF_RECORD_BYTES,
                    )?;
                    let (brief, observed_brief_id) = Brief::from_record(&brief_bytes)?;
                    if observed_brief_id != expected_id {
                        return Err(IdentityStoreError::Corrupt(
                            "brief object does not match its revision RecordId".into(),
                        ));
                    }
                    (Some(digest), Some(brief))
                }
                _ => {
                    return Err(IdentityStoreError::Corrupt(
                        "brief row differs from the immutable revision reference".into(),
                    ));
                }
            };
        let owner = (operation_id.clone(), lineage_record_id.clone());
        if let Some(previous) = contribution_owners.insert(contribution_id.clone(), owner.clone())
            && previous != owner
        {
            return Err(IdentityStoreError::Corrupt(
                "one ContributionId is reused by distinct logical capture operations".into(),
            ));
        }

        let retained = archive::retained_contribution(store, &lineage_record_id)?;
        let source_retained = if let Some(retained) = retained {
            if revision.base() != &retained.base.snapshot_id
                || revision.result() != &retained.result.snapshot_id
            {
                return Err(IdentityStoreError::Corrupt(
                    "revision snapshots differ from the retained capture lineage".into(),
                ));
            }
            true
        } else {
            false
        };
        let row = RevisionRow {
            record_id: record_id.clone(),
            operation_id,
            object_digest,
            lineage_record_id,
            revision,
            brief_object_digest,
            brief,
            source_retained,
        };
        if history.insert(record_id, row).is_some() {
            return Err(IdentityStoreError::Corrupt(
                "duplicate revision RecordId rows".into(),
            ));
        }
    }
    Ok(history)
}

fn replay_history(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
) -> Result<ReplayIndex, IdentityStoreError> {
    let history = load_history(store, binding)?;
    let anchor_state = binding.descriptor.initial_file_identity_state()?;
    let anchor = binding.descriptor.anchor_snapshot().clone();
    let mut replay = ReplayIndex {
        states: BTreeMap::from([(
            anchor.clone(),
            SnapshotState {
                state: anchor_state,
                provenance: BTreeSet::new(),
            },
        )]),
        rows: history,
        ..ReplayIndex::default()
    };
    let mut processed = BTreeSet::new();
    let mut verified_transitions = BTreeSet::new();

    loop {
        let mut candidates = BTreeMap::<
            SourceSnapshotId,
            Vec<(RecordId, ProjectFileIdentityState, BTreeSet<RecordId>)>,
        >::new();
        let mut unresolved = BTreeSet::new();
        let mut progress = false;

        for (id, row) in &replay.rows {
            if processed.contains(id) {
                continue;
            }
            if replay.conflicts.contains(row.revision.base()) {
                if row.revision.result() != row.revision.base() {
                    replay.conflicts.insert(row.revision.result().clone());
                    replay.states.remove(row.revision.result());
                    if replay.blocked_by_unretained.contains(row.revision.base()) {
                        replay
                            .blocked_by_unretained
                            .insert(row.revision.result().clone());
                    }
                }
                processed.insert(id.clone());
                progress = true;
                continue;
            }
            let Some(base_state) = replay.states.get(row.revision.base()) else {
                continue;
            };
            if !row.source_retained {
                unresolved.insert(row.revision.result().clone());
                processed.insert(id.clone());
                progress = true;
                continue;
            }
            let base = read_retained_snapshot(store, row.revision.base())?;
            let result = read_retained_snapshot(store, row.revision.result())?;
            let operations = explicit_operations(&row.revision, &base_state.state)?;
            let transition = advance_file_identities(
                binding.project_id(),
                &base_state.state,
                row.revision.contribution_id().as_str(),
                &base,
                &result,
                &operations,
            )
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
            let computed: Vec<_> = transition
                .changes()
                .iter()
                .map(|change| change.to_value())
                .collect();
            if computed.as_slice() != row.revision.source_changes() {
                return Err(IdentityStoreError::Corrupt(
                    "replayed source changes differ from the immutable revision".into(),
                ));
            }
            let mut provenance = base_state.provenance.clone();
            provenance.insert(id.clone());
            candidates
                .entry(row.revision.result().clone())
                .or_default()
                .push((id.clone(), transition.result_identity_state(), provenance));
            replay.transitions.insert(id.clone(), transition);
            verified_transitions.insert(id.clone());
            processed.insert(id.clone());
            progress = true;
        }

        if !progress {
            break;
        }
        replay
            .blocked_by_unretained
            .extend(unresolved.iter().cloned());
        let result_ids: BTreeSet<_> = candidates
            .keys()
            .cloned()
            .chain(unresolved.into_iter())
            .collect();
        for snapshot in result_ids {
            let values = candidates.remove(&snapshot).unwrap_or_default();
            let has_unresolved = replay.blocked_by_unretained.contains(&snapshot);
            let existing = replay.states.get(&snapshot);
            let first = values.first().map(|(_, state, _)| state);
            let conflict = replay.conflicts.contains(&snapshot)
                || has_unresolved
                || values
                    .iter()
                    .any(|(_, candidate, _)| first.is_some_and(|prior| prior != candidate))
                || existing
                    .zip(first)
                    .is_some_and(|(prior, candidate)| prior.state != *candidate);
            if conflict {
                if snapshot == anchor {
                    return Err(IdentityStoreError::Corrupt(
                        "revision history conflicts with the approved descriptor anchor".into(),
                    ));
                }
                replay.states.remove(&snapshot);
                replay.conflicts.insert(snapshot);
                continue;
            }
            if let Some((_, state, _)) = values.first() {
                let mut merged = BTreeSet::new();
                for (_, candidate, provenance) in &values {
                    if candidate == state {
                        merged.extend(provenance.iter().cloned());
                    }
                }
                if let Some(existing) = replay.states.get_mut(&snapshot) {
                    existing.provenance.extend(merged);
                } else {
                    replay.states.insert(
                        snapshot,
                        SnapshotState {
                            state: state.clone(),
                            provenance: merged,
                        },
                    );
                }
            }
        }
    }

    // A disconnected retained capture is not evidence of project membership.
    // If it can claim a snapshot that already has a verified map, quarantine
    // that map and every descendant that depended on the disputed snapshot.
    let unreachable: Vec<_> = replay
        .rows
        .iter()
        .filter(|(id, _)| !processed.contains(*id))
        .map(|(_, row)| {
            (
                row.revision.base().clone(),
                row.revision.result().clone(),
                row.source_retained,
            )
        })
        .collect();
    for (base, result, source_retained) in unreachable {
        if !source_retained || replay.blocked_by_unretained.contains(&base) {
            replay.blocked_by_unretained.insert(result.clone());
        }
        if replay.states.contains_key(&result) {
            if result == anchor {
                return Err(IdentityStoreError::Corrupt(
                    "unreachable revision history conflicts with the approved descriptor anchor"
                        .into(),
                ));
            }
            replay.states.remove(&result);
            replay.conflicts.insert(result);
        }
    }

    loop {
        let mut changed = false;
        for row in replay.rows.values() {
            let base = row.revision.base();
            let result = row.revision.result();
            if base != result && replay.conflicts.contains(base) {
                if result == &anchor {
                    return Err(IdentityStoreError::Corrupt(
                        "revision history conflicts with the approved descriptor anchor".into(),
                    ));
                }
                if replay.states.remove(result).is_some() {
                    changed = true;
                }
                if replay.conflicts.insert(result.clone()) {
                    changed = true;
                }
            }
            if replay.blocked_by_unretained.contains(base)
                && replay.blocked_by_unretained.insert(result.clone())
            {
                changed = true;
            }
            if replay.blocked_by_unretained.contains(result) && replay.states.contains_key(result) {
                if result == &anchor {
                    return Err(IdentityStoreError::Corrupt(
                        "unretained history conflicts with the approved descriptor anchor".into(),
                    ));
                }
                replay.states.remove(result);
                if replay.conflicts.insert(result.clone()) {
                    changed = true;
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    replay.applied = verified_transitions
        .into_iter()
        .filter(|id| {
            replay.rows.get(id).is_some_and(|row| {
                !replay.conflicts.contains(row.revision.base())
                    && !replay.conflicts.contains(row.revision.result())
                    && replay.states.contains_key(row.revision.result())
                    && !replay.blocked_by_unretained.contains(row.revision.base())
                    && !replay.blocked_by_unretained.contains(row.revision.result())
            })
        })
        .collect();
    Ok(replay)
}

fn explicit_operations(
    revision: &ContributionRevision,
    base_state: &ProjectFileIdentityState,
) -> Result<Vec<FileOperation>, IdentityStoreError> {
    let mut path_by_id = BTreeMap::new();
    for file in base_state.files() {
        if path_by_id
            .insert(file.file_id.clone(), file.path.clone())
            .is_some()
        {
            return Err(IdentityStoreError::Corrupt(
                "base FileId mapping is not unique".into(),
            ));
        }
    }
    let mut operations = Vec::new();
    let mut split_destinations = BTreeMap::<FileId, Vec<Vec<u8>>>::new();
    for change in revision.source_changes() {
        let CanonicalValue::Object(object) = change else {
            return Err(IdentityStoreError::Corrupt(
                "source change is not an object".into(),
            ));
        };
        match text_field(object, "relation")? {
            "explicit_rename" => {
                let before = state_field(object, "before")?;
                let after = state_field(object, "after")?;
                operations.push(FileOperation::Rename {
                    from: before.0,
                    to: after.0,
                });
            }
            "explicit_copy" => {
                let after = state_field(object, "after")?;
                let sources = id_fields(object, "derived_from")?;
                if sources.len() != 1 {
                    return Err(IdentityStoreError::Corrupt(
                        "bounded copy history must name exactly one source FileId".into(),
                    ));
                }
                let from = path_by_id.get(&sources[0]).cloned().ok_or_else(|| {
                    IdentityStoreError::Corrupt(
                        "copy source FileId is absent from the exact base state".into(),
                    )
                })?;
                operations.push(FileOperation::Copy { from, to: after.0 });
            }
            "explicit_split" => {
                let after = state_field(object, "after")?;
                let sources = id_fields(object, "derived_from")?;
                if sources.len() != 1 {
                    return Err(IdentityStoreError::Corrupt(
                        "bounded split history must name exactly one source FileId".into(),
                    ));
                }
                split_destinations
                    .entry(sources[0].clone())
                    .or_default()
                    .push(after.0);
            }
            "same_path" | "new" | "removed" | "uncertain_rename" => {}
            _ => {
                return Err(IdentityStoreError::Corrupt(
                    "source change relation is outside the bounded replay profile".into(),
                ));
            }
        }
    }
    for (source, mut destinations) in split_destinations {
        let from = path_by_id.get(&source).cloned().ok_or_else(|| {
            IdentityStoreError::Corrupt(
                "split source FileId is absent from the exact base state".into(),
            )
        })?;
        destinations.sort();
        destinations.dedup();
        if destinations.len() < 2 {
            return Err(IdentityStoreError::Corrupt(
                "split history has fewer than two unique destinations".into(),
            ));
        }
        operations.push(FileOperation::Split {
            from,
            to: destinations,
        });
    }
    Ok(operations)
}

fn state_field(
    object: &Object,
    field: &'static str,
) -> Result<(Vec<u8>, EntryKind, [u8; 32], FileId), IdentityStoreError> {
    let Some(CanonicalValue::Object(state)) = object.get(field) else {
        return Err(IdentityStoreError::Corrupt(format!(
            "source change is missing the {field} state"
        )));
    };
    let path = hex::decode(text_field(state, "path_hex")?)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let kind = EntryKind::from_git_mode(text_field(state, "mode")?)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let digest: [u8; 32] = hex::decode(text_field(state, "content_sha256")?)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?
        .try_into()
        .map_err(|_| IdentityStoreError::Corrupt("invalid content digest length".into()))?;
    let file_id = FileId::parse(text_field(state, "file_id")?)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    Ok((path, kind, digest, file_id))
}

fn text_field<'a>(object: &'a Object, field: &'static str) -> Result<&'a str, IdentityStoreError> {
    match object.get(field) {
        Some(CanonicalValue::String(value)) => Ok(value),
        _ => Err(IdentityStoreError::Corrupt(format!(
            "source change field {field} must be a string"
        ))),
    }
}

fn id_fields(object: &Object, field: &'static str) -> Result<Vec<FileId>, IdentityStoreError> {
    let Some(CanonicalValue::Array(values)) = object.get(field) else {
        return Err(IdentityStoreError::Corrupt(format!(
            "source change field {field} must be an array"
        )));
    };
    values
        .iter()
        .map(|value| match value {
            CanonicalValue::String(text) => {
                FileId::parse(text).map_err(|error| IdentityStoreError::Corrupt(error.to_string()))
            }
            _ => Err(IdentityStoreError::Corrupt(format!(
                "source change field {field} contains a non-string ID"
            ))),
        })
        .collect()
}

fn unique_state<'a>(
    replay: &'a ReplayIndex,
    snapshot: &SourceSnapshotId,
) -> Result<&'a SnapshotState, IdentityStoreError> {
    if replay.conflicts.contains(snapshot) {
        return Err(IdentityStoreError::UnknownSnapshot {
            snapshot: snapshot.as_str().into(),
            reason: IdentityStateUnknownReason::CompetingHistories,
        });
    }
    replay
        .states
        .get(snapshot)
        .ok_or_else(|| IdentityStoreError::UnknownSnapshot {
            snapshot: snapshot.as_str().into(),
            reason: if replay.blocked_by_unretained.contains(snapshot) {
                IdentityStateUnknownReason::HistoryDependsOnUnretainedCapture
            } else {
                IdentityStateUnknownReason::NoValidatedHistory
            },
        })
}

fn provenance_ids(provenance: &IdentityStateProvenance) -> BTreeSet<RecordId> {
    match provenance {
        IdentityStateProvenance::DescriptorAnchor { .. } => BTreeSet::new(),
        IdentityStateProvenance::Replayed {
            revision_record_ids,
        } => revision_record_ids.iter().cloned().collect(),
        IdentityStateProvenance::RevisionTransition {
            revision_record_id,
            base_revision_record_ids,
        } => base_revision_record_ids
            .iter()
            .cloned()
            .chain(std::iter::once(revision_record_id.clone()))
            .collect(),
    }
}

fn resolved_state(
    binding: &ApprovedProjectBinding,
    snapshot: &SourceSnapshotId,
    state: &SnapshotState,
) -> ResolvedProjectFileIdentityState {
    let provenance = if snapshot == binding.descriptor.anchor_snapshot() {
        IdentityStateProvenance::DescriptorAnchor {
            descriptor_record_id: binding.descriptor_record_id.clone(),
            anchor_commit: binding.anchor_commit.clone(),
            committed_config_commit: binding.committed_config_commit.clone(),
        }
    } else {
        IdentityStateProvenance::Replayed {
            revision_record_ids: state.provenance.iter().cloned().collect(),
        }
    };
    ResolvedProjectFileIdentityState {
        store_project: binding.store_project.clone(),
        store_dir: binding.store_dir.clone(),
        descriptor_record_id: binding.descriptor_record_id.clone(),
        snapshot: snapshot.clone(),
        state: state.state.clone(),
        provenance,
    }
}

fn validate_cached_maps(
    store: &CollaborationStore,
    replay: &ReplayIndex,
) -> Result<(), IdentityStoreError> {
    let rows = {
        let mut statement = store.read_connection().prepare(
            "SELECT snapshot_id,path,file_id,mode,content_sha256
             FROM local_project_file_ids ORDER BY snapshot_id,path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut by_snapshot = BTreeMap::<String, Vec<(Vec<u8>, String, String, String)>>::new();
    for (snapshot, path, file_id, mode, content) in rows {
        by_snapshot
            .entry(snapshot)
            .or_default()
            .push((path, file_id, mode, content));
    }
    for (snapshot_text, actual) in by_snapshot {
        let snapshot = SourceSnapshotId::parse(&snapshot_text)
            .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
        if replay.conflicts.contains(&snapshot) {
            return Err(IdentityStoreError::Corrupt(
                "cached FileId map belongs to an ambiguous snapshot history".into(),
            ));
        }
        let Some(expected_state) = replay.states.get(&snapshot) else {
            return Err(IdentityStoreError::Corrupt(
                "cached FileId map has no replay-verified history".into(),
            ));
        };
        let expected: Vec<_> = expected_state
            .state
            .files()
            .map(|file| {
                (
                    file.path.clone(),
                    file.file_id.as_str().to_string(),
                    file.kind.git_mode().to_string(),
                    hex::encode(file.content_sha256),
                )
            })
            .collect();
        if actual != expected {
            return Err(IdentityStoreError::Corrupt(
                "cached FileId map differs from replay-verified history".into(),
            ));
        }
    }
    Ok(())
}
