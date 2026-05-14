//! Storage layer for Aethyme graph fragments.
//!
//! This crate owns the on-disk format. `aethyme-graph-schema` owns
//! the in-memory types; this crate's job is to (a) write them to
//! disk deterministically, (b) read them back losslessly, and
//! (c) maintain the directory layout described in
//! `docs/architecture/graph-schema.md §5`.
//!
//! ## Storage layout (Option C)
//!
//! ```text
//! <repo>/
//! ├── .aethyme/
//! │   ├── engine-version           ← plain text, committed
//! │   ├── graph/                   ← committed
//! │   │   ├── <source-path>.bin    ← per-file binary fragment
//! │   │   ├── ...
//! │   │   └── _index/              ← committed per-module index shards
//! │   │       ├── <module>.ndjson
//! │   │       └── ...
//! │   └── cache/                   ← gitignored
//! │       └── *.redb               ← daemon's live mirror
//! ```
//!
//! ## Module layout
//!
//! - `fragment` — `Fragment` struct (the in-memory representation
//!   of one per-file graph file's worth of nodes + edges)
//! - `binary` — bincode-based read/write for fragments
//!   (commit 2.3+)
//! - `index_shard` — NDJSON read/write for per-module index shards
//!   (commit 2.4)
//! - `layout` — filesystem layout helpers (resolves a source path
//!   to its fragment path, etc.) (commit 2.5)
//!
//! ## Determinism contract
//!
//! Every public write path must produce byte-identical output for
//! the same input on every machine. The crate's tests pin this
//! property via cross-construction byte equality and snapshot
//! checks. Non-determinism here is the forever-bug that the schema
//! doc §5.4 was written to prevent.

pub mod binary;
pub mod fragment;

pub use binary::{
    read_fragment_bytes, write_fragment_bytes, FragmentDecodeError,
    FragmentEncodeError,
};
pub use fragment::{Fragment, FragmentBuildError};
