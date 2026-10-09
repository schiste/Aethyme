//! Experimental v0 contracts. See the crate docs for the stability rule.

pub mod analysis;
pub mod brief;
pub mod canonical_json;
mod digest;
pub mod record;
pub mod source_snapshot;

pub use digest::ID_PREFIX;
pub use record::{FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema, StateReading};
pub use source_snapshot::{
    EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotError, SourceSnapshotId,
};
