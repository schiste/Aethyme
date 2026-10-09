//! Experimental v0 contracts. See the crate docs for the stability rule.

pub mod source_snapshot;

pub use source_snapshot::{
    EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotError, SourceSnapshotId,
};
