//! redb-backed graph store for the Aethyme engine.
//!
//! This module is the local materialized read model for the committed graph
//! fragments under `<repo>/.aethyme/graph/`. The current writer persists
//! repositories, directories, files, areas, functions, classes, docs, configs,
//! surface/flow facts, unresolved/import placeholders, risks, and file/symbol
//! adjacency for query, symbol, rendered graph, graph-expand, task-expand, task
//! anchors/scope/next/localize, context-pack, activation, and
//! non-usage-boundary `explore` views, plus usage-boundary seed discovery. The
//! hybrid `callers` path still greps first, then expands candidate files
//! through redb adjacency. Usage-boundary remains hybrid too: redb supplies
//! symbols and candidate files, while source text supplies evidence.
//!
//! Non-scope for the current redb store: this file is not the durable graph
//! format, does not mutate fragment files, does not promise in-place redb file
//! migrations, and is not a daemon-owned live graph. If
//! `.aethyme/graph_store.redb` is missing or incompatible, rebuild it from
//! fragments with `aethyme-engine-cli index --repo <repo>`.
//!
//! Historical context: this replaced the old SurrealDB-backed `GraphStore`.
//! `docs/architecture/phase3-redb-graph-store-plan.md` preserves the migration
//! rationale; `docs/architecture/graph-schema.md` owns the current contract.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use redb::{
    Database, MultimapTableDefinition, ReadOnlyDatabase, ReadTransaction, ReadableDatabase,
    ReadableMultimapTable, ReadableTable, TableDefinition, WriteTransaction,
};
use serde::{Deserialize, Serialize};

use crate::model::area::AreaNode;
use crate::model::class::ClassNode;
use crate::model::config::ConfigNode;
use crate::model::directory::DirectoryNode;
use crate::model::doc::DocNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::model::file::FileNode;
use crate::model::function::FunctionNode;
use crate::model::intern::InternedStr;
use crate::model::repository::RepositoryNode;
use crate::model::risk::RiskFlag;
use crate::model::surface::{SurfaceKind, SurfaceNode};
use crate::model::unresolved::UnresolvedNode;

/// Bumped when the on-disk format changes incompatibly. We re-create the file
/// rather than try to migrate.
const SCHEMA_VERSION: u32 = 8;

/// Public compatibility identity used by the immutable materialization cache.
pub const GRAPH_STORE_SCHEMA_VERSION: u32 = SCHEMA_VERSION;

/// Single-row metadata table: schema version, build timestamps, repo root, ...
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_KEY_SCHEMA_VERSION: &str = "schema_version";

// ── Node tables ─────────────────────────────────────────────────────────────
// One table per kind keeps tablespaces separate so prefix-range scans on
// `path/` don't have to skip over unrelated kinds. Key = node id (raw &str so
// scope queries can range over it). Value = bincoded entity record.
//
// Current writer note: all typed tables below are populated by the index
// command. A schema-version bump protects query-only callers from older local
// stores where FUNCTIONS/CLASSES/DOCS/CONFIGS existed but were schema-ready
// rather than semantically populated.

const REPOSITORIES: TableDefinition<&str, &[u8]> = TableDefinition::new("repositories");
const DIRECTORIES: TableDefinition<&str, &[u8]> = TableDefinition::new("directories");
const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const AREAS: TableDefinition<&str, &[u8]> = TableDefinition::new("areas");
const FUNCTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("functions");
const CLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("classes");
const DOCS: TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
const CONFIGS: TableDefinition<&str, &[u8]> = TableDefinition::new("configs");
const SURFACES: TableDefinition<&str, &[u8]> = TableDefinition::new("surfaces");
const UNRESOLVED: TableDefinition<&str, &[u8]> = TableDefinition::new("unresolved");

// ── Adjacency (the wedge for ego/impact/dead-code queries) ──────────────────
// Both directions are first-class (informed by the `edges_by_target`
// algorithmic fix that turned MediaWiki dead-code from O(F·E) to O(F·in_deg)).
// Value = bincoded AdjacencyRecord (kind, other_node_id, confidence, source).

const EDGES_OUT: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_out");
const EDGES_IN: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_in");
const EDGES_BY_KIND: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("edges_by_kind");

// ── Scope-bounded lookups (raw paths give free prefix range reads) ──────────
// Key = file_path. Value = node id. A range scan from "includes/" to
// "includes/\xff" yields all symbols under that scope.
//
// Current writer note: NODES_BY_PATH is the broad path index for directories,
// files, classes, functions, docs, configs, and unresolved/import
// placeholders. FUNCTIONS_BY_PATH remains a narrower hot index for file-scoped
// symbol lookups.

const FUNCTIONS_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("functions_by_path");
const NODES_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("nodes_by_path");

// ── Symbol search ───────────────────────────────────────────────────────────
// Key = lowercased name. Value = node id.
//
// Current writer note: populated for function and class names. Name keys are
// ASCII-lowercased simple names, component keys are acronym-aware name tokens,
// and path-component keys are bounded location tokens extracted from the owning
// file path. V2 fuzzy ranking is computed at read time over the candidate rows;
// these tables only provide bounded lookup sets.

const SYMBOL_BY_NAME: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_name");
const SYMBOL_BY_COMPONENT: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_component");
const SYMBOL_BY_PATH_COMPONENT: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_path_component");

// ── Risk overlays ───────────────────────────────────────────────────────────

const RISK_FLAGS: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("risk_flags");

