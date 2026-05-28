//! Storage layer for Aethyme graph fragments.
//!
//! This crate owns the on-disk format. `aethyme-graph-schema` owns
//! the in-memory types; this crate's job is to (a) write them to
//! disk deterministically, (b) read them back losslessly, and
//! (c) maintain the directory layout described in
//! `docs/architecture/graph-schema.md §5`.
//!
//! ## Storage layout (Option C / variant C1)
//!
//! ```text
//! <repo>/
//! ├── .aethyme/
//! │   ├── engine-version           ← plain text, committed
//! │   ├── graph/                   ← committed
//! │   │   ├── <source-path>.bin    ← per-file binary fragment
//! │   │   ├── ...
//! │   │   ├── _index/              ← committed per-module index shards
//! │   │   │   ├── <module>.ndjson
//! │   │   │   └── ...
//! │   │   └── _overlays/           ← committed non-per-file producer output
//! │   │       ├── <kind>.bin        (Phase 4.7+ — structure, configs,
//! │   │       └── ...               docs, risks, areas, …)
//! │   ├── graph_store.redb         ← current local engine redb graph store
//! │   └── cache/                   ← gitignored
//! │       └── *.redb               ← future daemon-local mirrors
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
//! - `overlay` — typed non-per-file producer payloads
//!   (`OverlayFragment<P>`, commit 4.7.1+). Lets producers that
//!   synthesize repo-level structure rather than parsing one source
//!   file persist through the same crate, without forcing every
//!   payload into one giant enum.
//!
//! ## Determinism contract
//!
//! Every public write path must produce byte-identical output for
//! the same input on every machine. The crate's tests pin this
//! property via cross-construction byte equality and snapshot
//! checks. Non-determinism here is the forever-bug that the schema
//! doc §5.4 was written to prevent.

pub mod binary;
pub mod bootstrap;
pub mod disk;
pub mod fragment;
pub mod index_shard;
pub mod layout;
pub mod overlay;
pub mod store;

pub use binary::{
    read_fragment_bytes, write_fragment_bytes, FragmentDecodeError,
    FragmentEncodeError,
};
pub use bootstrap::{
    bootstrap_repo, read_engine_version, BootstrapError, BootstrapPaths,
    EngineVersionReadError, GITATTRIBUTES_CONTENT,
};
pub use disk::{
    read_fragment, read_index_shard, read_overlay, write_fragment,
    write_index_shard, write_overlay, FragmentReadError, FragmentWriteError,
    IndexShardReadError, IndexShardWriteError, OverlayReadError,
    OverlayWriteError,
};
pub use fragment::{Fragment, FragmentBuildError};
pub use index_shard::{
    dedupe_and_canonicalize, read_index_shard_bytes,
    write_index_shard_bytes, IndexShardDecodeError, IndexShardEncodeError,
    SymbolRecord,
};
pub use layout::{
    cache_dir, engine_version_path, fragment_path, index_shard_path,
    overlay_path, validate_module_name, validate_source_path, InvalidPath,
    AETHYME_DIR, CACHE_SUBDIR, ENGINE_VERSION_FILE, FRAGMENT_EXT,
    GRAPH_SUBDIR, INDEX_SHARD_EXT, INDEX_SUBDIR, OVERLAYS_SUBDIR,
};
pub use overlay::{
    read_overlay_bytes, write_overlay_bytes, OverlayBuildError,
    OverlayDecodeError, OverlayEncodeError, OverlayFragment,
};
pub use store::{FragmentStore, StoreLookupError, StoreOpenError};
