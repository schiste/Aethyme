//! Experimental v0 contracts. See the crate docs for the stability rule.

pub mod analysis;
pub mod brief;
pub mod canonical_json;
pub mod contribution_revision;
mod digest;
pub mod project_association;
pub mod project_identity;
pub mod record;
pub mod source_snapshot;
pub mod symbol_identity;

pub use contribution_revision::{
    BriefReference, ContributionId, ContributionRevision, ContributionRevisionError,
};
pub use digest::ID_PREFIX;
pub use project_association::{ProjectAssociationDescriptor, ProjectAssociationError};
pub use project_identity::{
    EnrolledFile, FileChange, FileId, FileIdentityTransition, FileNamespaceConflict,
    FileNamespaceConflictKind, FileOperation, FileState, IdentityRelation, ProjectDescriptor,
    ProjectFileIdentityState, ProjectId, ProjectIdentityError, advance_file_identities,
    validate_file_proposals,
};
pub use record::{FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema, StateReading};
pub use source_snapshot::{
    EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotError, SourceSnapshotId,
};