/// Rotate the in-flight write transaction after this many ops.
/// Bounds fsync rate and the size of any single committed batch.
const ROTATE_EVERY_OPS: usize = 4096;

/// Rotate the in-flight write transaction after this many bytes.
/// Bounds the in-memory dirty-page footprint of a single transaction.
const ROTATE_EVERY_BYTES: usize = 8 * 1024 * 1024;

/// META key under which `RepoMetadata` is bincoded.
const META_KEY_REPO_METADATA: &str = "repo_metadata";

/// One-shot repo-level metadata written at the end of an index pass. Mirrors
/// the fields the SurrealDB version put on the `repo` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMetadata {
    pub root_path: String,
    pub commit_hash: Option<String>,
    pub indexed_at_unix: i64,
    pub file_count: u64,
    pub languages: Vec<String>,
}

/// Adjacency-table value layout (Variant B from the schema decision).
///
/// Stored under EDGES_OUT keyed by `src`, and under EDGES_IN keyed by `dst`.
/// The `other` field carries `dst` in EDGES_OUT and `src` in EDGES_IN, so a
/// caller iterating either direction sees the opposite endpoint without a
/// cross-table lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AdjacencyRecord {
    pub kind: EdgeKind,
    pub other: InternedStr,
    pub confidence: u16,
    pub source: InternedStr,
}

/// Strictly bounded callable ids contained by one exact repository-relative
/// file path. The multimap value order is deterministic, so callers can use
/// this directly as seed order without scanning a broader path prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFunctionIds {
    pub ids: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum GraphStoreError {
    Io(std::io::Error),
    Db(redb::Error),
    Encode(bincode::Error),
    SchemaMismatch { found: u32, expected: u32 },
    MissingGraphStore { path: PathBuf },
    IncompatibleRedbFileFormat { path: PathBuf, found: u8 },
}

impl std::fmt::Display for GraphStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Db(e) => write!(f, "redb: {e}"),
            Self::Encode(e) => write!(f, "bincode: {e}"),
            Self::SchemaMismatch { found, expected } => {
                write!(
                    f,
                    "graph store schema mismatch: found v{found}, expected v{expected}"
                )
            }
            Self::MissingGraphStore { path } => write!(
                f,
                "graph store at {} is missing; rebuild it from committed fragments with `aethyme-engine-cli index --repo <repo>`. Query commands are read-only and will not create it.",
                path.display()
            ),
            Self::IncompatibleRedbFileFormat { path, found } => write!(
                f,
                "graph store at {} uses old redb file format v{found}; regenerate it from committed fragments with `aethyme-engine-cli index --repo <repo>`. The `.aethyme/graph/` fragments are not modified.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for GraphStoreError {}

impl From<std::io::Error> for GraphStoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<redb::Error> for GraphStoreError {
    fn from(e: redb::Error) -> Self {
        Self::Db(e)
    }
}
impl From<redb::DatabaseError> for GraphStoreError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::TransactionError> for GraphStoreError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::TableError> for GraphStoreError {
    fn from(e: redb::TableError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::StorageError> for GraphStoreError {
    fn from(e: redb::StorageError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::CommitError> for GraphStoreError {
    fn from(e: redb::CommitError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::CompactionError> for GraphStoreError {
    fn from(e: redb::CompactionError) -> Self {
        Self::Db(e.into())
    }
}
impl From<redb::SetDurabilityError> for GraphStoreError {
    fn from(e: redb::SetDurabilityError) -> Self {
        Self::Db(e.into())
    }
}
impl From<bincode::Error> for GraphStoreError {
    fn from(e: bincode::Error) -> Self {
        Self::Encode(e)
    }
}

/// Handle to a redb database holding the graph for one repository.
///
/// Lives at `<repo_root>/.aethyme/graph_store.redb`. Single file, overwritten
/// on rebuild — same lifecycle as the SurrealDB store it replaces.
pub struct GraphStore {
    db: Database,
    #[allow(dead_code)]
    db_path: PathBuf,
}

/// Read-only handle for commands that inspect an existing graph store.
///
/// Unlike `GraphStore::open`, this never creates or mutates
/// `<repo_root>/.aethyme/graph_store.redb`. Use it for query CLI paths so
/// inspectors do not take a writable database handle.
pub struct ReadOnlyGraphStore {
    db: ReadOnlyDatabase,
    #[allow(dead_code)]
    db_path: PathBuf,
}

const DB_FILE_NAME: &str = "graph_store.redb";
const STAGING_DB_FILE_NAME: &str = "graph_store.redb.indexing";

/// Durability policy for bulk graph-store index transactions.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IndexDurability {
    /// Every redb commit is durable when `commit()` returns.
    Immediate,
    /// Bulk commits may remain non-durable until followed by an immediate
    /// commit. Only use this for disposable rebuilds from committed fragments.
    None,
}

impl IndexDurability {
    fn apply(self, txn: &mut WriteTransaction) -> Result<(), GraphStoreError> {
        let durability = match self {
            Self::Immediate => redb::Durability::Immediate,
            Self::None => redb::Durability::None,
        };
        txn.set_durability(durability)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleGraphStore {
    pub path: PathBuf,
    pub found_redb_format: u8,
}

mod lifecycle;
mod listing;
mod nodes;
mod queries;
mod read_types;
mod relations;
mod surface_flow;
mod symbols;
mod write;

#[cfg(test)]
mod tests;

pub use read_types::*;
pub use write::*;

use listing::*;
use nodes::*;
use relations::*;
use surface_flow::*;
use symbols::*;
