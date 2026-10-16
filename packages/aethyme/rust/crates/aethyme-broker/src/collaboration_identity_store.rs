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
    FileIdentityTransition, FileOperation, ProjectDescriptor, ProjectFileIdentityState, ProjectId,
    RecordId, SourceSnapshot, SourceSnapshotId, advance_file_identities,
    brief::Brief,
    canonical_json::{Object, Value as CanonicalValue},
    contribution_revision::MAX_CONTRIBUTION_REVISION_BYTES,
    project_identity::{MAX_DESCRIPTOR_BYTES, PROJECT_DESCRIPTOR_PATH},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use thiserror::Error;

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_capture::OperationId;
use crate::collaboration_gc::{self, ArchiveUse};
use crate::collaboration_state::{CollaborationStore, ProjectKey};

const REQUIRED_SCHEMA: i64 = 26;
const MAX_REPLAY_REVISIONS: usize = 10_000;
const MAX_REPLAY_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
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
    bootstrap_snapshot: SourceSnapshotId,
    bootstrap_transition_sha256: String,
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

    pub fn bootstrap_snapshot(&self) -> &SourceSnapshotId {
        &self.bootstrap_snapshot
    }

    pub fn bootstrap_transition_sha256(&self) -> &str {
        &self.bootstrap_transition_sha256
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
    ProjectBindingBootstrap {
        descriptor_record_id: RecordId,
        bootstrap_snapshot: SourceSnapshotId,
        committed_config_commit: CommitOid,
        transition_sha256: String,
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
    Bootstrap(ResolvedProjectFileIdentityState),
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

/// One exact immutable revision pin; the logical ContributionId is checked
/// against both the row and archived revision bytes.
#[derive(Debug, Clone, Copy)]
pub struct ContributionRevisionPin<'a> {
    pub revision_record_id: &'a RecordId,
    pub object_digest: ObjectDigest,
    pub contribution_id: &'a ContributionId,
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
    brief_record_bytes: Option<Vec<u8>>,
    capture_record_id: RecordId,
    base_state: ResolvedProjectFileIdentityState,
    result_state: ResolvedProjectFileIdentityState,
    identity_transition: FileIdentityTransition,
    identity_proof_revision_ids: BTreeSet<RecordId>,
    identity_proof_object_digests: BTreeSet<ObjectDigest>,
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

    /// Exact canonical bytes of the brief object validated against this revision.
    pub fn brief_record_bytes(&self) -> Option<&[u8]> {
        self.brief_record_bytes.as_deref()
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

    /// Exact revision ancestors, including this revision, whose immutable
    /// FileId transitions support the resolved result.
    pub fn identity_proof_revision_ids(&self) -> &BTreeSet<RecordId> {
        &self.identity_proof_revision_ids
    }

    /// Archive objects for the exact identity proof: each revision record
    /// and its immutable brief when present, including this revision.
    pub fn identity_proof_object_digests(&self) -> &BTreeSet<ObjectDigest> {
        &self.identity_proof_object_digests
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
    #[error("bounded identity replay exceeds the {MAX_REPLAY_ARCHIVE_BYTES}-byte archive budget")]
    ReplayByteLimit,
    #[error("duplicate revision pin {0}")]
    DuplicateRevisionPin(String),
    #[error("archive use guard belongs to a different project store")]
    ForeignArchiveUse,
    #[error("could not acquire the shared archive-use lock at {path}: {source}")]
    ArchiveLock {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
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

impl IdentityStoreError {
    /// Stable machine-readable code for CLI and typed context error mapping.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::Unbound => "project_unbound",
            Self::ForeignStore => "foreign_store",
            Self::Corrupt(_) => "corrupt_state",
            Self::ReplayLimit => "identity_replay_limit",
            Self::ReplayByteLimit => "identity_replay_byte_limit",
            Self::DuplicateRevisionPin(_) => "duplicate_revision_pin",
            Self::ForeignArchiveUse => "foreign_archive_guard",
            Self::ArchiveLock { .. } => "archive_lock",
            Self::UnknownRevision(_) => "unknown_revision",
            Self::UnknownCaptureOperation(_) => "unknown_capture_operation",
            Self::AmbiguousCaptureLineage(_) => "ambiguous_capture_lineage",
            Self::ConcurrentRevision => "concurrent_revision",
            Self::Randomness(_) => "io",
            Self::UnknownSnapshot { reason, .. } => match reason {
                IdentityStateUnknownReason::SnapshotNotRetained => "snapshot_not_retained",
                IdentityStateUnknownReason::NoValidatedHistory => "identity_history_unknown",
                IdentityStateUnknownReason::CompetingHistories => "identity_history_conflict",
                IdentityStateUnknownReason::HistoryDependsOnUnretainedCapture => {
                    "identity_history_unretained"
                }
            },
            Self::Archive(error) => error.code(),
            Self::Sqlite(_) => "sqlite",
            Self::Sensitive(error) => match error {
                crate::collaboration_sensitive::SensitiveError::Quarantined => {
                    "sensitive_quarantined"
                }
                crate::collaboration_sensitive::SensitiveError::Mismatch => {
                    "sensitive_authority_mismatch"
                }
                crate::collaboration_sensitive::SensitiveError::NotFrozen => "sensitive_not_frozen",
                crate::collaboration_sensitive::SensitiveError::Busy => "sensitive_busy",
                crate::collaboration_sensitive::SensitiveError::UnknownWork => {
                    "sensitive_unknown_work"
                }
                crate::collaboration_sensitive::SensitiveError::UnclassifiedFiles => {
                    "sensitive_unclassified_files"
                }
                crate::collaboration_sensitive::SensitiveError::Invalid(_) => {
                    "sensitive_state_invalid"
                }
                crate::collaboration_sensitive::SensitiveError::Io(_) => "io",
                crate::collaboration_sensitive::SensitiveError::Sqlite(_) => "sqlite",
                crate::collaboration_sensitive::SensitiveError::Json(_) => {
                    "sensitive_state_invalid"
                }
                crate::collaboration_sensitive::SensitiveError::State(_) => "state",
                crate::collaboration_sensitive::SensitiveError::Authority(_) => "authority",
                crate::collaboration_sensitive::SensitiveError::Gc(error) => error.code(),
            },
            Self::Project(_) => "invalid_project_identity",
            Self::Revision(_) => "invalid_contribution_revision",
            Self::Brief(_) => "invalid_brief",
            Self::Snapshot(_) => "invalid_source_snapshot",
        }
    }
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
    brief_record_bytes: Option<Vec<u8>>,
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
    let archive_use = acquire_archive_use(store)?;
    resolve_approved_project_binding_with_archive_use(store, &archive_use)
}

pub(crate) fn resolve_approved_project_binding_with_archive_use(
    store: &CollaborationStore,
    archive_use: &ArchiveUse,
) -> Result<Option<ApprovedProjectBinding>, IdentityStoreError> {
    validate_archive_use(store, archive_use)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    resolve_approved_project_binding_locked(store)
}

fn resolve_approved_project_binding_locked(
    store: &CollaborationStore,
) -> Result<Option<ApprovedProjectBinding>, IdentityStoreError> {
    require_schema(store)?;
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )> = store
        .read_connection()
        .query_row(
            "SELECT project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
                    anchor_snapshot,confirmed_descriptor_record_id,committed_config_commit,
                    bootstrap_snapshot,bootstrap_transition_sha256
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
                    row.get(7)?,
                    row.get(8)?,
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
        bootstrap_snapshot_text,
        bootstrap_transition_text,
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
    let bootstrap_snapshot = SourceSnapshotId::parse(&bootstrap_snapshot_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    if bootstrap_transition_text.len() != 64
        || !bootstrap_transition_text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(IdentityStoreError::Corrupt(
            "invalid bootstrap transition digest".into(),
        ));
    }
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
    validate_identity_file_rows(
        store,
        descriptor.anchor_snapshot(),
        &descriptor.initial_file_identity_state()?,
    )?;

    let retained_bootstrap = archive::retained(store, &bootstrap_snapshot)?.ok_or_else(|| {
        IdentityStoreError::Corrupt("binding bootstrap snapshot is not retained".into())
    })?;
    if retained_bootstrap.commit != committed_config_commit {
        return Err(IdentityStoreError::Corrupt(
            "committed config commit differs from the retained bootstrap snapshot".into(),
        ));
    }
    if bootstrap_snapshot == *descriptor.anchor_snapshot() {
        return Err(IdentityStoreError::Corrupt(
            "binding bootstrap must be a distinct committed snapshot after the anchor".into(),
        ));
    }
    let bootstrap_source = read_retained_snapshot(store, &bootstrap_snapshot)?;
    validate_committed_binding_files(
        store,
        &bootstrap_source,
        &descriptor_bytes,
        &descriptor_record_id,
    )?;
    let anchor_source = read_retained_snapshot(store, descriptor.anchor_snapshot())?;
    let bootstrap_transition = advance_file_identities(
        &project_id,
        &descriptor.initial_file_identity_state()?,
        descriptor_record_id.as_str(),
        &anchor_source,
        &bootstrap_source,
        &[],
    )
    .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let expected_bootstrap_digest = bootstrap_transition_sha256(
        &project_id,
        &descriptor_record_id,
        descriptor.anchor_snapshot(),
        &bootstrap_snapshot,
        &committed_config_commit,
        &bootstrap_transition,
    );
    if expected_bootstrap_digest != bootstrap_transition_text {
        return Err(IdentityStoreError::Corrupt(
            "stored bootstrap transition differs from its committed source snapshots".into(),
        ));
    }
    let bootstrap_state = bootstrap_transition.result_identity_state();
    validate_identity_file_rows(store, &bootstrap_snapshot, &bootstrap_state)?;

    Ok(Some(ApprovedProjectBinding {
        store_project: store.project().clone(),
        store_dir: store.project_dir().to_path_buf(),
        project_id,
        descriptor,
        descriptor_record_id,
        descriptor_object,
        anchor_commit,
        committed_config_commit,
        bootstrap_snapshot,
        bootstrap_transition_sha256: bootstrap_transition_text,
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
    let archive_use = acquire_archive_use(store)?;
    resolve_project_file_identity_state_with_archive_use(store, binding, snapshot, &archive_use)
}

pub(crate) fn resolve_project_file_identity_state_with_archive_use(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    snapshot: &SourceSnapshotId,
    archive_use: &ArchiveUse,
) -> Result<IdentityStateResolution, IdentityStoreError> {
    validate_archive_use(store, archive_use)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    resolve_project_file_identity_state_locked(store, binding, snapshot)
}

fn resolve_project_file_identity_state_locked(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    snapshot: &SourceSnapshotId,
) -> Result<IdentityStateResolution, IdentityStoreError> {
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
    } else if snapshot == binding.bootstrap_snapshot() {
        Ok(IdentityStateResolution::Bootstrap(resolved))
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
    let archive_use = acquire_archive_use(store)?;
    resolve_bound_contribution_revision_with_archive_use(
        store,
        binding,
        revision_record_id,
        expected_revision_object,
        expected_contribution,
        &archive_use,
    )
}

/// Resolve one exact target-pinned revision while the caller retains the
/// shared archive barrier.
pub(crate) fn resolve_bound_contribution_revision_with_archive_use(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    revision_record_id: &RecordId,
    expected_revision_object: ObjectDigest,
    expected_contribution: &ContributionId,
    archive_use: &ArchiveUse,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    let pin = ContributionRevisionPin {
        revision_record_id,
        object_digest: expected_revision_object,
        contribution_id: expected_contribution,
    };
    let mut rows =
        resolve_bound_contribution_revisions_with_archive_use(store, binding, &[pin], archive_use)?;
    rows.remove(revision_record_id)
        .ok_or_else(|| IdentityStoreError::UnknownRevision(revision_record_id.as_str().into()))
}

/// Resolve a bounded set of target/context revision pins using one validated
/// history replay. The result is keyed by immutable revision RecordId.
pub fn resolve_bound_contribution_revisions(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    pins: &[ContributionRevisionPin<'_>],
) -> Result<BTreeMap<RecordId, ResolvedContributionRevision>, IdentityStoreError> {
    let archive_use = acquire_archive_use(store)?;
    resolve_bound_contribution_revisions_with_archive_use(store, binding, pins, &archive_use)
}

/// Resolve revision pins while the caller retains the shared archive barrier.
pub(crate) fn resolve_bound_contribution_revisions_with_archive_use(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    pins: &[ContributionRevisionPin<'_>],
    archive_use: &ArchiveUse,
) -> Result<BTreeMap<RecordId, ResolvedContributionRevision>, IdentityStoreError> {
    validate_archive_use(store, archive_use)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_binding_store(store, binding)?;
    require_schema(store)?;

    let mut seen = BTreeSet::new();
    for pin in pins {
        if !seen.insert(pin.revision_record_id.clone()) {
            return Err(IdentityStoreError::DuplicateRevisionPin(
                pin.revision_record_id.as_str().to_string(),
            ));
        }
    }
    if pins.is_empty() {
        return Ok(BTreeMap::new());
    }

    let replay = replay_history(store, binding)?;
    validate_cached_maps(store, &replay)?;
    let mut resolved = BTreeMap::new();
    for pin in pins {
        let item = resolve_bound_contribution_revision_from_replay(
            store,
            binding,
            &replay,
            pin.revision_record_id,
            pin.object_digest,
            pin.contribution_id,
        )?;
        resolved.insert(pin.revision_record_id.clone(), item);
    }
    Ok(resolved)
}

fn resolve_bound_contribution_revision_from_replay(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    replay: &ReplayIndex,
    revision_record_id: &RecordId,
    expected_revision_object: ObjectDigest,
    expected_contribution: &ContributionId,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
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
    let base = unique_state(replay, row.revision.base())?;
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
    let base_state = resolved_state(binding, row.revision.base(), base);
    let (identity_proof_revision_ids, identity_proof_object_digests) = identity_proof_artifacts(
        replay,
        &base_state,
        &row.record_id,
        row.object_digest,
        row.brief_object_digest,
    )?;
    let base_revision_record_ids = base.provenance.iter().cloned().collect();
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
        brief_record_bytes: row.brief_record_bytes.clone(),
        capture_record_id: row.lineage_record_id.clone(),
        base_state,
        result_state,
        identity_transition: transition,
        identity_proof_revision_ids,
        identity_proof_object_digests,
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
    let archive_use = acquire_archive_use(store)?;
    validate_bound_contribution_revision_bytes_with_archive_use(
        store,
        binding,
        base_state,
        expected_record_id,
        expected_object_digest,
        expected_contribution,
        expected_capture_lineage,
        revision_bytes,
        brief_bytes,
        &archive_use,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_bound_contribution_revision_bytes_with_archive_use(
    store: &CollaborationStore,
    binding: &ApprovedProjectBinding,
    base_state: &ResolvedProjectFileIdentityState,
    expected_record_id: &RecordId,
    expected_object_digest: ObjectDigest,
    expected_contribution: &ContributionId,
    expected_capture_lineage: Option<&RecordId>,
    revision_bytes: &[u8],
    brief_bytes: Option<&[u8]>,
    archive_use: &ArchiveUse,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    validate_archive_use(store, archive_use)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    validate_bound_contribution_revision_bytes_locked(
        store,
        binding,
        base_state,
        expected_record_id,
        expected_object_digest,
        expected_contribution,
        expected_capture_lineage,
        revision_bytes,
        brief_bytes,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_bound_contribution_revision_bytes_locked(
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

    let (brief_object_digest, brief, brief_record_bytes) = match (revision.brief(), brief_bytes) {
        (None, None) => (None, None, None),
        (Some(reference), Some(bytes)) => {
            let digest = ObjectDigest::of(bytes);
            if digest.hex().as_str() != reference.object_sha256.as_str() {
                return Err(IdentityStoreError::Corrupt(
                    "received brief bytes differ from the immutable revision digest".into(),
                ));
            }
            let (brief, observed_brief_id) = Brief::from_record(bytes)?;
            if observed_brief_id.as_str() != reference.record_id.as_str() {
                return Err(IdentityStoreError::Corrupt(
                    "received brief bytes differ from the immutable revision RecordId".into(),
                ));
            }
            (Some(digest), Some(brief), Some(bytes.to_vec()))
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
    let (identity_proof_revision_ids, identity_proof_object_digests) = identity_proof_artifacts(
        &replay,
        &verified_base,
        &observed_record_id,
        expected_object_digest,
        brief_object_digest,
    )?;
    let capture_record_id = revision.capture_record_id().clone();
    Ok(ResolvedContributionRevision {
        revision,
        record_id: observed_record_id,
        operation_id: None,
        object_digest: expected_object_digest,
        brief_object_digest,
        brief,
        brief_record_bytes,
        capture_record_id,
        base_state: verified_base,
        result_state,
        identity_transition: transition,
        identity_proof_revision_ids,
        identity_proof_object_digests,
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
    let archive_use = acquire_archive_use(store)?;
    revise_contribution_for_operation_with_archive_use(store, binding, request, &archive_use)
}

pub(crate) fn revise_contribution_for_operation_with_archive_use(
    store: &mut CollaborationStore,
    binding: &ApprovedProjectBinding,
    request: ContributionRevisionRequest<'_>,
    archive_use: &ArchiveUse,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
    validate_archive_use(store, archive_use)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    revise_contribution_for_operation_locked(store, binding, request, archive_use)
}

fn revise_contribution_for_operation_locked(
    store: &mut CollaborationStore,
    binding: &ApprovedProjectBinding,
    request: ContributionRevisionRequest<'_>,
    archive_use: &ArchiveUse,
) -> Result<ResolvedContributionRevision, IdentityStoreError> {
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

    let base_state = match resolve_project_file_identity_state_with_archive_use(
        store,
        binding,
        &base_snapshot,
        archive_use,
    )? {
        IdentityStateResolution::Anchor(state)
        | IdentityStateResolution::Bootstrap(state)
        | IdentityStateResolution::Replayed(state) => state,
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
        let resolved = resolve_bound_contribution_revision_with_archive_use(
            store,
            binding,
            &head,
            digest,
            &contribution_id,
            archive_use,
        )?;
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
    if changed > 0 {
        transaction.execute(
            "INSERT INTO contribution_revision_heads (contribution_id,revision_record_id)
             VALUES (?1,?2)
             ON CONFLICT(contribution_id) DO UPDATE
             SET revision_record_id=excluded.revision_record_id",
            rusqlite::params![contribution_id.as_str(), revision_record_id.as_str()],
        )?;
    }

    if changed > 0 {
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
    }
    transaction.commit()?;

    resolve_bound_contribution_revision_with_archive_use(
        store,
        binding,
        &revision_record_id,
        revision_digest,
        &contribution_id,
        archive_use,
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

fn read_replay_object(
    store: &CollaborationStore,
    digest: &ObjectDigest,
    per_object_limit: usize,
    remaining: &mut usize,
) -> Result<Vec<u8>, IdentityStoreError> {
    let path = archive::object_path(store, digest);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(IdentityStoreError::Archive(archive::ArchiveError::Io {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "identity replay object is not a regular file",
                ),
            }));
        }
        Err(source) => {
            return Err(IdentityStoreError::Archive(archive::ArchiveError::Io {
                path,
                source,
            }));
        }
    };
    if metadata.len() > per_object_limit as u64 {
        return Err(IdentityStoreError::Archive(archive::ArchiveError::Io {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "identity replay object exceeds its per-object bound",
            ),
        }));
    }
    if metadata.len() > *remaining as u64 {
        return Err(IdentityStoreError::ReplayByteLimit);
    }
    let bytes = archive::read_object_bounded(store, digest, per_object_limit.min(*remaining))?;
    *remaining -= bytes.len();
    Ok(bytes)
}

fn acquire_archive_use(store: &CollaborationStore) -> Result<ArchiveUse, IdentityStoreError> {
    collaboration_gc::archive_use(store).map_err(|source| IdentityStoreError::ArchiveLock {
        path: collaboration_gc::lock_path(store),
        source,
    })
}

fn validate_archive_use(
    store: &CollaborationStore,
    archive_use: &ArchiveUse,
) -> Result<(), IdentityStoreError> {
    if archive_use.covers(store) {
        Ok(())
    } else {
        Err(IdentityStoreError::ForeignArchiveUse)
    }
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

fn validate_committed_binding_files(
    store: &CollaborationStore,
    snapshot: &SourceSnapshot,
    expected_descriptor_bytes: &[u8],
    expected_descriptor_id: &RecordId,
) -> Result<(), IdentityStoreError> {
    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    let config_bytes =
        source_file_bytes(store, snapshot, b".aethyme/config.toml", MAX_CONFIG_BYTES)?;
    let config_text = std::str::from_utf8(&config_bytes).map_err(|_| {
        IdentityStoreError::Corrupt("committed collaboration config is not UTF-8".into())
    })?;
    let parsed: toml::Value = config_text.parse().map_err(|error| {
        IdentityStoreError::Corrupt(format!(
            "committed collaboration config is invalid: {error}"
        ))
    })?;
    let table = parsed
        .get("collaboration")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| {
            IdentityStoreError::Corrupt(
                "bootstrap snapshot has no committed [collaboration] table".into(),
            )
        })?;
    if table
        .keys()
        .any(|key| !["capture", "project", "descriptor"].contains(&key.as_str()))
    {
        return Err(IdentityStoreError::Corrupt(
            "bootstrap config contains an unsupported collaboration setting".into(),
        ));
    }
    if !matches!(
        table.get("capture").and_then(toml::Value::as_str),
        Some("advisory" | "required")
    ) {
        return Err(IdentityStoreError::Corrupt(
            "bootstrap config does not enable advisory or required capture".into(),
        ));
    }
    let project_text = table
        .get("project")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            IdentityStoreError::Corrupt("bootstrap config has no local project key".into())
        })?;
    let project = ProjectKey::parse(project_text)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    if project != *store.project() {
        return Err(IdentityStoreError::Corrupt(
            "bootstrap config project key differs from its local state store".into(),
        ));
    }
    if table.get("descriptor").and_then(toml::Value::as_str) != Some(PROJECT_DESCRIPTOR_PATH) {
        return Err(IdentityStoreError::Corrupt(
            "bootstrap config does not name the exact experimental descriptor path".into(),
        ));
    }
    let descriptor_bytes = source_file_bytes(
        store,
        snapshot,
        PROJECT_DESCRIPTOR_PATH.as_bytes(),
        MAX_DESCRIPTOR_BYTES,
    )?;
    if descriptor_bytes != expected_descriptor_bytes {
        return Err(IdentityStoreError::Corrupt(
            "committed bootstrap descriptor differs from the approved archived descriptor".into(),
        ));
    }
    let (_, observed_id) = ProjectDescriptor::from_record(&descriptor_bytes)?;
    if observed_id != *expected_descriptor_id {
        return Err(IdentityStoreError::Corrupt(
            "committed bootstrap descriptor differs from its confirmed RecordId".into(),
        ));
    }
    Ok(())
}

fn source_file_bytes(
    store: &CollaborationStore,
    snapshot: &SourceSnapshot,
    path: &[u8],
    limit: usize,
) -> Result<Vec<u8>, IdentityStoreError> {
    let entry = snapshot
        .entries()
        .iter()
        .find(|entry| entry.path() == path)
        .ok_or_else(|| {
            IdentityStoreError::Corrupt(format!(
                "retained binding snapshot does not contain {}",
                String::from_utf8_lossy(path)
            ))
        })?;
    if !matches!(entry.kind(), EntryKind::Regular | EntryKind::Executable) {
        return Err(IdentityStoreError::Corrupt(format!(
            "retained binding path {} is not a regular file",
            String::from_utf8_lossy(path)
        )));
    }
    let digest = ObjectDigest::from_bytes(*entry.content_sha256());
    archive::read_object_bounded(store, &digest, limit).map_err(Into::into)
}

pub(crate) fn bootstrap_transition_sha256(
    project_id: &ProjectId,
    descriptor_record_id: &RecordId,
    anchor_snapshot: &SourceSnapshotId,
    bootstrap_snapshot: &SourceSnapshotId,
    bootstrap_commit: &CommitOid,
    transition: &FileIdentityTransition,
) -> String {
    let changes = transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    let bytes = CanonicalValue::Object(
        Object::new(vec![
            (
                "domain".into(),
                CanonicalValue::String("aethyme project binding bootstrap v0".into()),
            ),
            (
                "project_id".into(),
                CanonicalValue::String(project_id.as_str().into()),
            ),
            (
                "descriptor_record_id".into(),
                CanonicalValue::String(descriptor_record_id.as_str().into()),
            ),
            (
                "anchor_snapshot".into(),
                CanonicalValue::String(anchor_snapshot.as_str().into()),
            ),
            (
                "bootstrap_snapshot".into(),
                CanonicalValue::String(bootstrap_snapshot.as_str().into()),
            ),
            (
                "bootstrap_commit".into(),
                CanonicalValue::String(bootstrap_commit.as_str().into()),
            ),
            ("changes".into(), CanonicalValue::Array(changes)),
        ])
        .expect("bootstrap digest fields are unique"),
    )
    .to_canonical_bytes();
    ObjectDigest::of(&bytes).hex()
}

fn validate_identity_file_rows(
    store: &CollaborationStore,
    snapshot: &SourceSnapshotId,
    expected_state: &ProjectFileIdentityState,
) -> Result<(), IdentityStoreError> {
    let mut statement = store.read_connection().prepare(
        "SELECT path,file_id,mode,content_sha256 FROM local_project_file_ids
         WHERE snapshot_id=?1 ORDER BY path",
    )?;
    let actual: Vec<_> = statement
        .query_map([snapshot.as_str()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let expected: Vec<_> = expected_state
        .files()
        .map(|file| {
            (
                file.path.clone(),
                file.file_id.as_str().to_string(),
                file.kind.git_mode().to_string(),
                ObjectDigest::from_bytes(file.content_sha256).hex(),
            )
        })
        .collect();
    if actual != expected {
        return Err(IdentityStoreError::Corrupt(format!(
            "stored FileId rows differ from the validated snapshot {}",
            snapshot.as_str()
        )));
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
    let live_revision_ids =
        collaboration_gc::live_identity_revision_roots(store, collaboration_gc::now_ms()).map_err(
            |error| match error {
                collaboration_gc::GcError::IdentityReplayLimit => IdentityStoreError::ReplayLimit,
                other => IdentityStoreError::Corrupt(format!(
                    "could not enumerate retained identity history: {other}"
                )),
            },
        )?;
    if live_revision_ids.len() > MAX_REPLAY_REVISIONS {
        return Err(IdentityStoreError::ReplayLimit);
    }
    if live_revision_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let live_id_text: Vec<String> = live_revision_ids
        .iter()
        .map(|record_id| record_id.as_str().to_string())
        .collect();
    let placeholders = (1..=live_id_text.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT r.revision_record_id,r.operation_id,r.contribution_id,
                r.lineage_record_id,r.record_sha256,r.brief_record_id,
                r.brief_object_sha256,l.project_id
         FROM contribution_revisions r
         JOIN local_logical_contributions l
           ON l.operation_id=r.operation_id
          AND l.contribution_id=r.contribution_id
          AND l.lineage_record_id=r.lineage_record_id
         WHERE r.revision_record_id IN ({placeholders})
         ORDER BY r.created_ms,r.revision_record_id"
    );
    let raw_rows = {
        let mut statement = store.read_connection().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(live_id_text.iter()), |row| {
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
    if raw_rows.len() != live_revision_ids.len() {
        return Err(IdentityStoreError::Corrupt(
            "a live identity proof has no exact persisted logical-operation binding".into(),
        ));
    }
    let mut history = BTreeMap::new();
    let mut replay_archive_bytes = MAX_REPLAY_ARCHIVE_BYTES;
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
        let bytes = read_replay_object(
            store,
            &object_digest,
            MAX_CONTRIBUTION_REVISION_BYTES,
            &mut replay_archive_bytes,
        )?;
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
        let (brief_object_digest, brief, brief_record_bytes) =
            match (revision.brief(), brief_record_text, brief_object_hex) {
                (None, None, None) => (None, None, None),
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
                    let brief_bytes = read_replay_object(
                        store,
                        &digest,
                        aethyme_contracts::experimental_v0::brief::MAX_BRIEF_RECORD_BYTES,
                        &mut replay_archive_bytes,
                    )?;
                    let (brief, observed_brief_id) = Brief::from_record(&brief_bytes)?;
                    if &observed_brief_id != expected_id {
                        return Err(IdentityStoreError::Corrupt(
                            "brief object does not match its revision RecordId".into(),
                        ));
                    }
                    (Some(digest), Some(brief), Some(brief_bytes))
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
            brief_record_bytes,
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
    let bootstrap_source = read_retained_snapshot(store, binding.bootstrap_snapshot())?;
    let anchor_source = read_retained_snapshot(store, binding.descriptor.anchor_snapshot())?;
    let bootstrap_transition = advance_file_identities(
        binding.project_id(),
        &anchor_state,
        binding.descriptor_record_id().as_str(),
        &anchor_source,
        &bootstrap_source,
        &[],
    )
    .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    if bootstrap_transition_sha256(
        binding.project_id(),
        binding.descriptor_record_id(),
        binding.descriptor.anchor_snapshot(),
        binding.bootstrap_snapshot(),
        binding.committed_config_commit(),
        &bootstrap_transition,
    ) != binding.bootstrap_transition_sha256()
    {
        return Err(IdentityStoreError::Corrupt(
            "bootstrap transition changed after binding resolution".into(),
        ));
    }
    let bootstrap = binding.bootstrap_snapshot().clone();
    if bootstrap == anchor {
        return Err(IdentityStoreError::Corrupt(
            "binding bootstrap snapshot equals its descriptor anchor".into(),
        ));
    }
    let mut replay = ReplayIndex {
        states: BTreeMap::from([
            (
                anchor.clone(),
                SnapshotState {
                    state: anchor_state,
                    provenance: BTreeSet::new(),
                },
            ),
            (
                bootstrap.clone(),
                SnapshotState {
                    state: bootstrap_transition.result_identity_state(),
                    provenance: BTreeSet::new(),
                },
            ),
        ]),
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
                replay.conflicts.insert(result.clone());
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

fn decode_lower_hex(text: &str) -> Result<Vec<u8>, IdentityStoreError> {
    if text.len() % 2 != 0 {
        return Err(IdentityStoreError::Corrupt(
            "source change hex field has an odd number of digits".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let high = lower_hex_nibble(pair[0]).ok_or_else(|| {
            IdentityStoreError::Corrupt("source change hex field is not lowercase hex".into())
        })?;
        let low = lower_hex_nibble(pair[1]).ok_or_else(|| {
            IdentityStoreError::Corrupt("source change hex field is not lowercase hex".into())
        })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
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
    let path = decode_lower_hex(text_field(state, "path_hex")?)?;
    let kind = EntryKind::from_git_mode(text_field(state, "mode")?)
        .map_err(|error| IdentityStoreError::Corrupt(error.to_string()))?;
    let digest: [u8; 32] = decode_lower_hex(text_field(state, "content_sha256")?)?
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

fn identity_proof_artifacts(
    replay: &ReplayIndex,
    base_state: &ResolvedProjectFileIdentityState,
    current_record_id: &RecordId,
    current_object_digest: ObjectDigest,
    current_brief_digest: Option<ObjectDigest>,
) -> Result<(BTreeSet<RecordId>, BTreeSet<ObjectDigest>), IdentityStoreError> {
    let mut revision_ids = provenance_ids(base_state.provenance());
    revision_ids.insert(current_record_id.clone());
    let mut object_digests = BTreeSet::new();
    for revision_id in &revision_ids {
        if revision_id == current_record_id {
            object_digests.insert(current_object_digest);
            object_digests.extend(current_brief_digest);
            continue;
        }
        let row = replay.rows.get(revision_id).ok_or_else(|| {
            IdentityStoreError::Corrupt(format!(
                "validated base provenance names missing revision {}",
                revision_id.as_str()
            ))
        })?;
        object_digests.insert(row.object_digest);
        object_digests.extend(row.brief_object_digest);
    }
    Ok((revision_ids, object_digests))
}

fn provenance_ids(provenance: &IdentityStateProvenance) -> BTreeSet<RecordId> {
    match provenance {
        IdentityStateProvenance::DescriptorAnchor { .. }
        | IdentityStateProvenance::ProjectBindingBootstrap { .. } => BTreeSet::new(),
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
    } else if snapshot == binding.bootstrap_snapshot() {
        IdentityStateProvenance::ProjectBindingBootstrap {
            descriptor_record_id: binding.descriptor_record_id.clone(),
            bootstrap_snapshot: binding.bootstrap_snapshot.clone(),
            committed_config_commit: binding.committed_config_commit.clone(),
            transition_sha256: binding.bootstrap_transition_sha256.clone(),
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
                    ObjectDigest::from_bytes(file.content_sha256).hex(),
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
