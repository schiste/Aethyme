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

impl GraphStore {
    /// Open or create the graph store for a repository. Verifies / writes the
    /// schema version sentinel and ensures every table exists so downstream
    /// reads on a fresh DB don't trip on `TableDoesNotExist`.
    pub fn open(repo_root: &Path) -> Result<Self, GraphStoreError> {
        Self::open_path(Self::final_path(repo_root))
    }

    /// Open an existing graph store for read-only queries.
    ///
    /// This is intentionally stricter than `open()`: it will not create
    /// `.aethyme/` or initialize an empty DB. Query commands should fail with
    /// a clear error if the materialized store has not been built yet.
    pub fn open_read_only(repo_root: &Path) -> Result<ReadOnlyGraphStore, GraphStoreError> {
        ReadOnlyGraphStore::open(repo_root)
    }

    /// Detect an existing redb file that the current engine cannot open
    /// because it was written by an older redb file format. Used by the index
    /// CLI to print an explicit regeneration notice before deleting the local
    /// materialized store.
    pub fn detect_incompatible_file_format(repo_root: &Path) -> Option<IncompatibleGraphStore> {
        let db_path = repo_root.join(".aethyme").join(DB_FILE_NAME);
        if !db_path.exists() {
            return None;
        }
        match Database::open(&db_path) {
            Err(redb::DatabaseError::UpgradeRequired(found)) => Some(IncompatibleGraphStore {
                path: db_path,
                found_redb_format: found,
            }),
            _ => None,
        }
    }

    /// Path to the DB file on disk.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Public graph-store path consumed by query commands and verification.
    pub fn final_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".aethyme").join(DB_FILE_NAME)
    }

    /// Private staging path used by disposable-fast rebuilds.
    pub fn staging_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".aethyme").join(STAGING_DB_FILE_NAME)
    }

    /// Borrow the underlying redb `Database` — used by the build session and
    /// query primitives that land in 3.2–3.4.
    #[allow(dead_code)]
    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    /// Compact the underlying redb file after all write transactions have
    /// committed. Returns whether redb moved pages during compaction.
    pub fn compact(&mut self) -> Result<bool, GraphStoreError> {
        Ok(self.db.compact()?)
    }

    /// Open a Variant-B index session. The session holds one open
    /// `WriteTransaction` and rotates it periodically based on
    /// `IndexSession::should_rotate`. Drop without `commit()` aborts.
    pub fn begin_index(&self) -> Result<IndexSession<'_>, GraphStoreError> {
        self.begin_index_with_durability(IndexDurability::Immediate)
    }

    /// Open an index session with an explicit redb durability policy.
    pub fn begin_index_with_durability(
        &self,
        durability: IndexDurability,
    ) -> Result<IndexSession<'_>, GraphStoreError> {
        let mut txn = self.db.begin_write()?;
        durability.apply(&mut txn)?;
        Ok(IndexSession {
            db: &self.db,
            txn: Some(txn),
            durability,
            ops_since_rotate: 0,
            bytes_since_rotate: 0,
        })
    }

    /// Drop all data and re-apply the schema. Mirrors the SurrealDB version's
    /// `reset()`, used at the start of every full index pass. Implemented as
    /// "delete the file, recreate it" — cheaper and simpler than range-deleting
    /// every table.
    pub fn reset(repo_root: &Path) -> Result<Self, GraphStoreError> {
        let staging_path = Self::staging_path(repo_root);
        if staging_path.exists() {
            std::fs::remove_file(&staging_path)?;
        }
        let db_path = Self::final_path(repo_root);
        if db_path.exists() {
            std::fs::remove_file(&db_path)?;
        }
        Self::open(repo_root)
    }

    /// Reset the disposable staging store without touching the public store.
    pub fn reset_staging(repo_root: &Path) -> Result<Self, GraphStoreError> {
        let db_path = Self::staging_path(repo_root);
        if db_path.exists() {
            std::fs::remove_file(&db_path)?;
        }
        Self::open_path(db_path)
    }

    /// Publish a fully-built staging store over the public graph-store path.
    pub fn publish_staging(repo_root: &Path) -> Result<(), GraphStoreError> {
        let staging_path = Self::staging_path(repo_root);
        let final_path = Self::final_path(repo_root);
        match std::fs::rename(&staging_path, &final_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists && final_path.exists() => {
                std::fs::remove_file(&final_path)?;
                std::fs::rename(&staging_path, &final_path)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn open_path(db_path: PathBuf) -> Result<Self, GraphStoreError> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = open_or_create_database(&db_path)?;
        ensure_schema(&db)?;
        Ok(Self { db, db_path })
    }

    /// Write repo-level metadata (root_path, commit, indexed_at, file_count,
    /// languages). Called once at the end of an index pass — uses its own
    /// short-lived transaction rather than going through IndexSession.
    pub fn set_repo_metadata(&self, meta: &RepoMetadata) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(meta)?;
        let txn = self.db.begin_write()?;
        {
            let mut t = txn.open_table(META)?;
            t.insert(META_KEY_REPO_METADATA, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Read previously-written repo metadata, if any.
    pub fn repo_metadata(&self) -> Result<Option<RepoMetadata>, GraphStoreError> {
        repo_metadata_from(&self.db)
    }

    /// Remove the file row at `file_id` and every adjacency row touching it.
    ///
    /// Mirrors the SurrealDB writer's `delete_file_data` scope: file row +
    /// edges. Does not touch areas, risks, or symbol-level tables (functions
    /// and classes aren't persisted today). Used for incremental re-indexing.
    ///
    /// Two-pass: first read out the partner sets, then delete from both
    /// adjacency tables. Reading and writing in the same write transaction is
    /// fine — redb's read-your-own-writes within a txn covers it.
    pub fn delete_file_data(&self, file_id: &str) -> Result<(), GraphStoreError> {
        let txn = self.db.begin_write()?;
        {
            // Collect partners before mutating, since we'll delete from both
            // EDGES_OUT (keyed by src) and EDGES_IN (keyed by dst).
            let outgoing_partners: Vec<(Vec<u8>, InternedStr)> = {
                let out = txn.open_multimap_table(EDGES_OUT)?;
                out.get(file_id)?
                    .map(|r| {
                        let v = r?.value().to_vec();
                        let rec: AdjacencyRecord = bincode::deserialize(&v)?;
                        Ok::<_, GraphStoreError>((v, rec.other))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let incoming_partners: Vec<(Vec<u8>, InternedStr)> = {
                let inv = txn.open_multimap_table(EDGES_IN)?;
                inv.get(file_id)?
                    .map(|r| {
                        let v = r?.value().to_vec();
                        let rec: AdjacencyRecord = bincode::deserialize(&v)?;
                        Ok::<_, GraphStoreError>((v, rec.other))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };

            // Delete edges from EDGES_OUT[file_id] and the matching rows in
            // EDGES_IN[partner].
            {
                let mut out = txn.open_multimap_table(EDGES_OUT)?;
                out.remove_all(file_id)?;
            }
            {
                let mut inv = txn.open_multimap_table(EDGES_IN)?;
                for (out_bytes, partner) in &outgoing_partners {
                    // Reconstruct the matching EDGES_IN row: same kind, same
                    // confidence, same source, but `other` flipped to file_id.
                    let out_rec: AdjacencyRecord = bincode::deserialize(out_bytes)?;
                    let in_rec = AdjacencyRecord {
                        kind: out_rec.kind,
                        other: InternedStr::from(file_id),
                        confidence: out_rec.confidence,
                        source: out_rec.source,
                    };
                    let in_bytes = bincode::serialize(&in_rec)?;
                    inv.remove(partner.as_str(), in_bytes.as_slice())?;
                }
            }

            // Symmetric: delete EDGES_IN[file_id] and the matching rows in
            // EDGES_OUT[partner].
            {
                let mut inv = txn.open_multimap_table(EDGES_IN)?;
                inv.remove_all(file_id)?;
            }
            {
                let mut out = txn.open_multimap_table(EDGES_OUT)?;
                for (in_bytes, partner) in &incoming_partners {
                    let in_rec: AdjacencyRecord = bincode::deserialize(in_bytes)?;
                    let out_rec = AdjacencyRecord {
                        kind: in_rec.kind,
                        other: InternedStr::from(file_id),
                        confidence: in_rec.confidence,
                        source: in_rec.source,
                    };
                    let out_bytes = bincode::serialize(&out_rec)?;
                    out.remove(partner.as_str(), out_bytes.as_slice())?;
                }
            }
            {
                let mut by_kind = txn.open_multimap_table(EDGES_BY_KIND)?;
                for (out_bytes, partner) in &outgoing_partners {
                    let out_rec: AdjacencyRecord = bincode::deserialize(out_bytes)?;
                    let edge = Edge::new(
                        file_id,
                        partner.as_str(),
                        out_rec.kind.clone(),
                        out_rec.confidence,
                        out_rec.source,
                    );
                    let edge_bytes = bincode::serialize(&edge)?;
                    by_kind.remove(edge_kind_label(&out_rec.kind), edge_bytes.as_slice())?;
                }
                for (in_bytes, partner) in &incoming_partners {
                    let in_rec: AdjacencyRecord = bincode::deserialize(in_bytes)?;
                    let edge = Edge::new(
                        partner.as_str(),
                        file_id,
                        in_rec.kind.clone(),
                        in_rec.confidence,
                        in_rec.source,
                    );
                    let edge_bytes = bincode::serialize(&edge)?;
                    by_kind.remove(edge_kind_label(&in_rec.kind), edge_bytes.as_slice())?;
                }
            }

            // The file row itself.
            let mut files = txn.open_table(FILES)?;
            files.remove(file_id)?;
        }
        txn.commit()?;
        Ok(())
    }
}

impl ReadOnlyGraphStore {
    /// Open an existing Redb graph store without acquiring a writable handle.
    pub fn open(repo_root: &Path) -> Result<Self, GraphStoreError> {
        let db_path = repo_root.join(".aethyme").join(DB_FILE_NAME);
        let db = open_read_only_database(&db_path)?;
        verify_schema_read_only(&db)?;
        Ok(Self { db, db_path })
    }

    /// Path to the DB file on disk.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Borrow the underlying redb `ReadOnlyDatabase` for tests and future
    /// read-only query primitives.
    #[allow(dead_code)]
    pub(crate) fn db(&self) -> &ReadOnlyDatabase {
        &self.db
    }
}

// ── Typed write wrappers ────────────────────────────────────────────────────
// Thin shims over IndexSession primitives. The mapping is structured ID →
// raw redb key (no sanitization), entity → bincoded value, plus the secondary
// indexes each kind requires. Mirrors the surface of `super::super::write`.
//
// These wrappers are intentionally small: each one owns the table/index
// contract for its node kind so the CLI writer cannot forget a secondary index
// when adding a new persisted kind.

fn symbol_index_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn symbol_components(name: &str) -> BTreeSet<String> {
    split_symbol_components(name)
        .into_iter()
        .map(|component| component.to_ascii_lowercase())
        .filter(|component| !component.is_empty())
        .collect()
}

fn split_symbol_components(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();

    for (idx, ch) in chars.iter().copied().enumerate() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }

        if current.is_empty() {
            current.push(ch);
            continue;
        }

        let prev = chars[idx - 1];
        let lc_uc = (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && ch.is_ascii_uppercase();
        let acronym_break = prev.is_ascii_uppercase()
            && ch.is_ascii_uppercase()
            && idx + 1 < chars.len()
            && chars[idx + 1].is_ascii_lowercase();
        if lc_uc || acronym_break {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Insert (or overwrite) an area. Key = `area.id` (e.g. `area:Repo:src`).
pub fn insert_area(session: &mut IndexSession<'_>, area: &AreaNode) -> Result<(), GraphStoreError> {
    session.insert_node(AREAS, &area.id, area)
}

/// Insert (or overwrite) the repository container. Key = `repo:<name>`.
pub fn insert_repository(
    session: &mut IndexSession<'_>,
    repository: &RepositoryNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(REPOSITORIES, &repository.id, repository)
}

/// Insert (or overwrite) a directory/container and index it by its relative
/// path.
pub fn insert_directory(
    session: &mut IndexSession<'_>,
    directory: &DirectoryNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(DIRECTORIES, &directory.id, directory)?;
    session.add_path_index(NODES_BY_PATH, &directory.path, &directory.id)?;
    Ok(())
}

/// Insert (or overwrite) a file. Key = `file.id` (e.g. `file:Repo:src/lib.rs`).
/// Also adds `path → file.id` to NODES_BY_PATH so a path can be resolved to
/// the structured ID.
pub fn insert_file(session: &mut IndexSession<'_>, file: &FileNode) -> Result<(), GraphStoreError> {
    session.insert_node(FILES, &file.id, file)?;
    session.add_path_index(NODES_BY_PATH, &file.path, &file.id)?;
    Ok(())
}

/// Insert (or overwrite) a function. Also indexes by file path and
/// ASCII-lowercased simple name.
pub fn insert_function(
    session: &mut IndexSession<'_>,
    function: &FunctionNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(FUNCTIONS, function.id.as_str(), function)?;
    session.add_path_index(
        FUNCTIONS_BY_PATH,
        function.file_path.as_str(),
        function.id.as_str(),
    )?;
    session.add_path_index(
        NODES_BY_PATH,
        function.file_path.as_str(),
        function.id.as_str(),
    )?;
    let name_key = symbol_index_key(function.name.as_str());
    session.add_symbol_index(&name_key, function.id.as_str())?;
    for component in symbol_components(function.name.as_str()) {
        session.add_symbol_component_index(&component, function.id.as_str())?;
    }
    for component in symbol_components(function.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, function.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) a class-like symbol. Also indexes by file path and
/// ASCII-lowercased simple name.
pub fn insert_class(
    session: &mut IndexSession<'_>,
    class: &ClassNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(CLASSES, class.id.as_str(), class)?;
    session.add_path_index(NODES_BY_PATH, class.file_path.as_str(), class.id.as_str())?;
    let name_key = symbol_index_key(class.name.as_str());
    session.add_symbol_index(&name_key, class.id.as_str())?;
    for component in symbol_components(class.name.as_str()) {
        session.add_symbol_component_index(&component, class.id.as_str())?;
    }
    for component in symbol_components(class.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, class.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) a documentation node and index it by path.
pub fn insert_doc(session: &mut IndexSession<'_>, doc: &DocNode) -> Result<(), GraphStoreError> {
    session.insert_node(DOCS, &doc.id, doc)?;
    session.add_path_index(NODES_BY_PATH, &doc.path, &doc.id)?;
    Ok(())
}

/// Insert (or overwrite) a configuration node and index it by path.
pub fn insert_config(
    session: &mut IndexSession<'_>,
    config: &ConfigNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(CONFIGS, &config.id, config)?;
    session.add_path_index(NODES_BY_PATH, &config.path, &config.id)?;
    Ok(())
}

/// Insert (or overwrite) a Surface/Flow node. Also indexes by owning file path
/// and simple name so task anchors can find routes, middleware, workers, and
/// credential operations without scanning the full store.
pub fn insert_surface(
    session: &mut IndexSession<'_>,
    surface: &SurfaceNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(SURFACES, surface.id.as_str(), surface)?;
    session.add_path_index(
        NODES_BY_PATH,
        surface.file_path.as_str(),
        surface.id.as_str(),
    )?;
    let name_key = symbol_index_key(surface.name.as_str());
    session.add_symbol_index(&name_key, surface.id.as_str())?;
    for component in symbol_components(surface.name.as_str()) {
        session.add_symbol_component_index(&component, surface.id.as_str())?;
    }
    for component in symbol_components(surface.detail.as_str()) {
        session.add_symbol_component_index(&component, surface.id.as_str())?;
    }
    for component in symbol_components(surface.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, surface.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) an unresolved/import placeholder and index it by the
/// source file path where the unresolved reference was observed.
pub fn insert_unresolved(
    session: &mut IndexSession<'_>,
    unresolved: &UnresolvedNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(UNRESOLVED, unresolved.id.as_str(), unresolved)?;
    session.add_path_index(
        NODES_BY_PATH,
        unresolved.file_path.as_str(),
        unresolved.id.as_str(),
    )?;
    Ok(())
}

/// Insert one logical edge. Composes into `IndexSession::insert_edge` with
/// `Edge`'s fields unpacked.
pub fn insert_edge(session: &mut IndexSession<'_>, edge: &Edge) -> Result<(), GraphStoreError> {
    session.insert_edge(
        edge.from.as_str(),
        edge.to.as_str(),
        edge.kind.clone(),
        edge.confidence,
        edge.source.clone(),
    )
}

/// Insert a risk flag under its scope.
pub fn insert_risk(session: &mut IndexSession<'_>, risk: &RiskFlag) -> Result<(), GraphStoreError> {
    session.add_risk(&risk.scope, risk)
}

// ── Read primitives ─────────────────────────────────────────────────────────
// Mirror the surface of `super::super::read` (live functions only —
// `subgraph` and `files_in_area` were dead code in the SurrealDB version
// and are not ported).

/// Top-level overview returned from `GraphStore::overview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub repo: Option<RepoMetadata>,
    pub areas: Vec<AreaNode>,
    pub entrypoint_paths: Vec<String>,
    pub risks: Vec<RiskFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StoredNodeKind {
    Repository,
    Directory,
    File,
    Area,
    Function,
    Class,
    Doc,
    Config,
    BehaviorTestSurface,
    CliSurface,
    CredentialOperation,
    JobSurface,
    MiddlewareInstallation,
    ProxySurface,
    QueueSurface,
    RouteSurface,
    WebhookSurface,
    WorkerSurface,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredNode {
    Repository(RepositoryNode),
    Directory(DirectoryNode),
    File(FileNode),
    Area(AreaNode),
    Function(FunctionNode),
    Class(ClassNode),
    Doc(DocNode),
    Config(ConfigNode),
    Surface(SurfaceNode),
    Unresolved(UnresolvedNode),
}

impl StoredNode {
    pub fn id(&self) -> &str {
        match self {
            Self::Repository(node) => &node.id,
            Self::Directory(node) => &node.id,
            Self::File(node) => &node.id,
            Self::Area(node) => &node.id,
            Self::Function(node) => node.id.as_str(),
            Self::Class(node) => node.id.as_str(),
            Self::Doc(node) => &node.id,
            Self::Config(node) => &node.id,
            Self::Surface(node) => node.id.as_str(),
            Self::Unresolved(node) => node.id.as_str(),
        }
    }

    pub fn kind(&self) -> StoredNodeKind {
        match self {
            Self::Repository(_) => StoredNodeKind::Repository,
            Self::Directory(_) => StoredNodeKind::Directory,
            Self::File(_) => StoredNodeKind::File,
            Self::Area(_) => StoredNodeKind::Area,
            Self::Function(_) => StoredNodeKind::Function,
            Self::Class(_) => StoredNodeKind::Class,
            Self::Doc(_) => StoredNodeKind::Doc,
            Self::Config(_) => StoredNodeKind::Config,
            Self::Surface(node) => stored_kind_from_surface_kind(node.kind),
            Self::Unresolved(_) => StoredNodeKind::Unresolved,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Repository(node) => Some(&node.root_path),
            Self::Directory(node) => Some(&node.path),
            Self::File(node) => Some(&node.path),
            Self::Area(node) => Some(&node.path_prefix),
            Self::Function(node) => Some(node.file_path.as_str()),
            Self::Class(node) => Some(node.file_path.as_str()),
            Self::Doc(node) => Some(&node.path),
            Self::Config(node) => Some(&node.path),
            Self::Surface(node) => Some(node.file_path.as_str()),
            Self::Unresolved(node) => Some(node.file_path.as_str()),
        }
    }
}

fn stored_kind_from_surface_kind(kind: SurfaceKind) -> StoredNodeKind {
    match kind {
        SurfaceKind::BehaviorTestSurface => StoredNodeKind::BehaviorTestSurface,
        SurfaceKind::CliSurface => StoredNodeKind::CliSurface,
        SurfaceKind::CredentialOperation => StoredNodeKind::CredentialOperation,
        SurfaceKind::JobSurface => StoredNodeKind::JobSurface,
        SurfaceKind::MiddlewareInstallation => StoredNodeKind::MiddlewareInstallation,
        SurfaceKind::ProxySurface => StoredNodeKind::ProxySurface,
        SurfaceKind::QueueSurface => StoredNodeKind::QueueSurface,
        SurfaceKind::RouteSurface => StoredNodeKind::RouteSurface,
        SurfaceKind::WebhookSurface => StoredNodeKind::WebhookSurface,
        SurfaceKind::WorkerSurface => StoredNodeKind::WorkerSurface,
    }
}

fn is_surface_stored_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::BehaviorTestSurface
            | StoredNodeKind::CliSurface
            | StoredNodeKind::CredentialOperation
            | StoredNodeKind::JobSurface
            | StoredNodeKind::MiddlewareInstallation
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::QueueSurface
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::WorkerSurface
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLookup {
    pub id: String,
    pub kind: StoredNodeKind,
    pub name: String,
    pub path: String,
    pub line: usize,
    pub signature: String,
    pub language: String,
    pub area_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDisplay {
    pub id: String,
    pub kind: StoredNodeKind,
    pub display: String,
    pub name: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub area_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRelation {
    Children,
    Parents,
    Callers,
    Callees,
    Docs,
    Configs,
    Imports,
    Importers,
    References,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedbRelationItem {
    pub node: NodeDisplay,
    pub relation: String,
    pub edge_kind: EdgeKind,
    pub confidence: u16,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedbRelationView {
    pub target: Option<NodeDisplay>,
    pub relation: GraphRelation,
    pub items: Vec<RedbRelationItem>,
}

const MIN_STEM_LEN: usize = 4;
const NAME_BASE: i32 = 100;
const NAME_COMPOUND_PER_EXTRA: i32 = 150;
const PATH_PER_TOKEN: i32 = 60;
const AREA_PER_TOKEN: i32 = 40;
const BASENAME_EXACT_BONUS: i32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SymbolMatchSignals {
    pub exact: bool,
    pub case_insensitive: bool,
    pub prefix: bool,
    pub component: bool,
    pub path: bool,
    pub area: bool,
    pub basename: bool,
}

impl SymbolMatchSignals {
    fn signal_count(&self) -> u8 {
        self.exact as u8
            + self.case_insensitive as u8
            + self.prefix as u8
            + self.component as u8
            + self.path as u8
            + self.area as u8
            + self.basename as u8
    }

    fn merge(&mut self, other: Self) {
        self.exact |= other.exact;
        self.case_insensitive |= other.case_insensitive;
        self.prefix |= other.prefix;
        self.component |= other.component;
        self.path |= other.path;
        self.area |= other.area;
        self.basename |= other.basename;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolCandidate {
    pub symbol: SymbolLookup,
    pub signals: SymbolMatchSignals,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatchOptions {
    pub limit: usize,
    pub kind: Option<StoredNodeKind>,
    pub path_prefix: Option<String>,
    pub area_id: Option<String>,
}

impl Default for SymbolMatchOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            kind: None,
            path_prefix: None,
            area_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAnchorCandidate {
    pub node: NodeDisplay,
    pub signals: SymbolMatchSignals,
    pub matched_tokens: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBoundaryCandidate {
    pub node: NodeDisplay,
    pub symbol: Option<SymbolLookup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceFlowCandidate {
    pub node: NodeDisplay,
    pub signals: SymbolMatchSignals,
    pub matched_tokens: Vec<String>,
    pub relation_kinds: Vec<EdgeKind>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacePathCandidate {
    pub path: String,
    pub surfaces: Vec<NodeDisplay>,
    pub matched_tokens: Vec<String>,
    pub relation_kinds: Vec<EdgeKind>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRelationStep {
    pub from: NodeDisplay,
    pub to: NodeDisplay,
    pub edge_kind: EdgeKind,
    pub confidence: u16,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowChain {
    pub roots: Vec<NodeDisplay>,
    pub steps: Vec<FlowRelationStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsystemCandidate {
    pub id: Option<String>,
    pub path_prefix: String,
    pub matched_tokens: Vec<String>,
    pub nodes: Vec<NodeDisplay>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskClassCoverage {
    pub task_class: String,
    pub tokens: Vec<String>,
    pub entrypoints: Vec<SurfaceFlowCandidate>,
    pub surface_paths: Vec<SurfacePathCandidate>,
    pub credential_flows: Vec<SurfaceFlowCandidate>,
    pub subsystems: Vec<SubsystemCandidate>,
    pub tests: Vec<NodeDisplay>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeighborDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewV2Limits {
    pub area_limit: usize,
    pub directory_limit: usize,
    pub entrypoint_limit: usize,
    pub risk_limit: usize,
    pub file_limit: usize,
    pub function_limit: usize,
    pub class_limit: usize,
    pub doc_limit: usize,
    pub config_limit: usize,
    pub surface_limit: usize,
    pub unresolved_limit: usize,
}

impl Default for OverviewV2Limits {
    fn default() -> Self {
        Self {
            area_limit: 20,
            directory_limit: 20,
            entrypoint_limit: 10,
            risk_limit: 20,
            file_limit: 20,
            function_limit: 20,
            class_limit: 20,
            doc_limit: 10,
            config_limit: 10,
            surface_limit: 20,
            unresolved_limit: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewV2 {
    pub repo: Option<RepoMetadata>,
    pub repository: Option<RepositoryNode>,
    pub areas: Vec<AreaNode>,
    pub directories: Vec<DirectoryNode>,
    pub entrypoint_paths: Vec<String>,
    pub risks: Vec<RiskFlag>,
    pub files: Vec<FileNode>,
    pub functions: Vec<FunctionNode>,
    pub classes: Vec<ClassNode>,
    pub docs: Vec<DocNode>,
    pub configs: Vec<ConfigNode>,
    pub surfaces: Vec<SurfaceNode>,
    pub unresolved: Vec<UnresolvedNode>,
}

fn area_depth(area: &AreaNode) -> u32 {
    (area.path_prefix.matches('/').count() + 1) as u32
}

fn node_kind_from_id(id: &str) -> Option<StoredNodeKind> {
    if id.starts_with("repo:") {
        Some(StoredNodeKind::Repository)
    } else if id.starts_with("dir:") {
        Some(StoredNodeKind::Directory)
    } else if id.starts_with("file:") {
        Some(StoredNodeKind::File)
    } else if id.starts_with("area:") {
        Some(StoredNodeKind::Area)
    } else if id.starts_with("fn:") {
        Some(StoredNodeKind::Function)
    } else if id.starts_with("class:") {
        Some(StoredNodeKind::Class)
    } else if id.starts_with("doc:") {
        Some(StoredNodeKind::Doc)
    } else if id.starts_with("config:") {
        Some(StoredNodeKind::Config)
    } else if id.starts_with("behavior_test_surface:") {
        Some(StoredNodeKind::BehaviorTestSurface)
    } else if id.starts_with("cli_surface:") {
        Some(StoredNodeKind::CliSurface)
    } else if id.starts_with("credential_operation:") {
        Some(StoredNodeKind::CredentialOperation)
    } else if id.starts_with("job_surface:") {
        Some(StoredNodeKind::JobSurface)
    } else if id.starts_with("middleware_installation:") {
        Some(StoredNodeKind::MiddlewareInstallation)
    } else if id.starts_with("proxy_surface:") {
        Some(StoredNodeKind::ProxySurface)
    } else if id.starts_with("queue_surface:") {
        Some(StoredNodeKind::QueueSurface)
    } else if id.starts_with("route_surface:") {
        Some(StoredNodeKind::RouteSurface)
    } else if id.starts_with("webhook_surface:") {
        Some(StoredNodeKind::WebhookSurface)
    } else if id.starts_with("worker_surface:") {
        Some(StoredNodeKind::WorkerSurface)
    } else if id.starts_with("unresolved_symbol:") || id.starts_with("import:") {
        Some(StoredNodeKind::Unresolved)
    } else {
        None
    }
}

fn read_table_node<T: for<'de> Deserialize<'de>>(
    txn: &ReadTransaction,
    table: TableDefinition<&str, &[u8]>,
    id: &str,
) -> Result<Option<T>, GraphStoreError> {
    let t = txn.open_table(table)?;
    let Some(value) = t.get(id)? else {
        return Ok(None);
    };
    Ok(Some(bincode::deserialize(value.value())?))
}

fn get_node_in_txn(txn: &ReadTransaction, id: &str) -> Result<Option<StoredNode>, GraphStoreError> {
    let Some(kind) = node_kind_from_id(id) else {
        return Ok(None);
    };
    match kind {
        StoredNodeKind::Repository => {
            Ok(read_table_node::<RepositoryNode>(txn, REPOSITORIES, id)?
                .map(StoredNode::Repository))
        }
        StoredNodeKind::Directory => {
            Ok(read_table_node::<DirectoryNode>(txn, DIRECTORIES, id)?.map(StoredNode::Directory))
        }
        StoredNodeKind::File => {
            Ok(read_table_node::<FileNode>(txn, FILES, id)?.map(StoredNode::File))
        }
        StoredNodeKind::Area => {
            Ok(read_table_node::<AreaNode>(txn, AREAS, id)?.map(StoredNode::Area))
        }
        StoredNodeKind::Function => {
            Ok(read_table_node::<FunctionNode>(txn, FUNCTIONS, id)?.map(StoredNode::Function))
        }
        StoredNodeKind::Class => {
            Ok(read_table_node::<ClassNode>(txn, CLASSES, id)?.map(StoredNode::Class))
        }
        StoredNodeKind::Doc => Ok(read_table_node::<DocNode>(txn, DOCS, id)?.map(StoredNode::Doc)),
        StoredNodeKind::Config => {
            Ok(read_table_node::<ConfigNode>(txn, CONFIGS, id)?.map(StoredNode::Config))
        }
        StoredNodeKind::BehaviorTestSurface
        | StoredNodeKind::CliSurface
        | StoredNodeKind::CredentialOperation
        | StoredNodeKind::JobSurface
        | StoredNodeKind::MiddlewareInstallation
        | StoredNodeKind::ProxySurface
        | StoredNodeKind::QueueSurface
        | StoredNodeKind::RouteSurface
        | StoredNodeKind::WebhookSurface
        | StoredNodeKind::WorkerSurface => {
            Ok(read_table_node::<SurfaceNode>(txn, SURFACES, id)?.map(StoredNode::Surface))
        }
        StoredNodeKind::Unresolved => {
            Ok(read_table_node::<UnresolvedNode>(txn, UNRESOLVED, id)?.map(StoredNode::Unresolved))
        }
    }
}

fn get_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Option<StoredNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    get_node_in_txn(&txn, id)
}

fn get_nodes_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    ids: &[S],
) -> Result<Vec<StoredNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(node) = get_node_in_txn(&txn, id.as_ref())? {
            out.push(node);
        }
    }
    Ok(out)
}

fn node_display_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Option<NodeDisplay>, GraphStoreError> {
    let txn = db.begin_read()?;
    display_from_id_in_txn(&txn, id)
}

fn area_id_from_node(node: &StoredNode) -> Option<String> {
    match node {
        StoredNode::Repository(_) => None,
        StoredNode::Directory(node) => node.area_id.clone(),
        StoredNode::File(node) => node.area_id.clone(),
        StoredNode::Area(node) => Some(node.id.clone()),
        StoredNode::Function(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Class(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Doc(node) => node.area_id.clone(),
        StoredNode::Config(node) => node.area_id.clone(),
        StoredNode::Surface(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Unresolved(node) => node.area_id.as_ref().map(|id| id.to_string()),
    }
}

fn path_from_node(node: &StoredNode) -> Option<String> {
    node.path().map(str::to_string)
}

fn area_for_node_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<Option<String>, GraphStoreError> {
    let txn = db.begin_read()?;
    if let Some(node) = get_node_in_txn(&txn, id_or_path)? {
        return Ok(area_id_from_node(&node));
    }
    drop(txn);
    Ok(resolve_file_path_from(db, id_or_path)?.and_then(|file| file.area_id))
}

fn symbol_lookup_from_node(node: StoredNode) -> Option<SymbolLookup> {
    match node {
        StoredNode::Function(function) => Some(SymbolLookup {
            id: function.id.to_string(),
            kind: StoredNodeKind::Function,
            name: function.name.to_string(),
            path: function.file_path.to_string(),
            line: function.line,
            signature: function.signature.to_string(),
            language: function.language.to_string(),
            area_id: function.area_id.map(|id| id.to_string()),
        }),
        StoredNode::Class(class) => Some(SymbolLookup {
            id: class.id.to_string(),
            kind: StoredNodeKind::Class,
            name: class.name.to_string(),
            path: class.file_path.to_string(),
            line: class.line,
            signature: class.signature.to_string(),
            language: class.language.to_string(),
            area_id: class.area_id.map(|id| id.to_string()),
        }),
        StoredNode::Surface(surface) => Some(SymbolLookup {
            id: surface.id.to_string(),
            kind: stored_kind_from_surface_kind(surface.kind),
            name: surface.name.to_string(),
            path: surface.file_path.to_string(),
            line: surface.line,
            signature: surface.detail.to_string(),
            language: surface.language.to_string(),
            area_id: surface.area_id.map(|id| id.to_string()),
        }),
        _ => None,
    }
}

fn node_display_from_node(node: StoredNode) -> NodeDisplay {
    match node {
        StoredNode::Repository(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Repository,
            display: node.name.clone(),
            name: node.name,
            path: Some(node.root_path),
            language: None,
            area_id: None,
        },
        StoredNode::Directory(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Directory,
            display: node.path.clone(),
            name: node.name,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::File(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::File,
            display: node.path.clone(),
            name: node.name,
            path: Some(node.path),
            language: node.language,
            area_id: node.area_id,
        },
        StoredNode::Area(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Area,
            display: node.path_prefix.clone(),
            name: node.name,
            path: Some(node.path_prefix),
            language: None,
            area_id: None,
        },
        StoredNode::Function(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Function,
            display: node.qualified_name.to_string(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Class(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Class,
            display: node.qualified_name.to_string(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Doc(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Doc,
            display: node.path.clone(),
            name: node.title,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::Config(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Config,
            display: node.path.clone(),
            name: node.config_type,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::Surface(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: stored_kind_from_surface_kind(node.kind),
            display: node.display(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Unresolved(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Unresolved,
            display: format!("{}::{}", node.file_path, node.name),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
    }
}

fn display_from_id_in_txn(
    txn: &ReadTransaction,
    id: &str,
) -> Result<Option<NodeDisplay>, GraphStoreError> {
    Ok(get_node_in_txn(txn, id)?.map(node_display_from_node))
}

fn find_symbols_from<D: ReadableDatabase>(
    db: &D,
    name: &str,
    kind: Option<StoredNodeKind>,
) -> Result<Vec<SymbolLookup>, GraphStoreError> {
    if matches!(
        kind,
        Some(
            StoredNodeKind::File
                | StoredNodeKind::Repository
                | StoredNodeKind::Directory
                | StoredNodeKind::Area
                | StoredNodeKind::Doc
                | StoredNodeKind::Config
                | StoredNodeKind::Unresolved
        )
    ) {
        return Ok(Vec::new());
    }

    let key = symbol_index_key(name);
    let ids = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        let mut ids = BTreeSet::new();
        for row in t.get(key.as_str())? {
            ids.insert(row?.value().to_string());
        }
        ids
    };

    let txn = db.begin_read()?;
    let mut out = Vec::new();
    for id in ids {
        let Some(node) = get_node_in_txn(&txn, &id)? else {
            continue;
        };
        if let Some(expected) = kind {
            if node.kind() != expected {
                continue;
            }
        }
        if let Some(symbol) = symbol_lookup_from_node(node) {
            out.push(symbol);
        }
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(out)
}

fn merge_symbol_candidate(
    candidates: &mut BTreeMap<String, SymbolCandidate>,
    symbol: SymbolLookup,
    signals: SymbolMatchSignals,
) {
    candidates
        .entry(symbol.id.clone())
        .and_modify(|existing| existing.signals.merge(signals))
        .or_insert(SymbolCandidate {
            symbol,
            signals,
            rank: 0,
        });
}

fn symbol_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for component in split_symbol_components(query) {
        let token = component.to_ascii_lowercase();
        if !token.is_empty() && !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn symbol_query_exact_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    let mut variants = Vec::new();
    push_unique_string(&mut variants, trimmed.to_string());

    let components = split_symbol_components(trimmed);
    if !components.is_empty() {
        push_unique_string(&mut variants, components.join("_"));
        push_unique_string(&mut variants, components.join(""));
        for component in components {
            push_unique_string(&mut variants, component);
        }
    }
    variants
}

fn symbol_query_index_variants(query: &str, tokens: &[String]) -> Vec<String> {
    let mut variants = Vec::new();
    push_unique_string(&mut variants, symbol_index_key(query.trim()));
    if !tokens.is_empty() {
        push_unique_string(&mut variants, tokens.join("_"));
        push_unique_string(&mut variants, tokens.join(""));
        for token in tokens {
            push_unique_string(&mut variants, token.clone());
        }
    }
    variants
}

fn shares_stem(left: &str, right: &str) -> bool {
    if left.len() < MIN_STEM_LEN || right.len() < MIN_STEM_LEN {
        return false;
    }
    let left_prefix = left.chars().take(MIN_STEM_LEN).collect::<String>();
    let right_prefix = right.chars().take(MIN_STEM_LEN).collect::<String>();
    left_prefix.eq_ignore_ascii_case(&right_prefix)
}

fn basename_without_extension(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn score_symbol_candidate(
    symbol: &SymbolLookup,
    query: &str,
    tokens: &[String],
    area_name: Option<&str>,
    mut signals: SymbolMatchSignals,
) -> (i32, SymbolMatchSignals) {
    let exact_variants = symbol_query_exact_variants(query);
    let index_variants = symbol_query_index_variants(query, tokens);
    let name_lower = symbol.name.to_ascii_lowercase();
    signals.exact |= exact_variants.iter().any(|variant| symbol.name == *variant);
    signals.case_insensitive |= index_variants.iter().any(|variant| name_lower == *variant);

    let component_lowers = split_symbol_components(symbol.name.as_str())
        .into_iter()
        .map(|component| component.to_ascii_lowercase())
        .collect::<Vec<_>>();

    let mut name_matched_tokens = Vec::new();
    for token in tokens {
        let component_match = component_lowers
            .iter()
            .any(|component| component == token || shares_stem(component, token));
        if component_match {
            signals.component = true;
        }
        if name_lower.starts_with(token)
            || component_lowers
                .iter()
                .any(|component| component.starts_with(token))
        {
            signals.prefix = true;
        }
        if name_lower == *token || name_lower.contains(token.as_str()) || component_match {
            if !name_matched_tokens.contains(token) {
                name_matched_tokens.push(token.clone());
            }
        }
    }

    let name_score = match name_matched_tokens.len() {
        0 => 0,
        matched => NAME_BASE + NAME_COMPOUND_PER_EXTRA * (matched as i32 - 1),
    };

    let path_lower = symbol.path.to_ascii_lowercase();
    let path_matched = tokens
        .iter()
        .filter(|token| token.len() >= MIN_STEM_LEN && path_lower.contains(token.as_str()))
        .count();
    if path_matched > 0 {
        signals.path = true;
    }
    let path_score = path_matched as i32 * PATH_PER_TOKEN;

    let area_matched = area_name
        .map(|area| {
            tokens
                .iter()
                .filter(|token| token.len() >= MIN_STEM_LEN && area.contains(token.as_str()))
                .count()
        })
        .unwrap_or(0);
    if area_matched > 0 {
        signals.area = true;
    }
    let area_score = area_matched as i32 * AREA_PER_TOKEN;

    let basename_lower = basename_without_extension(symbol.path.as_str());
    if !basename_lower.is_empty() && tokens.iter().any(|token| *token == basename_lower) {
        signals.basename = true;
    }
    let basename_score = if signals.basename {
        BASENAME_EXACT_BONUS
    } else {
        0
    };

    let rank = name_score + path_score + area_score + basename_score;
    let fallback_rank = if rank == 0 {
        i32::from(signals.signal_count()) * 10
    } else {
        0
    };
    (rank + fallback_rank, signals)
}

fn symbol_candidate_allowed(symbol: &SymbolLookup, options: &SymbolMatchOptions) -> bool {
    if let Some(kind) = options.kind {
        if symbol.kind != kind {
            return false;
        }
    }
    if let Some(prefix) = &options.path_prefix {
        if !symbol.path.starts_with(prefix) {
            return false;
        }
    }
    if let Some(area_id) = &options.area_id {
        if symbol.area_id.as_deref() != Some(area_id.as_str()) {
            return false;
        }
    }
    true
}

fn add_symbol_candidates_for_ids(
    txn: &ReadTransaction,
    ids: BTreeSet<String>,
    candidates: &mut BTreeMap<String, SymbolCandidate>,
    signals: SymbolMatchSignals,
    options: &SymbolMatchOptions,
) -> Result<(), GraphStoreError> {
    for id in ids {
        let Some(node) = get_node_in_txn(txn, &id)? else {
            continue;
        };
        let Some(symbol) = symbol_lookup_from_node(node) else {
            continue;
        };
        if symbol_candidate_allowed(&symbol, options) {
            merge_symbol_candidate(candidates, symbol, signals);
        }
        if candidates.len() >= options.limit.saturating_mul(4).max(options.limit) {
            break;
        }
    }
    Ok(())
}

fn collect_symbol_ids_for_exact_name<D: ReadableDatabase>(
    db: &D,
    key: &str,
) -> Result<BTreeSet<String>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
    let mut ids = BTreeSet::new();
    for row in t.get(key)? {
        ids.insert(row?.value().to_string());
    }
    Ok(ids)
}

fn collect_symbol_ids_for_name_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, mut values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        while let Some(value) = values.next() {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

fn collect_symbol_ids_for_component<D: ReadableDatabase>(
    db: &D,
    component: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
    let mut ids = BTreeSet::new();
    for row in t.get(component)? {
        ids.insert(row?.value().to_string());
        if ids.len() >= limit {
            break;
        }
    }
    Ok(ids)
}

fn collect_symbol_ids_for_component_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, mut values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        while let Some(value) = values.next() {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

fn collect_symbol_ids_for_path_component<D: ReadableDatabase>(
    db: &D,
    component: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
    let mut ids = BTreeSet::new();
    for row in t.get(component)? {
        ids.insert(row?.value().to_string());
        if ids.len() >= limit {
            break;
        }
    }
    Ok(ids)
}

fn collect_symbol_ids_for_path_component_prefix<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
    limit: usize,
) -> Result<BTreeSet<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(BTreeSet::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, mut values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        while let Some(value) = values.next() {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids);
            }
        }
    }
    Ok(ids)
}

fn stem_prefix(token: &str) -> Option<String> {
    if token.len() < MIN_STEM_LEN {
        return None;
    }
    Some(token.chars().take(MIN_STEM_LEN).collect())
}

fn symbols_matching_from<D: ReadableDatabase>(
    db: &D,
    query: &str,
    options: SymbolMatchOptions,
) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
    let trimmed = query.trim();
    let normalized = symbol_index_key(trimmed);
    let tokens = symbol_query_tokens(trimmed);
    let index_variants = symbol_query_index_variants(trimmed, &tokens);
    if normalized.is_empty() || options.limit == 0 {
        return Ok(Vec::new());
    }

    let mut candidates = BTreeMap::new();
    let txn = db.begin_read()?;

    for variant in &index_variants {
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_exact_name(db, variant)?,
            &mut candidates,
            SymbolMatchSignals {
                case_insensitive: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_name_prefix(db, variant, options.limit.saturating_mul(2))?,
            &mut candidates,
            SymbolMatchSignals {
                prefix: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
    }

    for token in &tokens {
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_component(db, token, options.limit.saturating_mul(2))?,
            &mut candidates,
            SymbolMatchSignals {
                component: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        if let Some(stem) = stem_prefix(token) {
            add_symbol_candidates_for_ids(
                &txn,
                collect_symbol_ids_for_component_prefix(
                    db,
                    &stem,
                    options.limit.saturating_mul(3),
                )?,
                &mut candidates,
                SymbolMatchSignals {
                    component: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
        add_symbol_candidates_for_ids(
            &txn,
            collect_symbol_ids_for_path_component(db, token, options.limit.saturating_mul(3))?,
            &mut candidates,
            SymbolMatchSignals {
                path: true,
                ..SymbolMatchSignals::default()
            },
            &options,
        )?;
        if let Some(stem) = stem_prefix(token) {
            add_symbol_candidates_for_ids(
                &txn,
                collect_symbol_ids_for_path_component_prefix(
                    db,
                    &stem,
                    options.limit.saturating_mul(3),
                )?,
                &mut candidates,
                SymbolMatchSignals {
                    path: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
    }

    let path_ids =
        ids_under_path_limited_from(db, NODES_BY_PATH, trimmed, options.limit.saturating_mul(3))?
            .into_iter()
            .collect::<BTreeSet<_>>();
    add_symbol_candidates_for_ids(
        &txn,
        path_ids,
        &mut candidates,
        SymbolMatchSignals {
            path: true,
            ..SymbolMatchSignals::default()
        },
        &options,
    )?;

    let areas = list_areas_from(db, None)?;
    for area in &areas {
        let area_name = symbol_index_key(&area.name);
        let area_path = symbol_index_key(&area.path_prefix);
        let area_matches = index_variants.iter().any(|variant| {
            area_name == *variant
                || area_name.starts_with(variant)
                || area_path == *variant
                || area_path.starts_with(variant)
                || area_path.contains(variant)
        }) || tokens.iter().any(|token| {
            token.len() >= MIN_STEM_LEN && (area_name.contains(token) || area_path.contains(token))
        }) || area.id.eq_ignore_ascii_case(trimmed);

        if area_matches {
            let area_ids = ids_under_path_limited_from(
                db,
                NODES_BY_PATH,
                &area.path_prefix,
                options.limit.saturating_mul(3),
            )?
            .into_iter()
            .collect::<BTreeSet<_>>();
            add_symbol_candidates_for_ids(
                &txn,
                area_ids,
                &mut candidates,
                SymbolMatchSignals {
                    area: true,
                    ..SymbolMatchSignals::default()
                },
                &options,
            )?;
        }
    }

    let area_names = areas
        .into_iter()
        .map(|area| (area.id, area.name.to_ascii_lowercase()))
        .collect::<BTreeMap<_, _>>();
    let mut out = candidates.into_values().collect::<Vec<_>>();
    for candidate in &mut out {
        let area_name = candidate
            .symbol
            .area_id
            .as_ref()
            .and_then(|area_id| area_names.get(area_id).map(String::as_str));
        let (rank, signals) = score_symbol_candidate(
            &candidate.symbol,
            trimmed,
            &tokens,
            area_name,
            candidate.signals,
        );
        candidate.rank = rank;
        candidate.signals = signals;
    }
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.signals.exact.cmp(&left.signals.exact))
            .then_with(|| {
                right
                    .signals
                    .case_insensitive
                    .cmp(&left.signals.case_insensitive)
            })
            .then_with(|| right.signals.prefix.cmp(&left.signals.prefix))
            .then_with(|| right.signals.component.cmp(&left.signals.component))
            .then_with(|| right.signals.path.cmp(&left.signals.path))
            .then_with(|| right.signals.area.cmp(&left.signals.area))
            .then_with(|| right.signals.basename.cmp(&left.signals.basename))
            .then_with(|| {
                right
                    .signals
                    .signal_count()
                    .cmp(&left.signals.signal_count())
            })
            .then_with(|| left.symbol.path.cmp(&right.symbol.path))
            .then_with(|| left.symbol.line.cmp(&right.symbol.line))
            .then_with(|| left.symbol.name.cmp(&right.symbol.name))
            .then_with(|| left.symbol.id.cmp(&right.symbol.id))
    });
    out.truncate(options.limit);
    Ok(out)
}

fn merge_task_anchor_candidate(
    candidates: &mut BTreeMap<String, TaskAnchorCandidate>,
    node: NodeDisplay,
    token: &str,
    signals: SymbolMatchSignals,
) {
    candidates
        .entry(node.id.clone())
        .and_modify(|existing| {
            existing.signals.merge(signals);
            if !existing.matched_tokens.iter().any(|value| value == token) {
                existing.matched_tokens.push(token.to_string());
                existing.matched_tokens.sort();
            }
        })
        .or_insert_with(|| TaskAnchorCandidate {
            node,
            signals,
            matched_tokens: vec![token.to_string()],
        });
}

fn task_anchor_candidates_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    task_tokens: &[S],
    limit: usize,
) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut candidates = BTreeMap::new();
    for token in task_tokens {
        let token = token.as_ref().trim();
        if token.len() < 2 {
            continue;
        }
        for symbol in symbols_matching_from(
            db,
            token,
            SymbolMatchOptions {
                limit,
                ..SymbolMatchOptions::default()
            },
        )? {
            let node = NodeDisplay {
                id: symbol.symbol.id.clone(),
                kind: symbol.symbol.kind,
                display: format!("{}::{}", symbol.symbol.path, symbol.symbol.name),
                name: symbol.symbol.name,
                path: Some(symbol.symbol.path),
                language: Some(symbol.symbol.language),
                area_id: symbol.symbol.area_id,
            };
            merge_task_anchor_candidate(&mut candidates, node, token, symbol.signals);
        }

        let path_ids = ids_under_path_limited_from(db, NODES_BY_PATH, token, limit)?;
        let txn = db.begin_read()?;
        for id in path_ids {
            let Some(node) = display_from_id_in_txn(&txn, &id)? else {
                continue;
            };
            merge_task_anchor_candidate(
                &mut candidates,
                node,
                token,
                SymbolMatchSignals {
                    path: true,
                    ..SymbolMatchSignals::default()
                },
            );
        }

        for area in list_areas_from(db, None)? {
            if area.name.eq_ignore_ascii_case(token)
                || area.path_prefix.eq_ignore_ascii_case(token)
                || area.path_prefix.starts_with(token)
            {
                merge_task_anchor_candidate(
                    &mut candidates,
                    NodeDisplay {
                        id: area.id.clone(),
                        kind: StoredNodeKind::Area,
                        display: area.path_prefix.clone(),
                        name: area.name.clone(),
                        path: Some(area.path_prefix.clone()),
                        language: None,
                        area_id: Some(area.id),
                    },
                    token,
                    SymbolMatchSignals {
                        area: true,
                        ..SymbolMatchSignals::default()
                    },
                );
            }
        }
    }

    let mut out = candidates.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .matched_tokens
            .len()
            .cmp(&left.matched_tokens.len())
            .then_with(|| {
                right
                    .signals
                    .signal_count()
                    .cmp(&left.signals.signal_count())
            })
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn usage_boundary_candidates_from<D: ReadableDatabase>(
    db: &D,
    scope: &str,
    symbol_kind: Option<StoredNodeKind>,
    limit: usize,
) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let ids = ids_under_path_limited_from(db, NODES_BY_PATH, scope, limit.saturating_mul(4))?;
    let txn = db.begin_read()?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for id in ids {
        let Some(node) = get_node_in_txn(&txn, &id)? else {
            continue;
        };
        let kind = node.kind();
        let include = match symbol_kind {
            Some(expected) => kind == expected,
            None => {
                matches!(
                    kind,
                    StoredNodeKind::Function | StoredNodeKind::Class | StoredNodeKind::File
                ) || is_surface_stored_kind(kind)
            }
        };
        if !include {
            continue;
        }
        let symbol = symbol_lookup_from_node(node.clone());
        let display = node_display_from_node(node);
        if seen.insert(display.id.clone()) {
            out.push(UsageBoundaryCandidate {
                node: display,
                symbol,
            });
        }
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by(|left, right| {
        left.node
            .path
            .cmp(&right.node.path)
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    Ok(out)
}

fn prefix_end(prefix: &str) -> String {
    format!("{prefix}\u{10ffff}")
}

fn ids_under_path_from<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &str>,
    prefix: &str,
) -> Result<Vec<String>, GraphStoreError> {
    ids_under_path_limited_from(db, table, prefix, usize::MAX)
}

fn ids_under_path_limited_from<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &str>,
    prefix: &str,
    limit: usize,
) -> Result<Vec<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(table)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, mut values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        while let Some(value) = values.next() {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids.into_iter().collect());
            }
        }
    }
    Ok(ids.into_iter().collect())
}

fn sort_nodes(nodes: &mut [StoredNode]) {
    nodes.sort_by(|left, right| {
        left.path()
            .unwrap_or("")
            .cmp(right.path().unwrap_or(""))
            .then_with(|| left.kind().cmp(&right.kind()))
            .then_with(|| left.id().cmp(right.id()))
    });
}

fn nodes_under_path_from<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
) -> Result<Vec<StoredNode>, GraphStoreError> {
    let ids = ids_under_path_from(db, NODES_BY_PATH, prefix)?;
    let txn = db.begin_read()?;
    let mut nodes = Vec::new();
    for id in ids {
        if let Some(node) = get_node_in_txn(&txn, &id)? {
            nodes.push(node);
        }
    }
    sort_nodes(&mut nodes);
    Ok(nodes)
}

fn functions_under_path_from<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
) -> Result<Vec<FunctionNode>, GraphStoreError> {
    let ids = ids_under_path_from(db, FUNCTIONS_BY_PATH, prefix)?;
    let txn = db.begin_read()?;
    let mut functions = Vec::new();
    for id in ids {
        if let Some(function) = read_table_node::<FunctionNode>(&txn, FUNCTIONS, &id)? {
            functions.push(function);
        }
    }
    functions.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(functions)
}

fn function_ids_for_path_from<D: ReadableDatabase>(
    db: &D,
    path: &str,
    limit: usize,
) -> Result<BoundedFunctionIds, GraphStoreError> {
    let txn = db.begin_read()?;
    let table = txn.open_multimap_table(FUNCTIONS_BY_PATH)?;
    let mut ids = Vec::new();
    for row in table.get(path)? {
        if ids.len() == limit {
            return Ok(BoundedFunctionIds {
                ids,
                truncated: true,
            });
        }
        ids.push(row?.value().to_string());
    }
    Ok(BoundedFunctionIds {
        ids,
        truncated: false,
    })
}

fn resolve_file_path_from<D: ReadableDatabase>(
    db: &D,
    path: &str,
) -> Result<Option<FileNode>, GraphStoreError> {
    let ids = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(NODES_BY_PATH)?;
        let mut ids = Vec::new();
        for row in t.get(path)? {
            ids.push(row?.value().to_string());
        }
        ids
    };
    let txn = db.begin_read()?;
    for id in ids {
        if let Some(file) = read_table_node::<FileNode>(&txn, FILES, &id)? {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

fn neighbors_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    direction: NeighborDirection,
    kind: Option<EdgeKind>,
) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
    let table = match direction {
        NeighborDirection::Outgoing => EDGES_OUT,
        NeighborDirection::Incoming => EDGES_IN,
    };
    let mut rows = collect_adjacency(db, table, id)?;
    if let Some(expected) = kind {
        rows.retain(|row| row.kind == expected);
    }
    rows.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.other.cmp(&right.other))
            .then_with(|| left.source.cmp(&right.source))
    });
    Ok(rows)
}

fn edge_kind_label(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::BelongsTo => "belongs_to",
        EdgeKind::Defines => "defines",
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::Documents => "documents",
        EdgeKind::Configures => "configures",
        EdgeKind::EntrypointFor => "entrypoint_for",
        EdgeKind::Authorizes => "authorizes",
        EdgeKind::Exposes => "exposes",
        EdgeKind::ForwardsTo => "forwards_to",
        EdgeKind::InstallsMiddleware => "installs_middleware",
        EdgeKind::IssuesCredential => "issues_credential",
        EdgeKind::StoresCredential => "stores_credential",
        EdgeKind::UsesCredential => "uses_credential",
        EdgeKind::ValidatesCredential => "validates_credential",
        EdgeKind::RewritesHeader => "rewrites_header",
        EdgeKind::TestedBy => "tested_by",
    }
}

fn edges_by_kind_limited_from<D: ReadableDatabase>(
    db: &D,
    kind: EdgeKind,
    limit: usize,
) -> Result<Vec<Edge>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_BY_KIND)?;
    let mut edges = Vec::new();
    for row in t.get(edge_kind_label(&kind))? {
        let edge: Edge = bincode::deserialize(row?.value())?;
        edges.push(edge);
        if edges.len() >= limit {
            break;
        }
    }
    edges.sort();
    edges.dedup();
    edges.truncate(limit);
    Ok(edges)
}

fn relation_specs(relation: GraphRelation) -> Vec<(NeighborDirection, Vec<EdgeKind>)> {
    match relation {
        GraphRelation::Children => vec![(
            NeighborDirection::Outgoing,
            vec![
                EdgeKind::Contains,
                EdgeKind::Defines,
                EdgeKind::Authorizes,
                EdgeKind::Exposes,
                EdgeKind::ForwardsTo,
                EdgeKind::InstallsMiddleware,
                EdgeKind::IssuesCredential,
                EdgeKind::StoresCredential,
                EdgeKind::UsesCredential,
                EdgeKind::ValidatesCredential,
                EdgeKind::RewritesHeader,
                EdgeKind::TestedBy,
            ],
        )],
        GraphRelation::Parents => vec![(
            NeighborDirection::Incoming,
            vec![
                EdgeKind::Contains,
                EdgeKind::Defines,
                EdgeKind::BelongsTo,
                EdgeKind::Authorizes,
                EdgeKind::Exposes,
                EdgeKind::ForwardsTo,
                EdgeKind::InstallsMiddleware,
                EdgeKind::IssuesCredential,
                EdgeKind::StoresCredential,
                EdgeKind::UsesCredential,
                EdgeKind::ValidatesCredential,
                EdgeKind::RewritesHeader,
                EdgeKind::TestedBy,
            ],
        )],
        GraphRelation::Callers => vec![(NeighborDirection::Incoming, vec![EdgeKind::Calls])],
        GraphRelation::Callees => vec![(NeighborDirection::Outgoing, vec![EdgeKind::Calls])],
        GraphRelation::Docs => vec![
            (NeighborDirection::Outgoing, vec![EdgeKind::Documents]),
            (NeighborDirection::Incoming, vec![EdgeKind::Documents]),
        ],
        GraphRelation::Configs => vec![
            (
                NeighborDirection::Outgoing,
                vec![EdgeKind::Configures, EdgeKind::EntrypointFor],
            ),
            (
                NeighborDirection::Incoming,
                vec![EdgeKind::Configures, EdgeKind::EntrypointFor],
            ),
        ],
        GraphRelation::Imports => vec![(NeighborDirection::Outgoing, vec![EdgeKind::Imports])],
        GraphRelation::Importers => vec![(NeighborDirection::Incoming, vec![EdgeKind::Imports])],
        GraphRelation::References => vec![
            (
                NeighborDirection::Outgoing,
                vec![
                    EdgeKind::References,
                    EdgeKind::Authorizes,
                    EdgeKind::Exposes,
                    EdgeKind::ForwardsTo,
                    EdgeKind::InstallsMiddleware,
                    EdgeKind::IssuesCredential,
                    EdgeKind::RewritesHeader,
                    EdgeKind::StoresCredential,
                    EdgeKind::TestedBy,
                    EdgeKind::UsesCredential,
                    EdgeKind::ValidatesCredential,
                ],
            ),
            (
                NeighborDirection::Incoming,
                vec![
                    EdgeKind::References,
                    EdgeKind::Authorizes,
                    EdgeKind::Exposes,
                    EdgeKind::ForwardsTo,
                    EdgeKind::InstallsMiddleware,
                    EdgeKind::IssuesCredential,
                    EdgeKind::RewritesHeader,
                    EdgeKind::StoresCredential,
                    EdgeKind::TestedBy,
                    EdgeKind::UsesCredential,
                    EdgeKind::ValidatesCredential,
                ],
            ),
        ],
    }
}

fn relation_items_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    relation: GraphRelation,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<RedbRelationItem>, GraphStoreError> {
    let mut adjacency = Vec::new();
    for (direction, kinds) in relation_specs(relation) {
        for kind in kinds {
            adjacency.extend(neighbors_from(db, id, direction, Some(kind))?);
        }
    }
    let txn = db.begin_read()?;
    let mut items = Vec::new();
    for edge in adjacency {
        let Some(node) = display_from_id_in_txn(&txn, edge.other.as_str())? else {
            continue;
        };
        if let Some(expected) = kind_filter {
            if node.kind != expected {
                continue;
            }
        }
        items.push(RedbRelationItem {
            relation: edge_kind_label(&edge.kind).to_string(),
            edge_kind: edge.kind,
            confidence: edge.confidence,
            source: edge.source.to_string(),
            node,
        });
    }
    items.sort_by(|left, right| {
        left.node
            .display
            .cmp(&right.node.display)
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.id.cmp(&right.node.id))
            .then_with(|| left.edge_kind.cmp(&right.edge_kind))
    });
    items.dedup();
    Ok(items)
}

fn relation_view_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    relation: GraphRelation,
) -> Result<RedbRelationView, GraphStoreError> {
    Ok(RedbRelationView {
        target: node_display_from(db, id)?,
        relation,
        items: relation_items_from(db, id, relation, None)?,
    })
}

fn children_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    Ok(
        relation_items_from(db, id, GraphRelation::Children, kind_filter)?
            .into_iter()
            .map(|item| item.node)
            .collect(),
    )
}

fn parents_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    Ok(
        relation_items_from(db, id, GraphRelation::Parents, kind_filter)?
            .into_iter()
            .map(|item| item.node)
            .collect(),
    )
}

fn docs_for_from<D: ReadableDatabase>(db: &D, id: &str) -> Result<Vec<DocNode>, GraphStoreError> {
    let ids = relation_items_from(db, id, GraphRelation::Docs, Some(StoredNodeKind::Doc))?
        .into_iter()
        .map(|item| item.node.id)
        .collect::<Vec<_>>();
    let txn = db.begin_read()?;
    let mut docs = Vec::new();
    for id in ids {
        if let Some(doc) = read_table_node::<DocNode>(&txn, DOCS, &id)? {
            docs.push(doc);
        }
    }
    docs.sort();
    docs.dedup();
    Ok(docs)
}

fn configs_for_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Vec<ConfigNode>, GraphStoreError> {
    let ids = relation_items_from(db, id, GraphRelation::Configs, Some(StoredNodeKind::Config))?
        .into_iter()
        .map(|item| item.node.id)
        .collect::<Vec<_>>();
    let txn = db.begin_read()?;
    let mut configs = Vec::new();
    for id in ids {
        if let Some(config) = read_table_node::<ConfigNode>(&txn, CONFIGS, &id)? {
            configs.push(config);
        }
    }
    configs.sort();
    configs.dedup();
    Ok(configs)
}

const FLOW_QUERY_LIMIT: usize = 50;
const FLOW_EDGE_LOOKUP_LIMIT: usize = 128;
const FLOW_CHAIN_ROOT_LIMIT: usize = 8;
const FLOW_CHAIN_STEP_LIMIT: usize = 32;
const FLOW_CHAIN_DEPTH_LIMIT: usize = 4;
const SUBSYSTEM_NODE_LIMIT: usize = 6;

fn push_unique_edge_kind(values: &mut Vec<EdgeKind>, value: EdgeKind) {
    if !values.contains(&value) {
        values.push(value);
        values.sort();
    }
}

fn bounded_query_terms<S: AsRef<str>>(tokens: &[S]) -> Vec<String> {
    let mut terms = Vec::new();
    for raw in tokens {
        let trimmed = raw.as_ref().trim();
        if trimmed.len() >= 2 {
            push_unique_string(&mut terms, trimmed.to_string());
        }
        for token in symbol_query_tokens(trimmed) {
            if token.len() >= 2 {
                push_unique_string(&mut terms, token);
            }
        }
    }
    terms
}

fn node_text_matches_term(node: &NodeDisplay, term: &str) -> bool {
    let needle = symbol_index_key(term);
    if needle.is_empty() {
        return false;
    }
    let mut haystack = format!("{} {}", node.display, node.name).to_ascii_lowercase();
    if let Some(path) = &node.path {
        haystack.push(' ');
        haystack.push_str(&path.to_ascii_lowercase());
    }
    haystack.contains(needle.as_str())
}

fn generic_entrypoint_term(term: &str) -> bool {
    matches!(
        symbol_index_key(term).as_str(),
        "entrypoint"
            | "entrypoints"
            | "route"
            | "routes"
            | "endpoint"
            | "endpoints"
            | "handler"
            | "handlers"
            | "worker"
            | "workers"
            | "proxy"
            | "proxies"
            | "webhook"
            | "webhooks"
            | "cli"
            | "job"
            | "jobs"
            | "ingress"
            | "surface"
            | "surfaces"
    )
}

fn generic_surface_term(term: &str) -> bool {
    generic_entrypoint_term(term)
        || matches!(
            symbol_index_key(term).as_str(),
            "middleware"
                | "middlewares"
                | "credential"
                | "credentials"
                | "auth"
                | "authorization"
                | "test"
                | "tests"
                | "behavior"
                | "queue"
                | "queues"
        )
}

fn generic_credential_term(term: &str) -> bool {
    matches!(
        symbol_index_key(term).as_str(),
        "auth"
            | "authorize"
            | "authorization"
            | "credential"
            | "credentials"
            | "token"
            | "tokens"
            | "jwt"
            | "apikey"
            | "api"
            | "key"
            | "keys"
            | "session"
            | "sessions"
            | "middleware"
            | "route"
            | "routes"
    )
}

fn is_ingress_candidate_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::File
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WorkerSurface
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::CliSurface
            | StoredNodeKind::JobSurface
            | StoredNodeKind::QueueSurface
    )
}

fn is_credential_candidate_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::File
            | StoredNodeKind::Function
            | StoredNodeKind::Class
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WorkerSurface
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::MiddlewareInstallation
            | StoredNodeKind::CredentialOperation
    )
}

fn is_surface_or_file_kind(kind: StoredNodeKind) -> bool {
    kind == StoredNodeKind::File || is_surface_stored_kind(kind)
}

fn surface_flow_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::Exposes,
        EdgeKind::ForwardsTo,
        EdgeKind::InstallsMiddleware,
        EdgeKind::IssuesCredential,
        EdgeKind::RewritesHeader,
        EdgeKind::StoresCredential,
        EdgeKind::TestedBy,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

fn credential_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::IssuesCredential,
        EdgeKind::RewritesHeader,
        EdgeKind::StoresCredential,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

fn middleware_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::InstallsMiddleware,
        EdgeKind::RewritesHeader,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

fn merge_surface_flow_candidate(
    candidates: &mut BTreeMap<String, SurfaceFlowCandidate>,
    node: NodeDisplay,
    token: &str,
    signals: SymbolMatchSignals,
    relation_kind: Option<EdgeKind>,
    rank: i32,
) {
    candidates
        .entry(node.id.clone())
        .and_modify(|existing| {
            existing.signals.merge(signals);
            if !existing.matched_tokens.iter().any(|value| value == token) {
                existing.matched_tokens.push(token.to_string());
                existing.matched_tokens.sort();
            }
            if let Some(kind) = relation_kind.clone() {
                push_unique_edge_kind(&mut existing.relation_kinds, kind);
            }
            existing.rank = existing.rank.max(rank);
        })
        .or_insert_with(|| {
            let mut relation_kinds = Vec::new();
            if let Some(kind) = relation_kind {
                push_unique_edge_kind(&mut relation_kinds, kind);
            }
            SurfaceFlowCandidate {
                node,
                signals,
                matched_tokens: vec![token.to_string()],
                relation_kinds,
                rank,
            }
        });
}

fn node_display_from_symbol(symbol: &SymbolLookup) -> NodeDisplay {
    NodeDisplay {
        id: symbol.id.clone(),
        kind: symbol.kind,
        display: format!("{}::{}", symbol.path, symbol.name),
        name: symbol.name.clone(),
        path: Some(symbol.path.clone()),
        language: Some(symbol.language.clone()),
        area_id: symbol.area_id.clone(),
    }
}

fn relation_kinds_for_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    allowed: &[EdgeKind],
) -> Result<Vec<EdgeKind>, GraphStoreError> {
    let mut out = Vec::new();
    for direction in [NeighborDirection::Outgoing, NeighborDirection::Incoming] {
        for edge in neighbors_from(db, id, direction, None)? {
            if allowed.contains(&edge.kind) {
                push_unique_edge_kind(&mut out, edge.kind);
            }
        }
    }
    Ok(out)
}

fn surface_flow_candidates_from<D, S, F, G>(
    db: &D,
    tokens: &[S],
    allowed_kind: F,
    relation_kinds: &[EdgeKind],
    generic_term: G,
    limit: usize,
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError>
where
    D: ReadableDatabase,
    S: AsRef<str>,
    F: Fn(StoredNodeKind) -> bool,
    G: Fn(&str) -> bool,
{
    if limit == 0 {
        return Ok(Vec::new());
    }
    let terms = bounded_query_terms(tokens);
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = BTreeMap::new();
    for term in &terms {
        for symbol in symbols_matching_from(
            db,
            term,
            SymbolMatchOptions {
                limit: FLOW_QUERY_LIMIT,
                ..SymbolMatchOptions::default()
            },
        )? {
            if !allowed_kind(symbol.symbol.kind) {
                continue;
            }
            merge_surface_flow_candidate(
                &mut candidates,
                node_display_from_symbol(&symbol.symbol),
                term,
                symbol.signals,
                None,
                symbol.rank,
            );
        }
    }

    let txn = db.begin_read()?;
    for kind in relation_kinds {
        for edge in edges_by_kind_limited_from(db, kind.clone(), FLOW_EDGE_LOOKUP_LIMIT)? {
            for endpoint in [edge.from.as_str(), edge.to.as_str()] {
                let Some(node) = display_from_id_in_txn(&txn, endpoint)? else {
                    continue;
                };
                if !allowed_kind(node.kind) {
                    continue;
                }
                let Some(term) = terms
                    .iter()
                    .find(|term| generic_term(term) || node_text_matches_term(&node, term))
                else {
                    continue;
                };
                merge_surface_flow_candidate(
                    &mut candidates,
                    node,
                    term,
                    SymbolMatchSignals {
                        path: true,
                        ..SymbolMatchSignals::default()
                    },
                    Some(kind.clone()),
                    80,
                );
            }
        }
    }
    drop(txn);

    let relation_family = surface_flow_edge_kinds();
    for candidate in candidates.values_mut() {
        for kind in relation_kinds_for_node_from(db, &candidate.node.id, &relation_family)? {
            push_unique_edge_kind(&mut candidate.relation_kinds, kind);
        }
        candidate.rank += candidate.matched_tokens.len() as i32 * 25;
        candidate.rank += candidate.relation_kinds.len() as i32 * 20;
        if is_surface_stored_kind(candidate.node.kind) {
            candidate.rank += 15;
        }
    }

    let mut out = candidates.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| right.relation_kinds.len().cmp(&left.relation_kinds.len()))
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn entrypoints_for_task_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
    surface_flow_candidates_from(
        db,
        tokens,
        is_ingress_candidate_kind,
        &[
            EdgeKind::EntrypointFor,
            EdgeKind::Exposes,
            EdgeKind::ForwardsTo,
        ],
        generic_entrypoint_term,
        FLOW_QUERY_LIMIT,
    )
}

fn surface_paths_for_behavior_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
    let candidates = surface_flow_candidates_from(
        db,
        tokens,
        is_surface_stored_kind,
        &surface_flow_edge_kinds(),
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )?;
    let mut paths: BTreeMap<String, SurfacePathCandidate> = BTreeMap::new();
    for candidate in candidates {
        let Some(path) = candidate.node.path.clone() else {
            continue;
        };
        paths
            .entry(path.clone())
            .and_modify(|existing| {
                existing.rank += candidate.rank;
                for token in &candidate.matched_tokens {
                    push_unique_string(&mut existing.matched_tokens, token.clone());
                }
                for kind in &candidate.relation_kinds {
                    push_unique_edge_kind(&mut existing.relation_kinds, kind.clone());
                }
                if existing.surfaces.len() < SUBSYSTEM_NODE_LIMIT
                    && !existing
                        .surfaces
                        .iter()
                        .any(|surface| surface.id == candidate.node.id)
                {
                    existing.surfaces.push(candidate.node.clone());
                }
            })
            .or_insert_with(|| SurfacePathCandidate {
                path,
                surfaces: vec![candidate.node],
                matched_tokens: candidate.matched_tokens,
                relation_kinds: candidate.relation_kinds,
                rank: candidate.rank,
            });
    }
    let mut out = paths.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

fn credential_flow_candidates_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
    let credential_kinds = credential_edge_kinds();
    let mut candidates = surface_flow_candidates_from(
        db,
        tokens,
        is_credential_candidate_kind,
        &credential_kinds,
        generic_credential_term,
        FLOW_QUERY_LIMIT,
    )?;
    candidates.retain(|candidate| {
        candidate.node.kind == StoredNodeKind::CredentialOperation
            || candidate
                .relation_kinds
                .iter()
                .any(|kind| credential_kinds.contains(kind))
            || candidate
                .matched_tokens
                .iter()
                .any(|token| generic_credential_term(token))
    });
    Ok(candidates)
}

fn resolve_query_nodes_from<D: ReadableDatabase>(
    db: &D,
    query: &str,
    limit: usize,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = BTreeMap::new();
    if let Some(node) = node_display_from(db, trimmed)? {
        out.insert(node.id.clone(), node);
    }
    if let Some(file) = resolve_file_path_from(db, trimmed)? {
        let node = node_display_from_node(StoredNode::File(file));
        out.insert(node.id.clone(), node);
    }
    for symbol in symbols_matching_from(
        db,
        trimmed,
        SymbolMatchOptions {
            limit,
            ..SymbolMatchOptions::default()
        },
    )? {
        let node = node_display_from_symbol(&symbol.symbol);
        out.insert(node.id.clone(), node);
        if out.len() >= limit {
            break;
        }
    }
    if trimmed.contains('/') {
        let ids = ids_under_path_limited_from(db, NODES_BY_PATH, trimmed, limit)?;
        let txn = db.begin_read()?;
        for id in ids {
            if let Some(node) = display_from_id_in_txn(&txn, &id)? {
                out.insert(node.id.clone(), node);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    let mut nodes = out.into_values().collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });
    nodes.truncate(limit);
    Ok(nodes)
}

fn relation_steps_for_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    directions: &[NeighborDirection],
    kinds: &[EdgeKind],
    limit: usize,
) -> Result<Vec<FlowRelationStep>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut steps = BTreeMap::new();
    for direction in directions {
        for edge in neighbors_from(db, id, *direction, None)? {
            if !kinds.contains(&edge.kind) {
                continue;
            }
            let (from_id, to_id) = match direction {
                NeighborDirection::Outgoing => (id, edge.other.as_str()),
                NeighborDirection::Incoming => (edge.other.as_str(), id),
            };
            let Some(from) = node_display_from(db, from_id)? else {
                continue;
            };
            let Some(to) = node_display_from(db, to_id)? else {
                continue;
            };
            steps.insert(
                (from.id.clone(), to.id.clone(), edge.kind.clone()),
                FlowRelationStep {
                    from,
                    to,
                    edge_kind: edge.kind,
                    confidence: edge.confidence,
                    source: edge.source.to_string(),
                },
            );
            if steps.len() >= limit {
                break;
            }
        }
        if steps.len() >= limit {
            break;
        }
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn insert_chain_step(
    steps: &mut BTreeMap<(String, String, EdgeKind), FlowRelationStep>,
    step: FlowRelationStep,
) {
    steps
        .entry((
            step.from.id.clone(),
            step.to.id.clone(),
            step.edge_kind.clone(),
        ))
        .or_insert(step);
}

fn middleware_chain_for_route_from<D: ReadableDatabase>(
    db: &D,
    route_or_file: &str,
) -> Result<FlowChain, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, route_or_file, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut steps = BTreeMap::new();
    let middleware_kinds = middleware_edge_kinds();
    for root in &roots {
        for step in relation_steps_for_node_from(
            db,
            &root.id,
            &[NeighborDirection::Outgoing, NeighborDirection::Incoming],
            &middleware_kinds,
            FLOW_CHAIN_STEP_LIMIT,
        )? {
            insert_chain_step(&mut steps, step);
        }

        for edge in neighbors_from(
            db,
            &root.id,
            NeighborDirection::Incoming,
            Some(EdgeKind::Exposes),
        )? {
            for step in relation_steps_for_node_from(
                db,
                edge.other.as_str(),
                &[NeighborDirection::Outgoing],
                &middleware_kinds,
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                insert_chain_step(&mut steps, step);
            }
        }

        if root.kind != StoredNodeKind::File {
            if let Some(path) = &root.path {
                if let Some(file) = resolve_file_path_from(db, path)? {
                    for step in relation_steps_for_node_from(
                        db,
                        &file.id,
                        &[NeighborDirection::Outgoing],
                        &middleware_kinds,
                        FLOW_CHAIN_STEP_LIMIT,
                    )? {
                        insert_chain_step(&mut steps, step);
                    }
                }
            }
        }
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(FLOW_CHAIN_STEP_LIMIT);
    Ok(FlowChain { roots, steps: out })
}

fn forwarding_chain_for_surface_from<D: ReadableDatabase>(
    db: &D,
    surface: &str,
) -> Result<FlowChain, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, surface, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut steps = BTreeMap::new();
    let mut seen_nodes = BTreeSet::new();
    let mut frontier = roots.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let forwarding_kinds = vec![EdgeKind::ForwardsTo, EdgeKind::RewritesHeader];
    for _ in 0..FLOW_CHAIN_DEPTH_LIMIT {
        let mut next = Vec::new();
        for id in frontier {
            if !seen_nodes.insert(id.clone()) {
                continue;
            }
            for step in relation_steps_for_node_from(
                db,
                &id,
                &[NeighborDirection::Outgoing],
                &forwarding_kinds,
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                if step.edge_kind == EdgeKind::ForwardsTo && !seen_nodes.contains(&step.to.id) {
                    next.push(step.to.id.clone());
                }
                insert_chain_step(&mut steps, step);
                if steps.len() >= FLOW_CHAIN_STEP_LIMIT {
                    break;
                }
            }
            if steps.len() >= FLOW_CHAIN_STEP_LIMIT {
                break;
            }
        }
        if next.is_empty() || steps.len() >= FLOW_CHAIN_STEP_LIMIT {
            break;
        }
        next.sort();
        next.dedup();
        frontier = next;
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(FLOW_CHAIN_STEP_LIMIT);
    Ok(FlowChain { roots, steps: out })
}

fn tests_for_surface_or_symbol_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, id, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut tests = BTreeMap::new();
    for root in &roots {
        let mut seed_ids = vec![root.id.clone()];
        if let Some(path) = &root.path {
            if let Some(file) = resolve_file_path_from(db, path)? {
                push_unique_string(&mut seed_ids, file.id);
            }
        }
        for seed_id in seed_ids {
            for step in relation_steps_for_node_from(
                db,
                &seed_id,
                &[NeighborDirection::Outgoing, NeighborDirection::Incoming],
                &[EdgeKind::TestedBy],
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                for node in [step.from, step.to] {
                    if node.kind == StoredNodeKind::BehaviorTestSurface {
                        tests.insert(node.id.clone(), node);
                    }
                }
            }
        }
    }
    let mut out = tests.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

fn behavior_tests_for_task_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    let candidates = surface_flow_candidates_from(
        db,
        tokens,
        |kind| kind == StoredNodeKind::BehaviorTestSurface,
        &[EdgeKind::TestedBy, EdgeKind::ValidatesCredential],
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )?;
    let mut tests = BTreeMap::new();
    for candidate in candidates {
        tests.insert(candidate.node.id.clone(), candidate.node);
    }
    let mut out = tests.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

fn subsystem_path_for_node<D: ReadableDatabase>(
    db: &D,
    node: &NodeDisplay,
) -> Result<(Option<String>, String), GraphStoreError> {
    if let Some(area_id) = &node.area_id {
        let txn = db.begin_read()?;
        if let Some(area) = read_table_node::<AreaNode>(&txn, AREAS, area_id)? {
            return Ok((Some(area.id), area.path_prefix));
        }
    }
    let path = node.path.as_deref().unwrap_or(node.display.as_str());
    let prefix = path
        .split('/')
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
        .trim_matches('/')
        .to_string();
    Ok((node.area_id.clone(), prefix))
}

fn merge_subsystem_candidate<D: ReadableDatabase>(
    db: &D,
    groups: &mut BTreeMap<String, SubsystemCandidate>,
    node: NodeDisplay,
    matched_tokens: &[String],
    rank: i32,
) -> Result<(), GraphStoreError> {
    let (id, path_prefix) = subsystem_path_for_node(db, &node)?;
    if path_prefix.is_empty() {
        return Ok(());
    }
    groups
        .entry(path_prefix.clone())
        .and_modify(|existing| {
            existing.rank += rank;
            for token in matched_tokens {
                push_unique_string(&mut existing.matched_tokens, token.clone());
            }
            if existing.nodes.len() < SUBSYSTEM_NODE_LIMIT
                && !existing
                    .nodes
                    .iter()
                    .any(|candidate| candidate.id == node.id)
            {
                existing.nodes.push(node.clone());
            }
        })
        .or_insert_with(|| SubsystemCandidate {
            id,
            path_prefix,
            matched_tokens: matched_tokens.to_vec(),
            nodes: vec![node],
            rank,
        });
    Ok(())
}

fn subsystems_matching_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
    let terms = bounded_query_terms(tokens);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups = BTreeMap::new();
    for candidate in surface_flow_candidates_from(
        db,
        tokens,
        is_surface_or_file_kind,
        &surface_flow_edge_kinds(),
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )? {
        merge_subsystem_candidate(
            db,
            &mut groups,
            candidate.node,
            &candidate.matched_tokens,
            candidate.rank,
        )?;
    }
    let anchors = task_anchor_candidates_from(db, &terms, FLOW_QUERY_LIMIT)?;
    for anchor in anchors {
        merge_subsystem_candidate(
            db,
            &mut groups,
            anchor.node,
            &anchor.matched_tokens,
            anchor.signals.signal_count() as i32 * 30,
        )?;
    }
    let mut out = groups.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| left.path_prefix.cmp(&right.path_prefix))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

fn coverage_tokens_for_task_class(task_class: &str) -> Vec<String> {
    let mut tokens = bounded_query_terms(&[task_class]);
    let lower = symbol_index_key(task_class);
    if lower.contains("auth") || lower.contains("token") || lower.contains("credential") {
        for token in ["auth", "token", "credential", "middleware", "route", "test"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("route")
        || lower.contains("entrypoint")
        || lower.contains("ingress")
        || lower.contains("surface")
    {
        for token in ["route", "worker", "proxy", "webhook", "cli", "job"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("config") {
        for token in ["config", "entrypoint", "middleware"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("usage") || lower.contains("caller") || lower.contains("impact") {
        for token in ["route", "middleware", "auth", "test"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    tokens
}

fn coverage_for_task_class_from<D: ReadableDatabase>(
    db: &D,
    task_class: &str,
) -> Result<TaskClassCoverage, GraphStoreError> {
    let tokens = coverage_tokens_for_task_class(task_class);
    let entrypoints = entrypoints_for_task_from(db, &tokens)?;
    let surface_paths = surface_paths_for_behavior_from(db, &tokens)?;
    let credential_flows = credential_flow_candidates_from(db, &tokens)?;
    let subsystems = subsystems_matching_from(db, &tokens)?;

    let mut tests = BTreeMap::new();
    for id in entrypoints
        .iter()
        .chain(credential_flows.iter())
        .take(10)
        .map(|candidate| candidate.node.id.as_str())
    {
        for test in tests_for_surface_or_symbol_from(db, id)? {
            tests.insert(test.id.clone(), test);
        }
    }
    for test in behavior_tests_for_task_from(db, &tokens)? {
        tests.insert(test.id.clone(), test);
    }
    let tests = tests.into_values().collect::<Vec<_>>();

    let mut missing = Vec::new();
    if entrypoints.is_empty() {
        missing.push("entrypoints".to_string());
    }
    if surface_paths.is_empty() {
        missing.push("surface_paths".to_string());
    }
    if credential_flows.is_empty()
        && tokens
            .iter()
            .any(|token| generic_credential_term(token.as_str()))
    {
        missing.push("credential_flows".to_string());
    }
    if tests.is_empty() {
        missing.push("behavior_tests".to_string());
    }

    Ok(TaskClassCoverage {
        task_class: task_class.to_string(),
        tokens,
        entrypoints,
        surface_paths,
        credential_flows,
        subsystems,
        tests,
        missing,
    })
}

fn risk_key_candidates(path: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return keys;
    }
    keys.insert(trimmed.to_string());
    keys.insert(format!("{trimmed}/"));
    let mut current = trimmed;
    while let Some((parent, _)) = current.rsplit_once('/') {
        keys.insert(parent.to_string());
        keys.insert(format!("{parent}/"));
        current = parent;
    }
    keys
}

fn risk_path_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<String, GraphStoreError> {
    let txn = db.begin_read()?;
    if let Some(node) = get_node_in_txn(&txn, id_or_path)? {
        return Ok(path_from_node(&node).unwrap_or_else(|| id_or_path.to_string()));
    }
    Ok(id_or_path.to_string())
}

fn risks_for_node_or_path_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<Vec<RiskFlag>, GraphStoreError> {
    let path = risk_path_from(db, id_or_path)?;
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(RISK_FLAGS)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for key in risk_key_candidates(&path) {
        for row in t.get(key.as_str())? {
            let risk: RiskFlag = bincode::deserialize(row?.value())?;
            if seen.insert((risk.scope.clone(), risk.reason.clone())) {
                out.push(risk);
            }
        }
    }
    let prefix = path.trim_matches('/');
    if !prefix.is_empty() {
        let end = prefix_end(prefix);
        for entry in t.range(prefix..end.as_str())? {
            let (key, mut values) = entry?;
            if !key.value().starts_with(prefix) {
                continue;
            }
            while let Some(value) = values.next() {
                let risk: RiskFlag = bincode::deserialize(value?.value())?;
                if seen.insert((risk.scope.clone(), risk.reason.clone())) {
                    out.push(risk);
                }
            }
            if out.len() >= 50 {
                break;
            }
        }
    }
    out.sort_by(|left, right| {
        right
            .level
            .cmp(&left.level)
            .then_with(|| left.scope.cmp(&right.scope))
            .then_with(|| left.reason.cmp(&right.reason))
    });
    out.truncate(50);
    Ok(out)
}

fn repo_metadata_from<D: ReadableDatabase>(
    db: &D,
) -> Result<Option<RepoMetadata>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(META)?;
    let Some(value) = t.get(META_KEY_REPO_METADATA)? else {
        return Ok(None);
    };
    let meta: RepoMetadata = bincode::deserialize(value.value())?;
    Ok(Some(meta))
}

fn collect_adjacency<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &[u8]>,
    key: &str,
) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = match txn.open_multimap_table(table) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for r in t.get(key)? {
        let row = r?;
        let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
        out.push(rec);
    }
    Ok(out)
}

fn edge_count_from<D: ReadableDatabase>(db: &D) -> Result<u64, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_OUT)?;
    let mut count = 0;
    for entry in t.iter()? {
        let (_, mut values) = entry?;
        while let Some(value) = values.next() {
            value?;
            count += 1;
        }
    }
    Ok(count)
}

fn all_edges_from<D: ReadableDatabase>(db: &D) -> Result<Vec<Edge>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_OUT)?;
    let mut edges = Vec::new();
    for entry in t.iter()? {
        let (key, mut values) = entry?;
        let from = key.value().to_string();
        while let Some(value) = values.next() {
            let row = value?;
            let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
            edges.push(Edge::new(
                from.clone(),
                rec.other.as_str(),
                rec.kind,
                rec.confidence,
                rec.source.as_str(),
            ));
        }
    }
    edges.sort();
    Ok(edges)
}

fn list_areas_from<D: ReadableDatabase>(
    db: &D,
    depth: Option<u32>,
) -> Result<Vec<AreaNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(AREAS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        let area: AreaNode = bincode::deserialize(value.value())?;
        if let Some(d) = depth {
            if area_depth(&area) != d {
                continue;
            }
        }
        out.push(area);
    }
    // Stable sort by depth then path_prefix so callers don't see redb's
    // iteration order (which is sorted-by-key but the keys are structured
    // IDs, not path_prefixes).
    out.sort_by(|a, b| {
        area_depth(a)
            .cmp(&area_depth(b))
            .then_with(|| a.path_prefix.cmp(&b.path_prefix))
    });
    Ok(out)
}

fn overview_from<D: ReadableDatabase>(
    db: &D,
    area_limit: usize,
    entrypoint_limit: usize,
    risk_limit: usize,
) -> Result<Overview, GraphStoreError> {
    let repo = repo_metadata_from(db)?;

    // Top areas at depth 1, in path_prefix order.
    let mut areas = list_areas_from(db, Some(1))?;
    areas.truncate(area_limit);

    // Entrypoints: scan EDGES_OUT once, collect distinct sources whose
    // adjacency carries kind = EntrypointFor. O(E) but no per-file lookups.
    // Resolve src node_id → file path via FILES so the output matches what
    // surreal returned.
    let entrypoint_paths = {
        let txn = db.begin_read()?;
        let edges = txn.open_multimap_table(EDGES_OUT)?;
        let files = txn.open_table(FILES)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut paths = Vec::new();
        'outer: for kv in edges.iter()? {
            let (key, mut values) = kv?;
            let src = key.value().to_string();
            let mut has_entrypoint = false;
            while let Some(v) = values.next() {
                let row = v?;
                let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
                if matches!(rec.kind, EdgeKind::EntrypointFor) {
                    has_entrypoint = true;
                    break;
                }
            }
            if !has_entrypoint || !seen.insert(src.clone()) {
                continue;
            }
            let Some(blob) = files.get(src.as_str())? else {
                continue;
            };
            let file: FileNode = bincode::deserialize(blob.value())?;
            paths.push(file.path);
            if paths.len() >= entrypoint_limit {
                break 'outer;
            }
        }
        paths
    };

    // Risks: collect from RISK_FLAGS multimap, sort by level desc, truncate.
    let risks = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(RISK_FLAGS)?;
        let mut all = Vec::new();
        for kv in t.iter()? {
            let (_, mut values) = kv?;
            while let Some(v) = values.next() {
                let row = v?;
                let risk: RiskFlag = bincode::deserialize(row.value())?;
                all.push(risk);
            }
        }
        all.sort_by(|a, b| b.level.cmp(&a.level));
        all.truncate(risk_limit);
        all
    };

    Ok(Overview {
        repo,
        areas,
        entrypoint_paths,
        risks,
    })
}

fn list_files_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<FileNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(FILES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<FileNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_functions_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<FunctionNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(FUNCTIONS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<FunctionNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_classes_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<ClassNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(CLASSES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<ClassNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_docs_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<DocNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(DOCS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<DocNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_configs_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<ConfigNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(CONFIGS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<ConfigNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_surfaces_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<SurfaceNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(SURFACES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<SurfaceNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn read_repository_from<D: ReadableDatabase>(
    db: &D,
) -> Result<Option<RepositoryNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(REPOSITORIES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<RepositoryNode>(value.value())?);
    }
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out.into_iter().next())
}

fn list_directories_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<DirectoryNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(DIRECTORIES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<DirectoryNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn list_unresolved_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<UnresolvedNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(UNRESOLVED)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<UnresolvedNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

fn overview_v2_from<D: ReadableDatabase>(
    db: &D,
    limits: OverviewV2Limits,
) -> Result<OverviewV2, GraphStoreError> {
    let overview = overview_from(
        db,
        limits.area_limit,
        limits.entrypoint_limit,
        limits.risk_limit,
    )?;
    Ok(OverviewV2 {
        repo: overview.repo,
        repository: read_repository_from(db)?,
        areas: overview.areas,
        directories: list_directories_from(db, limits.directory_limit)?,
        entrypoint_paths: overview.entrypoint_paths,
        risks: overview.risks,
        files: list_files_from(db, limits.file_limit)?,
        functions: list_functions_from(db, limits.function_limit)?,
        classes: list_classes_from(db, limits.class_limit)?,
        docs: list_docs_from(db, limits.doc_limit)?,
        configs: list_configs_from(db, limits.config_limit)?,
        surfaces: list_surfaces_from(db, limits.surface_limit)?,
        unresolved: list_unresolved_from(db, limits.unresolved_limit)?,
    })
}

impl GraphStore {
    /// Read a typed node by canonical id.
    pub fn get_node(&self, id: &str) -> Result<Option<StoredNode>, GraphStoreError> {
        get_node_from(&self.db, id)
    }

    /// Read typed nodes by canonical id, preserving input order and omitting
    /// missing ids.
    pub fn get_nodes<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<StoredNode>, GraphStoreError> {
        get_nodes_from(&self.db, ids)
    }

    /// Read a display-ready projection for one typed node.
    pub fn node_display(&self, id: &str) -> Result<Option<NodeDisplay>, GraphStoreError> {
        node_display_from(&self.db, id)
    }

    /// Return the area id associated with a node id or exact file path.
    pub fn area_for_node(&self, id_or_path: &str) -> Result<Option<String>, GraphStoreError> {
        area_for_node_from(&self.db, id_or_path)
    }

    /// Return children via outgoing `contains` / `defines` edges.
    pub fn children(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        children_from(&self.db, id, kind)
    }

    /// Return parents via incoming `contains` / `defines` / `belongs_to` edges.
    pub fn parents(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        parents_from(&self.db, id, kind)
    }

    /// Return a display-ready relation view for rendered graph flows.
    pub fn relation_view(
        &self,
        id: &str,
        relation: GraphRelation,
    ) -> Result<RedbRelationView, GraphStoreError> {
        relation_view_from(&self.db, id, relation)
    }

    /// Return docs attached to a node through `documents` edges.
    pub fn docs_for(&self, id: &str) -> Result<Vec<DocNode>, GraphStoreError> {
        docs_for_from(&self.db, id)
    }

    /// Return configs attached to a node through `configures` or
    /// `entrypoint_for` edges.
    pub fn configs_for(&self, id: &str) -> Result<Vec<ConfigNode>, GraphStoreError> {
        configs_for_from(&self.db, id)
    }

    /// Return risk flags attached to a node id, exact path, or path prefix.
    pub fn risk_for_node_or_path(
        &self,
        id_or_path: &str,
    ) -> Result<Vec<RiskFlag>, GraphStoreError> {
        risks_for_node_or_path_from(&self.db, id_or_path)
    }

    /// Find function/class symbols by exact simple name, case-insensitive.
    pub fn find_symbols(
        &self,
        name: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<SymbolLookup>, GraphStoreError> {
        find_symbols_from(&self.db, name, kind)
    }

    /// Return bounded function/class symbol candidates for exact, prefix,
    /// component, path, and area signals.
    pub fn symbols_matching(&self, query: &str) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, SymbolMatchOptions::default())
    }

    /// Same as `symbols_matching`, with caller-provided bounds and filters.
    pub fn symbols_matching_with(
        &self,
        query: &str,
        options: SymbolMatchOptions,
    ) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, options)
    }

    /// Return bounded typed anchor candidates for tokenized task text.
    pub fn task_anchor_candidates<S: AsRef<str>>(
        &self,
        task_tokens: &[S],
        limit: usize,
    ) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
        task_anchor_candidates_from(&self.db, task_tokens, limit)
    }

    /// Return bounded symbol/path seeds for usage-boundary flows.
    pub fn usage_boundary_candidates(
        &self,
        scope: &str,
        symbol_kind: Option<StoredNodeKind>,
        limit: usize,
    ) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
        usage_boundary_candidates_from(&self.db, scope, symbol_kind, limit)
    }

    /// Return bounded ingress candidates for task tokens without scanning all
    /// nodes or all edges.
    pub fn entrypoints_for_task<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        entrypoints_for_task_from(&self.db, tokens)
    }

    /// Return bounded repo-relative paths with matching Surface/Flow behavior.
    pub fn surface_paths_for_behavior<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
        surface_paths_for_behavior_from(&self.db, tokens)
    }

    /// Return bounded credential issue/store/use/validation candidates.
    pub fn credential_flow_candidates<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        credential_flow_candidates_from(&self.db, tokens)
    }

    /// Return middleware/auth/header relations around a route, surface, or
    /// exact file path.
    pub fn middleware_chain_for_route(
        &self,
        route_or_file: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        middleware_chain_for_route_from(&self.db, route_or_file)
    }

    /// Return a bounded forwarding/header chain starting from a surface.
    pub fn forwarding_chain_for_surface(
        &self,
        surface: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        forwarding_chain_for_surface_from(&self.db, surface)
    }

    /// Return bounded subsystem slices matching task tokens.
    pub fn subsystems_matching<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
        subsystems_matching_from(&self.db, tokens)
    }

    /// Return behavior-test surfaces directly linked to a surface, symbol, or
    /// owning file.
    pub fn tests_for_surface_or_symbol(
        &self,
        id: &str,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        tests_for_surface_or_symbol_from(&self.db, id)
    }

    /// Return a bounded coverage projection for a generic task class.
    pub fn coverage_for_task_class(
        &self,
        task_class: &str,
    ) -> Result<TaskClassCoverage, GraphStoreError> {
        coverage_for_task_class_from(&self.db, task_class)
    }

    /// Return all typed nodes whose indexed path starts with `prefix`.
    pub fn nodes_under_path(&self, prefix: &str) -> Result<Vec<StoredNode>, GraphStoreError> {
        nodes_under_path_from(&self.db, prefix)
    }

    /// Return all functions whose file path starts with `prefix`.
    pub fn functions_under_path(&self, prefix: &str) -> Result<Vec<FunctionNode>, GraphStoreError> {
        functions_under_path_from(&self.db, prefix)
    }

    /// Return at most `limit` callable ids for one exact file path.
    pub fn function_ids_for_path(
        &self,
        path: &str,
        limit: usize,
    ) -> Result<BoundedFunctionIds, GraphStoreError> {
        function_ids_for_path_from(&self.db, path, limit)
    }

    /// Return incoming or outgoing adjacency, optionally filtered by edge kind.
    pub fn neighbors(
        &self,
        id: &str,
        direction: NeighborDirection,
        kind: Option<EdgeKind>,
    ) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        neighbors_from(&self.db, id, direction, kind)
    }

    /// Resolve an exact repo-relative path to the persisted file node.
    pub fn resolve_file_path(&self, path: &str) -> Result<Option<FileNode>, GraphStoreError> {
        resolve_file_path_from(&self.db, path)
    }

    /// Bounded V2 overview slice over typed nodes plus existing overview data.
    pub fn overview_v2(&self, limits: OverviewV2Limits) -> Result<OverviewV2, GraphStoreError> {
        overview_v2_from(&self.db, limits)
    }

    /// List all areas, optionally filtered by depth (1 = top-level, 2 =
    /// nested under top-level, etc). Depth is computed from `path_prefix`.
    pub fn list_areas(&self, depth: Option<u32>) -> Result<Vec<AreaNode>, GraphStoreError> {
        list_areas_from(&self.db, depth)
    }

    /// Outgoing adjacency rows for `entity_id`. Each row carries the partner
    /// (`other`) so callers don't need a second lookup.
    pub fn edges_from(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_OUT, entity_id)
    }

    /// Incoming adjacency rows for `entity_id`. The `O(in_degree)` shape
    /// from the dead-code algorithm fix — the whole reason EDGES_IN exists
    /// from day one in this schema.
    pub fn edges_to(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_IN, entity_id)
    }

    /// One-shot summary used by the `query-overview` CLI command:
    /// repo metadata + top-N areas at depth 1 + first-N entrypoint files +
    /// top-N risks (by RiskLevel descending).
    pub fn overview(
        &self,
        area_limit: usize,
        entrypoint_limit: usize,
        risk_limit: usize,
    ) -> Result<Overview, GraphStoreError> {
        overview_from(&self.db, area_limit, entrypoint_limit, risk_limit)
    }
}

impl ReadOnlyGraphStore {
    /// Read previously-written repo metadata, if any.
    pub fn repo_metadata(&self) -> Result<Option<RepoMetadata>, GraphStoreError> {
        repo_metadata_from(&self.db)
    }

    /// Read a typed node by canonical id.
    pub fn get_node(&self, id: &str) -> Result<Option<StoredNode>, GraphStoreError> {
        get_node_from(&self.db, id)
    }

    /// Read typed nodes by canonical id, preserving input order and omitting
    /// missing ids.
    pub fn get_nodes<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<StoredNode>, GraphStoreError> {
        get_nodes_from(&self.db, ids)
    }

    /// Read a display-ready projection for one typed node.
    pub fn node_display(&self, id: &str) -> Result<Option<NodeDisplay>, GraphStoreError> {
        node_display_from(&self.db, id)
    }

    /// Return the area id associated with a node id or exact file path.
    pub fn area_for_node(&self, id_or_path: &str) -> Result<Option<String>, GraphStoreError> {
        area_for_node_from(&self.db, id_or_path)
    }

    /// Return children via outgoing `contains` / `defines` edges.
    pub fn children(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        children_from(&self.db, id, kind)
    }

    /// Return parents via incoming `contains` / `defines` / `belongs_to` edges.
    pub fn parents(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        parents_from(&self.db, id, kind)
    }

    /// Return a display-ready relation view for rendered graph flows.
    pub fn relation_view(
        &self,
        id: &str,
        relation: GraphRelation,
    ) -> Result<RedbRelationView, GraphStoreError> {
        relation_view_from(&self.db, id, relation)
    }

    /// Return docs attached to a node through `documents` edges.
    pub fn docs_for(&self, id: &str) -> Result<Vec<DocNode>, GraphStoreError> {
        docs_for_from(&self.db, id)
    }

    /// Return configs attached to a node through `configures` or
    /// `entrypoint_for` edges.
    pub fn configs_for(&self, id: &str) -> Result<Vec<ConfigNode>, GraphStoreError> {
        configs_for_from(&self.db, id)
    }

    /// Return risk flags attached to a node id, exact path, or path prefix.
    pub fn risk_for_node_or_path(
        &self,
        id_or_path: &str,
    ) -> Result<Vec<RiskFlag>, GraphStoreError> {
        risks_for_node_or_path_from(&self.db, id_or_path)
    }

    /// Find function/class symbols by exact simple name, case-insensitive.
    pub fn find_symbols(
        &self,
        name: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<SymbolLookup>, GraphStoreError> {
        find_symbols_from(&self.db, name, kind)
    }

    /// Return bounded function/class symbol candidates for exact, prefix,
    /// component, path, and area signals.
    pub fn symbols_matching(&self, query: &str) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, SymbolMatchOptions::default())
    }

    /// Same as `symbols_matching`, with caller-provided bounds and filters.
    pub fn symbols_matching_with(
        &self,
        query: &str,
        options: SymbolMatchOptions,
    ) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, options)
    }

    /// Return bounded typed anchor candidates for tokenized task text.
    pub fn task_anchor_candidates<S: AsRef<str>>(
        &self,
        task_tokens: &[S],
        limit: usize,
    ) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
        task_anchor_candidates_from(&self.db, task_tokens, limit)
    }

    /// Return bounded symbol/path seeds for usage-boundary flows.
    pub fn usage_boundary_candidates(
        &self,
        scope: &str,
        symbol_kind: Option<StoredNodeKind>,
        limit: usize,
    ) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
        usage_boundary_candidates_from(&self.db, scope, symbol_kind, limit)
    }

    /// Return bounded ingress candidates for task tokens without scanning all
    /// nodes or all edges.
    pub fn entrypoints_for_task<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        entrypoints_for_task_from(&self.db, tokens)
    }

    /// Return bounded repo-relative paths with matching Surface/Flow behavior.
    pub fn surface_paths_for_behavior<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
        surface_paths_for_behavior_from(&self.db, tokens)
    }

    /// Return bounded credential issue/store/use/validation candidates.
    pub fn credential_flow_candidates<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        credential_flow_candidates_from(&self.db, tokens)
    }

    /// Return middleware/auth/header relations around a route, surface, or
    /// exact file path.
    pub fn middleware_chain_for_route(
        &self,
        route_or_file: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        middleware_chain_for_route_from(&self.db, route_or_file)
    }

    /// Return a bounded forwarding/header chain starting from a surface.
    pub fn forwarding_chain_for_surface(
        &self,
        surface: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        forwarding_chain_for_surface_from(&self.db, surface)
    }

    /// Return bounded subsystem slices matching task tokens.
    pub fn subsystems_matching<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
        subsystems_matching_from(&self.db, tokens)
    }

    /// Return behavior-test surfaces directly linked to a surface, symbol, or
    /// owning file.
    pub fn tests_for_surface_or_symbol(
        &self,
        id: &str,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        tests_for_surface_or_symbol_from(&self.db, id)
    }

    /// Return a bounded coverage projection for a generic task class.
    pub fn coverage_for_task_class(
        &self,
        task_class: &str,
    ) -> Result<TaskClassCoverage, GraphStoreError> {
        coverage_for_task_class_from(&self.db, task_class)
    }

    /// Return all typed nodes whose indexed path starts with `prefix`.
    pub fn nodes_under_path(&self, prefix: &str) -> Result<Vec<StoredNode>, GraphStoreError> {
        nodes_under_path_from(&self.db, prefix)
    }

    /// Return all functions whose file path starts with `prefix`.
    pub fn functions_under_path(&self, prefix: &str) -> Result<Vec<FunctionNode>, GraphStoreError> {
        functions_under_path_from(&self.db, prefix)
    }

    /// Return at most `limit` callable ids for one exact file path.
    pub fn function_ids_for_path(
        &self,
        path: &str,
        limit: usize,
    ) -> Result<BoundedFunctionIds, GraphStoreError> {
        function_ids_for_path_from(&self.db, path, limit)
    }

    /// Return incoming or outgoing adjacency, optionally filtered by edge kind.
    pub fn neighbors(
        &self,
        id: &str,
        direction: NeighborDirection,
        kind: Option<EdgeKind>,
    ) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        neighbors_from(&self.db, id, direction, kind)
    }

    /// Resolve an exact repo-relative path to the persisted file node.
    pub fn resolve_file_path(&self, path: &str) -> Result<Option<FileNode>, GraphStoreError> {
        resolve_file_path_from(&self.db, path)
    }

    /// Bounded V2 overview slice over typed nodes plus existing overview data.
    pub fn overview_v2(&self, limits: OverviewV2Limits) -> Result<OverviewV2, GraphStoreError> {
        overview_v2_from(&self.db, limits)
    }

    /// List all areas, optionally filtered by depth.
    pub fn list_areas(&self, depth: Option<u32>) -> Result<Vec<AreaNode>, GraphStoreError> {
        list_areas_from(&self.db, depth)
    }

    /// Outgoing adjacency rows for `entity_id`.
    pub fn edges_from(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_OUT, entity_id)
    }

    /// Incoming adjacency rows for `entity_id`.
    pub fn edges_to(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_IN, entity_id)
    }

    pub(crate) fn edge_count(&self) -> Result<u64, GraphStoreError> {
        edge_count_from(&self.db)
    }

    /// Return all persisted logical edges from the outgoing adjacency table.
    pub fn all_edges(&self) -> Result<Vec<Edge>, GraphStoreError> {
        all_edges_from(&self.db)
    }

    /// One-shot summary for query commands.
    pub fn overview(
        &self,
        area_limit: usize,
        entrypoint_limit: usize,
        risk_limit: usize,
    ) -> Result<Overview, GraphStoreError> {
        overview_from(&self.db, area_limit, entrypoint_limit, risk_limit)
    }
}

/// One open redb write transaction that accepts many node/edge inserts and
/// commits/rotates based on a policy.
///
/// "Op" counts every primary-table or adjacency insert; secondary-index
/// updates are folded into the parent op (one logical edge insert =
/// 2 adjacency rows, counted as 1 op). Bytes count actual bincode payload.
pub struct IndexSession<'db> {
    db: &'db Database,
    txn: Option<WriteTransaction>,
    durability: IndexDurability,
    ops_since_rotate: usize,
    bytes_since_rotate: usize,
}

impl<'db> IndexSession<'db> {
    /// Insert (or overwrite) a node row in the given primary table.
    ///
    /// Caller picks the table; higher-level helpers such as `insert_file`,
    /// `insert_area`, and `insert_function` layer path/symbol secondary-index
    /// writes on top of this primitive.
    pub fn insert_node(
        &mut self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &impl Serialize,
    ) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut t = txn.open_table(table)?;
            t.insert(key, bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Insert one logical edge as two adjacency rows: `(src → kind/dst)` in
    /// EDGES_OUT and `(dst → kind/src)` in EDGES_IN. Counted as one op.
    pub fn insert_edge(
        &mut self,
        src: &str,
        dst: &str,
        kind: EdgeKind,
        confidence: u16,
        source: InternedStr,
    ) -> Result<(), GraphStoreError> {
        let out_record = AdjacencyRecord {
            kind: kind.clone(),
            other: InternedStr::from(dst),
            confidence,
            source: source.clone(),
        };
        let in_record = AdjacencyRecord {
            kind,
            other: InternedStr::from(src),
            confidence,
            source,
        };
        let kind_key = edge_kind_label(&out_record.kind);
        let edge_record = Edge::new(
            src,
            dst,
            out_record.kind.clone(),
            confidence,
            out_record.source.clone(),
        );
        let out_bytes = bincode::serialize(&out_record)?;
        let in_bytes = bincode::serialize(&in_record)?;
        let edge_bytes = bincode::serialize(&edge_record)?;
        let written = out_bytes.len() + in_bytes.len() + edge_bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut out = txn.open_multimap_table(EDGES_OUT)?;
            out.insert(src, out_bytes.as_slice())?;
            let mut inv = txn.open_multimap_table(EDGES_IN)?;
            inv.insert(dst, in_bytes.as_slice())?;
            let mut by_kind = txn.open_multimap_table(EDGES_BY_KIND)?;
            by_kind.insert(kind_key, edge_bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Append `node_id` under `path` in a multimap path index
    /// (FUNCTIONS_BY_PATH or NODES_BY_PATH). No counter increment — these
    /// are tiny side effects of a parent insert.
    pub fn add_path_index(
        &mut self,
        table: MultimapTableDefinition<&str, &str>,
        path: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(table)?;
        t.insert(path, node_id)?;
        Ok(())
    }

    /// Append `node_id` under lowercased `name` in SYMBOL_BY_NAME.
    /// No counter increment — folded into a parent symbol insert.
    pub fn add_symbol_index(
        &mut self,
        name_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        t.insert(name_lower, node_id)?;
        Ok(())
    }

    /// Append `node_id` under a lowercased symbol component in
    /// SYMBOL_BY_COMPONENT. No counter increment — folded into a parent symbol
    /// insert.
    pub fn add_symbol_component_index(
        &mut self,
        component_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
        t.insert(component_lower, node_id)?;
        Ok(())
    }

    /// Append `node_id` under a lowercased file-path component in
    /// SYMBOL_BY_PATH_COMPONENT. No counter increment — folded into a parent
    /// symbol insert.
    pub fn add_symbol_path_component_index(
        &mut self,
        component_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
        t.insert(component_lower, node_id)?;
        Ok(())
    }

    /// Append a bincoded risk record under `scope` in RISK_FLAGS.
    pub fn add_risk(&mut self, scope: &str, value: &impl Serialize) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut t = txn.open_multimap_table(RISK_FLAGS)?;
            t.insert(scope, bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Commit all pending writes. Consumes the session.
    pub fn commit(mut self) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .take()
            .expect("IndexSession invariant: txn present");
        txn.commit()?;
        Ok(())
    }

    /// Force a rotation now (commit current txn, open a fresh one). Useful at
    /// natural pipeline boundaries (e.g. between indexing passes).
    pub fn rotate(&mut self) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .take()
            .expect("IndexSession invariant: txn present");
        txn.commit()?;
        let mut txn = self.db.begin_write()?;
        self.durability.apply(&mut txn)?;
        self.txn = Some(txn);
        self.ops_since_rotate = 0;
        self.bytes_since_rotate = 0;
        Ok(())
    }

    // Rotate policy rationale
    //
    // Returning `true` after an insert triggers commit + fresh transaction.
    // Returning `false` keeps batching.
    //
    // GraphStore sees many small writes (one node = one row, one edge = two
    // adjacency rows). On MediaWiki the ballpark is ~25k files + ~80k
    // functions + ~10k classes + ~1M edges = O(1M) ops. Per-op payload is
    // small (tens to a few hundred bytes).
    //
    // V1 uses the hybrid policy because neither single counter is sufficient:
    // ops bound fsync latency on tiny-row workloads, while bytes bound dirty
    // page growth when payloads get larger. The current constants are
    // ROTATE_EVERY_OPS = 4096 and ROTATE_EVERY_BYTES = 8 MiB. On a MediaWiki-
    // scale run with about 1M logical ops, the ops threshold yields roughly
    // 244 commits, which was acceptable in the initial profile runs. Tune the
    // constants only from measured index profiles, not from eval-score pressure.
    //
    // Constraints to respect:
    //   - `ops_since_rotate` and `bytes_since_rotate` reset on rotate.
    //   - returning `true` is ALWAYS safe (just slower); returning `false`
    //     too aggressively risks unbounded memory growth on big repos.
    fn should_rotate(&self) -> bool {
        self.ops_since_rotate >= ROTATE_EVERY_OPS || self.bytes_since_rotate >= ROTATE_EVERY_BYTES
    }
}

fn open_or_create_database(db_path: &Path) -> Result<Database, GraphStoreError> {
    match Database::create(db_path) {
        Ok(db) => Ok(db),
        Err(redb::DatabaseError::UpgradeRequired(found)) => {
            Err(GraphStoreError::IncompatibleRedbFileFormat {
                path: db_path.to_path_buf(),
                found,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn open_read_only_database(db_path: &Path) -> Result<ReadOnlyDatabase, GraphStoreError> {
    if !db_path.exists() {
        return Err(GraphStoreError::MissingGraphStore {
            path: db_path.to_path_buf(),
        });
    }
    match ReadOnlyDatabase::open(db_path) {
        Ok(db) => Ok(db),
        Err(redb::DatabaseError::UpgradeRequired(found)) => {
            Err(GraphStoreError::IncompatibleRedbFileFormat {
                path: db_path.to_path_buf(),
                found,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn ensure_schema(db: &Database) -> Result<(), GraphStoreError> {
    let txn = db.begin_write()?;
    {
        // Schema-version check: read existing into an owned [u8;4], release
        // the borrow, then write only if absent.
        let mut meta = txn.open_table(META)?;
        let existing: Option<[u8; 4]> = match meta.get(META_KEY_SCHEMA_VERSION)? {
            Some(v) => {
                let bytes = v.value();
                if bytes.len() != 4 {
                    return Err(GraphStoreError::SchemaMismatch {
                        found: 0,
                        expected: SCHEMA_VERSION,
                    });
                }
                Some(bytes.try_into().unwrap())
            }
            None => None,
        };
        match existing {
            Some(buf) => {
                let found = u32::from_le_bytes(buf);
                if found != SCHEMA_VERSION {
                    return Err(GraphStoreError::SchemaMismatch {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
            }
            None => {
                meta.insert(META_KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_le_bytes()[..])?;
            }
        }
        // Touch every table so they exist on a fresh DB.
        let _ = txn.open_table(REPOSITORIES)?;
        let _ = txn.open_table(DIRECTORIES)?;
        let _ = txn.open_table(FILES)?;
        let _ = txn.open_table(AREAS)?;
        let _ = txn.open_table(FUNCTIONS)?;
        let _ = txn.open_table(CLASSES)?;
        let _ = txn.open_table(DOCS)?;
        let _ = txn.open_table(CONFIGS)?;
        let _ = txn.open_table(SURFACES)?;
        let _ = txn.open_table(UNRESOLVED)?;
        let _ = txn.open_multimap_table(EDGES_OUT)?;
        let _ = txn.open_multimap_table(EDGES_IN)?;
        let _ = txn.open_multimap_table(EDGES_BY_KIND)?;
        let _ = txn.open_multimap_table(FUNCTIONS_BY_PATH)?;
        let _ = txn.open_multimap_table(NODES_BY_PATH)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
        let _ = txn.open_multimap_table(RISK_FLAGS)?;
    }
    txn.commit()?;
    Ok(())
}

fn verify_schema_read_only(db: &ReadOnlyDatabase) -> Result<(), GraphStoreError> {
    let txn = db.begin_read()?;
    {
        let meta = txn.open_table(META)?;
        let value = meta
            .get(META_KEY_SCHEMA_VERSION)?
            .ok_or(GraphStoreError::SchemaMismatch {
                found: 0,
                expected: SCHEMA_VERSION,
            })?;
        let bytes = value.value();
        if bytes.len() != 4 {
            return Err(GraphStoreError::SchemaMismatch {
                found: 0,
                expected: SCHEMA_VERSION,
            });
        }
        let found = u32::from_le_bytes(bytes.try_into().unwrap());
        if found != SCHEMA_VERSION {
            return Err(GraphStoreError::SchemaMismatch {
                found,
                expected: SCHEMA_VERSION,
            });
        }

        // Query commands expect the same tables as the writable store. Read-only
        // open validates the materialized store instead of creating anything.
        let _ = txn.open_table(REPOSITORIES)?;
        let _ = txn.open_table(DIRECTORIES)?;
        let _ = txn.open_table(FILES)?;
        let _ = txn.open_table(AREAS)?;
        let _ = txn.open_table(FUNCTIONS)?;
        let _ = txn.open_table(CLASSES)?;
        let _ = txn.open_table(DOCS)?;
        let _ = txn.open_table(CONFIGS)?;
        let _ = txn.open_table(SURFACES)?;
        let _ = txn.open_table(UNRESOLVED)?;
        let _ = txn.open_multimap_table(EDGES_OUT)?;
        let _ = txn.open_multimap_table(EDGES_IN)?;
        let _ = txn.open_multimap_table(EDGES_BY_KIND)?;
        let _ = txn.open_multimap_table(FUNCTIONS_BY_PATH)?;
        let _ = txn.open_multimap_table(NODES_BY_PATH)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
        let _ = txn.open_multimap_table(RISK_FLAGS)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use redb::{ReadableDatabase, ReadableTableMetadata};
    use serde::Deserialize;

    macro_rules! function_name {
        () => {{
            fn f() {}
            fn type_name<T>(_: T) -> &'static str {
                std::any::type_name::<T>()
            }
            let n = type_name(f);
            &n[..n.len() - 3]
        }};
    }

    fn tmp_root(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("aethyme_graph_store_{name}_{nonce}"))
    }

    fn mark_graph_store_as_redb_v2(root: &Path) {
        let db_path = root.join(".aethyme").join(DB_FILE_NAME);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .expect("open db file");
        for offset in [64u64, 192u64] {
            std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))
                .expect("seek version byte");
            std::io::Write::write_all(&mut file, &[2]).expect("write v2 marker");
        }
    }

    #[test]
    fn open_creates_dotaethyme_and_db_file() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        assert!(root.join(".aethyme").is_dir(), ".aethyme dir created");
        assert!(store.path().exists(), "graph_store.redb file created");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reopen_is_idempotent() {
        let root = tmp_root(function_name!());
        let _ = GraphStore::open(&root).expect("first open");
        let _ = GraphStore::open(&root).expect("reopen");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn all_tables_exist_on_fresh_db() {
        // Reads from a fresh store should not trip TableDoesNotExist.
        // ensure_schema must touch every table so downstream queries
        // don't have to special-case empty-DB lookups.
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let txn = store.db().begin_read().expect("read txn");

        // Single tables: open_table on a missing table is a TableError.
        txn.open_table(REPOSITORIES).expect("REPOSITORIES");
        txn.open_table(DIRECTORIES).expect("DIRECTORIES");
        txn.open_table(FILES).expect("FILES");
        txn.open_table(AREAS).expect("AREAS");
        txn.open_table(FUNCTIONS).expect("FUNCTIONS");
        txn.open_table(CLASSES).expect("CLASSES");
        txn.open_table(DOCS).expect("DOCS");
        txn.open_table(CONFIGS).expect("CONFIGS");
        txn.open_table(SURFACES).expect("SURFACES");
        txn.open_table(UNRESOLVED).expect("UNRESOLVED");
        txn.open_table(META).expect("META");

        // Multimap tables.
        let edges_out = txn.open_multimap_table(EDGES_OUT).expect("EDGES_OUT");
        let edges_in = txn.open_multimap_table(EDGES_IN).expect("EDGES_IN");
        let edges_by_kind = txn
            .open_multimap_table(EDGES_BY_KIND)
            .expect("EDGES_BY_KIND");
        assert_eq!(edges_out.len().unwrap(), 0);
        assert_eq!(edges_in.len().unwrap(), 0);
        assert_eq!(edges_by_kind.len().unwrap(), 0);
        txn.open_multimap_table(FUNCTIONS_BY_PATH)
            .expect("FUNCTIONS_BY_PATH");
        txn.open_multimap_table(NODES_BY_PATH)
            .expect("NODES_BY_PATH");
        txn.open_multimap_table(SYMBOL_BY_NAME)
            .expect("SYMBOL_BY_NAME");
        txn.open_multimap_table(SYMBOL_BY_COMPONENT)
            .expect("SYMBOL_BY_COMPONENT");
        txn.open_multimap_table(RISK_FLAGS).expect("RISK_FLAGS");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_version_is_persisted() {
        let root = tmp_root(function_name!());
        let _ = GraphStore::open(&root).expect("open");
        // Reopen and confirm the sentinel reads back as the current schema.
        let store = GraphStore::open(&root).expect("reopen");
        let txn = store.db().begin_read().expect("read txn");
        let meta = txn.open_table(META).expect("META");
        let value = meta
            .get(META_KEY_SCHEMA_VERSION)
            .expect("get")
            .expect("present");
        let bytes: [u8; 4] = value.value().try_into().expect("4 bytes");
        assert_eq!(u32::from_le_bytes(bytes), SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_open_does_not_create_missing_store() {
        let root = tmp_root(function_name!());
        assert!(GraphStore::open_read_only(&root).is_err());
        assert!(
            !root.join(".aethyme").exists(),
            "read-only open must not initialize .aethyme"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Test-only sample node — small Serialize/Deserialize struct so we can
    /// exercise insert_node without depending on FileNode/FunctionNode shape.
    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct SampleNode {
        id: String,
        path: String,
    }

    fn read_node_bytes(
        db: &Database,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Option<Vec<u8>> {
        let txn = db.begin_read().expect("read txn");
        let t = txn.open_table(table).expect("open");
        t.get(key).expect("get").map(|v| v.value().to_vec())
    }

    fn collect_multimap(
        db: &Database,
        table: MultimapTableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Vec<Vec<u8>> {
        let txn = db.begin_read().expect("read txn");
        let t = txn.open_multimap_table(table).expect("open");
        let iter = t.get(key).expect("get");
        iter.map(|r| r.expect("row").value().to_vec()).collect()
    }

    fn collect_str_multimap(
        db: &Database,
        table: MultimapTableDefinition<&str, &str>,
        key: &str,
    ) -> Vec<String> {
        let txn = db.begin_read().expect("read txn");
        let t = txn.open_multimap_table(table).expect("open");
        let iter = t.get(key).expect("get");
        iter.map(|r| r.expect("row").value().to_string()).collect()
    }

    #[test]
    fn insert_node_and_read_back() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");
        let node = SampleNode {
            id: "file:Repo:src/lib.rs".into(),
            path: "src/lib.rs".into(),
        };
        session.insert_node(FILES, &node.id, &node).expect("insert");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), FILES, "file:Repo:src/lib.rs").expect("present");
        let got: SampleNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, node);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn insert_edge_writes_both_directions() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");
        session
            .insert_edge(
                "file:Repo:a.rs",
                "file:Repo:b.rs",
                EdgeKind::Imports,
                100,
                InternedStr::from("import"),
            )
            .expect("insert_edge");
        session.commit().expect("commit");

        let out = collect_multimap(store.db(), EDGES_OUT, "file:Repo:a.rs");
        assert_eq!(out.len(), 1, "EDGES_OUT has one row keyed by src");
        let out_rec: AdjacencyRecord = bincode::deserialize(&out[0]).expect("decode");
        assert_eq!(out_rec.kind, EdgeKind::Imports);
        assert_eq!(out_rec.other.as_str(), "file:Repo:b.rs");
        assert_eq!(out_rec.confidence, 100);

        let inv = collect_multimap(store.db(), EDGES_IN, "file:Repo:b.rs");
        assert_eq!(inv.len(), 1, "EDGES_IN has one row keyed by dst");
        let in_rec: AdjacencyRecord = bincode::deserialize(&inv[0]).expect("decode");
        assert_eq!(in_rec.kind, EdgeKind::Imports);
        assert_eq!(in_rec.other.as_str(), "file:Repo:a.rs");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_store_reads_query_surfaces() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let file_a = FileNode::new(
            "Repo",
            "src/a.rs",
            Some("Rust".to_string()),
            crate::model::file::FileRole::Source,
            10,
            100,
            false,
            Some(area.id.clone()),
        );
        let file_b = FileNode::new(
            "Repo",
            "src/b.rs",
            Some("Rust".to_string()),
            crate::model::file::FileRole::Source,
            20,
            200,
            false,
            Some(area.id.clone()),
        );
        let edge = Edge::new(
            file_a.id.clone(),
            file_b.id.clone(),
            EdgeKind::Imports,
            100,
            "read-only-test",
        );

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("area");
        insert_file(&mut session, &file_a).expect("file a");
        insert_file(&mut session, &file_b).expect("file b");
        insert_edge(&mut session, &edge).expect("edge");
        session.commit().expect("commit");
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.to_string_lossy().to_string(),
                commit_hash: Some("abc123".to_string()),
                indexed_at_unix: 1,
                file_count: 2,
                languages: vec!["Rust".to_string()],
            })
            .expect("metadata");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only open");
        assert_eq!(
            readonly
                .repo_metadata()
                .expect("metadata")
                .unwrap()
                .file_count,
            2
        );
        assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);
        assert_eq!(
            readonly.edges_from(&file_a.id).expect("edges from")[0]
                .other
                .as_str(),
            file_b.id
        );
        assert_eq!(
            readonly.edges_to(&file_b.id).expect("edges to")[0]
                .other
                .as_str(),
            file_a.id
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn surface_node(kind: SurfaceKind, path: &str, name: &str, line: usize) -> SurfaceNode {
        let schema_id = aethyme_graph_schema::NodeId::new(
            match kind {
                SurfaceKind::BehaviorTestSurface => {
                    aethyme_graph_schema::NodeKind::BehaviorTestSurface
                }
                SurfaceKind::CliSurface => aethyme_graph_schema::NodeKind::CliSurface,
                SurfaceKind::CredentialOperation => {
                    aethyme_graph_schema::NodeKind::CredentialOperation
                }
                SurfaceKind::JobSurface => aethyme_graph_schema::NodeKind::JobSurface,
                SurfaceKind::MiddlewareInstallation => {
                    aethyme_graph_schema::NodeKind::MiddlewareInstallation
                }
                SurfaceKind::ProxySurface => aethyme_graph_schema::NodeKind::ProxySurface,
                SurfaceKind::QueueSurface => aethyme_graph_schema::NodeKind::QueueSurface,
                SurfaceKind::RouteSurface => aethyme_graph_schema::NodeKind::RouteSurface,
                SurfaceKind::WebhookSurface => aethyme_graph_schema::NodeKind::WebhookSurface,
                SurfaceKind::WorkerSurface => aethyme_graph_schema::NodeKind::WorkerSurface,
            },
            "Repo",
            path,
            name,
        )
        .expect("schema id");
        SurfaceNode {
            id: InternedStr::from(schema_id.as_str()),
            kind,
            name: InternedStr::from(name),
            file_id: InternedStr::from(format!("file:Repo:{path}")),
            file_path: InternedStr::from(path),
            area_id: Some(InternedStr::from("area:Repo:src")),
            language: InternedStr::from("python"),
            line,
            detail: InternedStr::from(kind.label()),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn surface_nodes_are_persisted_and_queryable() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let file = FileNode::new(
            "Repo",
            "src/routes.py",
            Some("python".to_string()),
            crate::model::file::FileRole::Source,
            20,
            300,
            false,
            Some(area.id.clone()),
        );
        let route = surface_node(
            SurfaceKind::RouteSurface,
            "src/routes.py",
            "GET /api/token",
            3,
        );
        let middleware = surface_node(
            SurfaceKind::MiddlewareInstallation,
            "src/routes.py",
            "TokenAuthMiddleware",
            7,
        );
        let proxy = surface_node(
            SurfaceKind::ProxySurface,
            "src/routes.py",
            "https://api.example.com",
            12,
        );

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("area");
        insert_file(&mut session, &file).expect("file");
        for surface in [&route, &middleware, &proxy] {
            insert_surface(&mut session, surface).expect("surface");
        }
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                route.id.as_str(),
                EdgeKind::Exposes,
                850,
                "surface-flow",
            ),
        )
        .expect("route edge");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                middleware.id.as_str(),
                EdgeKind::InstallsMiddleware,
                850,
                "surface-flow",
            ),
        )
        .expect("middleware edge");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                proxy.id.as_str(),
                EdgeKind::ForwardsTo,
                850,
                "surface-flow",
            ),
        )
        .expect("proxy edge");
        session.commit().expect("commit");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only open");
        assert_eq!(
            readonly
                .node_display(route.id.as_str())
                .expect("display")
                .unwrap()
                .kind,
            StoredNodeKind::RouteSurface
        );
        assert!(matches!(
            readonly.get_node(middleware.id.as_str()).expect("node"),
            Some(StoredNode::Surface(node)) if node.kind == SurfaceKind::MiddlewareInstallation
        ));
        let nodes = readonly.nodes_under_path("src/").expect("nodes under path");
        assert!(
            nodes
                .iter()
                .filter(|node| matches!(node, StoredNode::Surface(_)))
                .count()
                >= 3
        );
        let symbols = readonly
            .find_symbols("GET /api/token", Some(StoredNodeKind::RouteSurface))
            .expect("symbol lookup");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].id, route.id.as_str());
        let children = readonly.children(&file.id, None).expect("children");
        assert!(children
            .iter()
            .any(|node| node.kind == StoredNodeKind::RouteSurface));
        assert!(children
            .iter()
            .any(|node| node.kind == StoredNodeKind::MiddlewareInstallation));
        assert!(children
            .iter()
            .any(|node| node.kind == StoredNodeKind::ProxySurface));
        assert_eq!(
            readonly
                .overview_v2(OverviewV2Limits {
                    area_limit: 0,
                    directory_limit: 0,
                    entrypoint_limit: 0,
                    risk_limit: 0,
                    file_limit: 0,
                    function_limit: 0,
                    class_limit: 0,
                    doc_limit: 0,
                    config_limit: 0,
                    surface_limit: 10,
                    unresolved_limit: 0,
                })
                .expect("overview")
                .surfaces
                .len(),
            3
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn surface_flow_edges_round_trip_through_redb() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let file = FileNode::new(
            "Repo",
            "src/flow.py",
            Some("python".to_string()),
            crate::model::file::FileRole::Source,
            20,
            300,
            false,
            Some(area.id.clone()),
        );
        let surface = surface_node(
            SurfaceKind::CredentialOperation,
            "src/flow.py",
            "token flow",
            5,
        );
        let flow_edges = [
            EdgeKind::Exposes,
            EdgeKind::ForwardsTo,
            EdgeKind::RewritesHeader,
            EdgeKind::InstallsMiddleware,
            EdgeKind::ValidatesCredential,
            EdgeKind::Authorizes,
            EdgeKind::IssuesCredential,
            EdgeKind::StoresCredential,
            EdgeKind::UsesCredential,
            EdgeKind::TestedBy,
        ];

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("area");
        insert_file(&mut session, &file).expect("file");
        insert_surface(&mut session, &surface).expect("surface");
        for kind in &flow_edges {
            insert_edge(
                &mut session,
                &Edge::new(
                    &file.id,
                    surface.id.as_str(),
                    kind.clone(),
                    850,
                    "surface-flow",
                ),
            )
            .expect("flow edge");
        }
        session.commit().expect("commit");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only open");
        let persisted = readonly
            .neighbors(&file.id, NeighborDirection::Outgoing, None)
            .expect("outgoing neighbors")
            .into_iter()
            .filter(|edge| edge.other == surface.id.as_str())
            .map(|edge| edge.kind)
            .collect::<BTreeSet<_>>();
        for kind in &flow_edges {
            assert!(
                persisted.contains(kind),
                "expected persisted flow edge {kind:?}; got {persisted:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn compact_preserves_committed_query_data() {
        let root = tmp_root(function_name!());
        let mut store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("area");
        session.commit().expect("commit");

        let _ = store.compact().expect("compact");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only reopen");
        assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_durable_index_commits_are_persisted_by_final_metadata_commit() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let mut session = store
            .begin_index_with_durability(IndexDurability::None)
            .expect("session");
        insert_area(&mut session, &area).expect("area");
        session.commit().expect("commit");
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.to_string_lossy().to_string(),
                commit_hash: None,
                indexed_at_unix: 1,
                file_count: 0,
                languages: Vec::new(),
            })
            .expect("metadata");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only reopen");
        assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_store_does_not_replace_public_store_until_publish() {
        let root = tmp_root(function_name!());
        let old_area = AreaNode::new("Repo", "old", false);
        let new_area = AreaNode::new("Repo", "new", false);
        let store = GraphStore::reset(&root).expect("open public");
        let mut session = store.begin_index().expect("public session");
        insert_area(&mut session, &old_area).expect("old area");
        session.commit().expect("public commit");
        drop(store);

        let staging = GraphStore::reset_staging(&root).expect("open staging");
        let mut session = staging
            .begin_index_with_durability(IndexDurability::None)
            .expect("staging session");
        insert_area(&mut session, &new_area).expect("new area");
        session.commit().expect("staging commit");
        staging
            .set_repo_metadata(&RepoMetadata {
                root_path: root.to_string_lossy().to_string(),
                commit_hash: None,
                indexed_at_unix: 1,
                file_count: 0,
                languages: Vec::new(),
            })
            .expect("metadata");
        drop(staging);

        let readonly = GraphStore::open_read_only(&root).expect("read public");
        assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![old_area]);
        drop(readonly);

        GraphStore::publish_staging(&root).expect("publish staging");
        let readonly = GraphStore::open_read_only(&root).expect("read published");
        assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![new_area]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reset_removes_stale_staging_store() {
        let root = tmp_root(function_name!());
        let staging = GraphStore::reset_staging(&root).expect("open staging");
        let staging_path = GraphStore::staging_path(&root);
        assert!(staging_path.exists(), "staging file exists");
        drop(staging);

        let store = GraphStore::reset(&root).expect("reset public");
        assert!(!staging_path.exists(), "normal reset cleans stale staging");
        assert!(store.path().exists(), "public store exists");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drop_without_commit_loses_writes() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        {
            let mut session = store.begin_index().expect("session");
            let node = SampleNode {
                id: "x".into(),
                path: "x.rs".into(),
            };
            session.insert_node(FILES, &node.id, &node).expect("insert");
            // session dropped without commit — txn aborts
        }
        assert!(
            read_node_bytes(store.db(), FILES, "x").is_none(),
            "uncommitted writes must not be visible"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_rotate_persists_then_continues() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");

        let a = SampleNode {
            id: "a".into(),
            path: "a.rs".into(),
        };
        session.insert_node(FILES, &a.id, &a).expect("insert a");
        session.rotate().expect("rotate");

        // After rotate, `a` is durable; subsequent inserts continue in a fresh txn.
        assert!(
            read_node_bytes(store.db(), FILES, "a").is_some(),
            "a is durable"
        );

        let b = SampleNode {
            id: "b".into(),
            path: "b.rs".into(),
        };
        session.insert_node(FILES, &b.id, &b).expect("insert b");
        session.commit().expect("commit");

        assert!(
            read_node_bytes(store.db(), FILES, "b").is_some(),
            "b after second commit"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn secondary_indexes_record_node_ids() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");

        session
            .add_path_index(FUNCTIONS_BY_PATH, "src/lib.rs", "fn:Repo:src/lib.rs:foo")
            .expect("path");
        session
            .add_symbol_index("foo", "fn:Repo:src/lib.rs:foo")
            .expect("name");
        session
            .add_symbol_component_index("foo", "fn:Repo:src/lib.rs:foo")
            .expect("component");
        session
            .add_symbol_path_component_index("lib", "fn:Repo:src/lib.rs:foo")
            .expect("path component");
        session.commit().expect("commit");

        let txn = store.db().begin_read().expect("read");
        let by_path = txn
            .open_multimap_table(FUNCTIONS_BY_PATH)
            .expect("FUNCTIONS_BY_PATH");
        let path_hits: Vec<String> = by_path
            .get("src/lib.rs")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(path_hits, vec!["fn:Repo:src/lib.rs:foo"]);

        let by_name = txn
            .open_multimap_table(SYMBOL_BY_NAME)
            .expect("SYMBOL_BY_NAME");
        let name_hits: Vec<String> = by_name
            .get("foo")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(name_hits, vec!["fn:Repo:src/lib.rs:foo"]);

        let by_component = txn
            .open_multimap_table(SYMBOL_BY_COMPONENT)
            .expect("SYMBOL_BY_COMPONENT");
        let component_hits: Vec<String> = by_component
            .get("foo")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(component_hits, vec!["fn:Repo:src/lib.rs:foo"]);

        let by_path_component = txn
            .open_multimap_table(SYMBOL_BY_PATH_COMPONENT)
            .expect("SYMBOL_BY_PATH_COMPONENT");
        let path_component_hits: Vec<String> = by_path_component
            .get("lib")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(path_component_hits, vec!["fn:Repo:src/lib.rs:foo"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    use crate::model::file::FileRole;
    use crate::model::risk::{RiskArea, RiskLevel};

    fn sample_file(repo: &str, path: &str, area_id: Option<&str>) -> FileNode {
        FileNode::new(
            repo,
            path,
            Some("rust".into()),
            FileRole::Source,
            42,
            1024,
            false,
            area_id.map(|s| s.to_string()),
        )
    }

    fn sample_directory(repo: &str, path: &str, area_id: Option<&str>) -> DirectoryNode {
        DirectoryNode::new(repo, path, area_id.map(|s| s.to_string()))
    }

    fn sample_class(file: &FileNode, name: &str) -> ClassNode {
        ClassNode::new(
            "Repo",
            InternedStr::from(file.id.clone()),
            InternedStr::from(file.path.clone()),
            file.area_id.clone().map(InternedStr::from),
            InternedStr::from(
                file.language
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
            InternedStr::from(name),
            7,
            InternedStr::from(format!("class {name}")),
        )
    }

    fn sample_function(
        file: &FileNode,
        name: &str,
        parent_class_id: Option<InternedStr>,
    ) -> FunctionNode {
        FunctionNode::new(
            "Repo",
            InternedStr::from(file.id.clone()),
            InternedStr::from(file.path.clone()),
            file.area_id.clone().map(InternedStr::from),
            parent_class_id,
            InternedStr::from(
                file.language
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
            InternedStr::from(name),
            12,
            InternedStr::from(format!("def {name}()")),
        )
    }

    fn sample_unresolved(file: &FileNode, name: &str) -> UnresolvedNode {
        UnresolvedNode::new(
            InternedStr::from(format!("unresolved_symbol:Repo:{name}")),
            InternedStr::from(name),
            Some(InternedStr::from("function")),
            InternedStr::from(file.id.clone()),
            InternedStr::from(file.id.clone()),
            InternedStr::from(file.path.clone()),
            file.area_id.clone().map(InternedStr::from),
            InternedStr::from(
                file.language
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
        )
    }

    #[test]
    fn typed_insert_repository_round_trip() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let repository = RepositoryNode::new("Repo", root.to_str().unwrap());
        let key = repository.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_repository(&mut session, &repository).expect("insert_repository");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), REPOSITORIES, &key).expect("present");
        let got: RepositoryNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, repository);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_directory_indexes_path() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let directory = sample_directory("Repo", "src", Some("area:Repo:src"));
        let id = directory.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_directory(&mut session, &directory).expect("insert_directory");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), DIRECTORIES, &id).expect("present");
        let got: DirectoryNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, directory);
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "src"),
            vec![id]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_area_round_trip() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let key = area.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("insert_area");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), AREAS, &key).expect("present");
        let got: AreaNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, area);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_file_indexes_path() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let id = file.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_file(&mut session, &file).expect("insert_file");
        session.commit().expect("commit");

        // Primary row.
        let bytes = read_node_bytes(store.db(), FILES, &id).expect("present");
        let got: FileNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, file);

        // NODES_BY_PATH lookup.
        let txn = store.db().begin_read().expect("read");
        let by_path = txn
            .open_multimap_table(NODES_BY_PATH)
            .expect("NODES_BY_PATH");
        let hits: Vec<String> = by_path
            .get("src/lib.rs")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(hits, vec![id]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_function_populates_symbol_and_path_indexes() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let function = sample_function(&file, "LoadToken", None);
        let id = function.id.to_string();

        let mut session = store.begin_index().expect("session");
        insert_function(&mut session, &function).expect("insert_function");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), FUNCTIONS, &id).expect("present");
        let got: FunctionNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, function);
        assert_eq!(
            collect_str_multimap(store.db(), FUNCTIONS_BY_PATH, "src/lib.rs"),
            vec![id.clone()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
            vec![id.clone()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_NAME, "loadtoken"),
            vec![id]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "load"),
            vec![function.id.to_string()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "token"),
            vec![function.id.to_string()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_PATH_COMPONENT, "lib"),
            vec![function.id.to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exact_file_callable_lookup_is_deterministic_and_strictly_bounded() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let prefixed_file = sample_file("Repo", "src/lib.rs.extra", Some("area:Repo:src"));
        let alpha = sample_function(&file, "Alpha", None);
        let zeta = sample_function(&file, "Zeta", None);
        let prefixed = sample_function(&prefixed_file, "Prefixed", None);

        let mut session = store.begin_index().expect("session");
        for function in [&zeta, &prefixed, &alpha] {
            insert_function(&mut session, function).expect("insert_function");
        }
        session.commit().expect("commit");

        let bounded = store
            .function_ids_for_path("src/lib.rs", 1)
            .expect("bounded exact lookup");
        assert_eq!(bounded.ids, vec![alpha.id.to_string()]);
        assert!(bounded.truncated);

        let complete = store
            .function_ids_for_path("src/lib.rs", 2)
            .expect("complete exact lookup");
        assert_eq!(
            complete.ids,
            vec![alpha.id.to_string(), zeta.id.to_string()]
        );
        assert!(!complete.truncated);
        assert!(!complete.ids.contains(&prefixed.id.to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_class_populates_symbol_and_path_indexes() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let class = sample_class(&file, "TokenLoader");
        let id = class.id.to_string();

        let mut session = store.begin_index().expect("session");
        insert_class(&mut session, &class).expect("insert_class");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), CLASSES, &id).expect("present");
        let got: ClassNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, class);
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
            vec![id.clone()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_NAME, "tokenloader"),
            vec![id]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "token"),
            vec![class.id.to_string()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "loader"),
            vec![class.id.to_string()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), SYMBOL_BY_PATH_COMPONENT, "lib"),
            vec![class.id.to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_doc_and_config_index_paths() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let doc = DocNode::new(
            "Repo",
            "file:Repo:docs/auth.md",
            "docs/auth.md",
            "Auth",
            "markdown",
            Some("area:Repo:docs".to_string()),
        );
        let config = ConfigNode::new(
            "Repo",
            "file:Repo:pyproject.toml",
            "pyproject.toml",
            "toml",
            None,
        );

        let mut session = store.begin_index().expect("session");
        insert_doc(&mut session, &doc).expect("insert_doc");
        insert_config(&mut session, &config).expect("insert_config");
        session.commit().expect("commit");

        let doc_bytes = read_node_bytes(store.db(), DOCS, &doc.id).expect("doc present");
        let got_doc: DocNode = bincode::deserialize(&doc_bytes).expect("decode doc");
        assert_eq!(got_doc, doc);
        let config_bytes =
            read_node_bytes(store.db(), CONFIGS, &config.id).expect("config present");
        let got_config: ConfigNode = bincode::deserialize(&config_bytes).expect("decode config");
        assert_eq!(got_config, config);
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "docs/auth.md"),
            vec![doc.id.clone()]
        );
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "pyproject.toml"),
            vec![config.id.clone()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_unresolved_indexes_source_path() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let unresolved = sample_unresolved(&file, "missing_call");
        let id = unresolved.id.to_string();

        let mut session = store.begin_index().expect("session");
        insert_unresolved(&mut session, &unresolved).expect("insert_unresolved");
        session.commit().expect("commit");

        let bytes = read_node_bytes(store.db(), UNRESOLVED, &id).expect("present");
        let got: UnresolvedNode = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(got, unresolved);
        assert_eq!(
            collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
            vec![id]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_edge_passes_through() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let edge = Edge::new(
            "file:Repo:a.rs",
            "file:Repo:b.rs",
            EdgeKind::Imports,
            100,
            "import",
        );

        let mut session = store.begin_index().expect("session");
        insert_edge(&mut session, &edge).expect("insert_edge");
        session.commit().expect("commit");

        let out = collect_multimap(store.db(), EDGES_OUT, "file:Repo:a.rs");
        assert_eq!(out.len(), 1);
        let rec: AdjacencyRecord = bincode::deserialize(&out[0]).expect("decode");
        assert_eq!(rec.kind, EdgeKind::Imports);
        assert_eq!(rec.other.as_str(), "file:Repo:b.rs");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typed_insert_risk_round_trip() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let risk = RiskFlag::new("auth/", RiskArea::Auth, RiskLevel::High, "secrets");

        let mut session = store.begin_index().expect("session");
        insert_risk(&mut session, &risk).expect("insert_risk");
        session.commit().expect("commit");

        let txn = store.db().begin_read().expect("read");
        let t = txn.open_multimap_table(RISK_FLAGS).expect("RISK_FLAGS");
        let hits: Vec<RiskFlag> = t
            .get("auth/")
            .expect("get")
            .map(|r| bincode::deserialize::<RiskFlag>(r.expect("row").value()).expect("decode"))
            .collect();
        assert_eq!(hits, vec![risk]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_and_read_repo_metadata() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        assert!(store.repo_metadata().expect("read").is_none());

        let meta = RepoMetadata {
            root_path: "/tmp/repo".into(),
            commit_hash: Some("deadbeef".into()),
            indexed_at_unix: 1_700_000_000,
            file_count: 4093,
            languages: vec!["php".into(), "js".into()],
        };
        store.set_repo_metadata(&meta).expect("write meta");

        let got = store.repo_metadata().expect("read").expect("present");
        assert_eq!(got, meta);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reset_drops_all_data_but_preserves_schema() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "src", false);
        let key = area.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("insert");
        session.commit().expect("commit");
        assert!(read_node_bytes(store.db(), AREAS, &key).is_some());

        // Drop the original handle so the file lock is released, then reset.
        drop(store);
        let store2 = GraphStore::reset(&root).expect("reset");
        assert!(read_node_bytes(store2.db(), AREAS, &key).is_none(), "wiped");

        // Schema is fresh — schema_version sentinel must round-trip again.
        let txn = store2.db().begin_read().expect("read");
        let meta = txn.open_table(META).expect("META");
        let value = meta
            .get(META_KEY_SCHEMA_VERSION)
            .expect("get")
            .expect("present");
        let bytes: [u8; 4] = value.value().try_into().expect("4 bytes");
        assert_eq!(u32::from_le_bytes(bytes), SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_file_data_removes_file_and_its_edges() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let a = sample_file("Repo", "a.rs", None);
        let b = sample_file("Repo", "b.rs", None);
        let c = sample_file("Repo", "c.rs", None);
        let a_id = a.id.clone();

        let mut session = store.begin_index().expect("session");
        insert_file(&mut session, &a).expect("a");
        insert_file(&mut session, &b).expect("b");
        insert_file(&mut session, &c).expect("c");

        // Edges: a → b, a → c, c → a (a has both outgoing and incoming).
        insert_edge(
            &mut session,
            &Edge::new(&a.id, &b.id, EdgeKind::Imports, 100, "imp"),
        )
        .expect("a→b");
        insert_edge(
            &mut session,
            &Edge::new(&a.id, &c.id, EdgeKind::Imports, 100, "imp"),
        )
        .expect("a→c");
        insert_edge(
            &mut session,
            &Edge::new(&c.id, &a.id, EdgeKind::Imports, 100, "imp"),
        )
        .expect("c→a");
        session.commit().expect("commit");
        assert_eq!(
            edges_by_kind_limited_from(store.db(), EdgeKind::Imports, 10)
                .expect("imports by kind")
                .len(),
            3
        );

        store.delete_file_data(&a_id).expect("delete");

        // a's row is gone.
        assert!(read_node_bytes(store.db(), FILES, &a_id).is_none());
        // a's outgoing edges are gone.
        assert_eq!(collect_multimap(store.db(), EDGES_OUT, &a_id).len(), 0);
        // a's incoming edges are gone.
        assert_eq!(collect_multimap(store.db(), EDGES_IN, &a_id).len(), 0);
        // b no longer has an incoming from a.
        assert_eq!(collect_multimap(store.db(), EDGES_IN, &b.id).len(), 0);
        // c no longer has an incoming from a (the a→c edge), but DOES still
        // have its outgoing to a removed too.
        assert_eq!(collect_multimap(store.db(), EDGES_IN, &c.id).len(), 0);
        assert_eq!(collect_multimap(store.db(), EDGES_OUT, &c.id).len(), 0);
        // b and c rows themselves remain.
        assert!(read_node_bytes(store.db(), FILES, &b.id).is_some());
        assert!(read_node_bytes(store.db(), FILES, &c.id).is_some());
        assert!(
            edges_by_kind_limited_from(store.db(), EdgeKind::Imports, 10)
                .expect("imports by kind after delete")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_areas_filters_by_depth_and_sorts_stable() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");
        // Insert areas in non-sorted order to verify the sort.
        for prefix in ["src/lib", "tests", "src", "src/bin", "docs"] {
            insert_area(&mut session, &AreaNode::new("Repo", prefix, false)).expect("insert");
        }
        session.commit().expect("commit");

        let all = store.list_areas(None).expect("list all");
        let prefixes: Vec<&str> = all.iter().map(|a| a.path_prefix.as_str()).collect();
        // depth 1 areas first (alphabetical), then depth 2.
        assert_eq!(prefixes, vec!["docs", "src", "tests", "src/bin", "src/lib"]);

        let depth1 = store.list_areas(Some(1)).expect("depth 1");
        let prefixes1: Vec<&str> = depth1.iter().map(|a| a.path_prefix.as_str()).collect();
        assert_eq!(prefixes1, vec!["docs", "src", "tests"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edges_from_and_edges_to() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");
        insert_edge(
            &mut session,
            &Edge::new("file:R:a.rs", "file:R:b.rs", EdgeKind::Imports, 100, "imp"),
        )
        .expect("a→b");
        insert_edge(
            &mut session,
            &Edge::new("file:R:a.rs", "file:R:c.rs", EdgeKind::Imports, 100, "imp"),
        )
        .expect("a→c");
        insert_edge(
            &mut session,
            &Edge::new("file:R:c.rs", "file:R:b.rs", EdgeKind::Imports, 100, "imp"),
        )
        .expect("c→b");
        session.commit().expect("commit");

        let from_a = store.edges_from("file:R:a.rs").expect("from a");
        let mut targets: Vec<&str> = from_a.iter().map(|r| r.other.as_str()).collect();
        targets.sort();
        assert_eq!(targets, vec!["file:R:b.rs", "file:R:c.rs"]);

        let to_b = store.edges_to("file:R:b.rs").expect("to b");
        let mut srcs: Vec<&str> = to_b.iter().map(|r| r.other.as_str()).collect();
        srcs.sort();
        assert_eq!(srcs, vec!["file:R:a.rs", "file:R:c.rs"]);

        // Unknown id: empty, not error.
        assert!(store.edges_from("file:R:nope.rs").expect("ok").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    struct ReadApiFixture {
        root: PathBuf,
        repository: RepositoryNode,
        directory: DirectoryNode,
        file: FileNode,
        test_file: FileNode,
        class: ClassNode,
        function: FunctionNode,
        doc: DocNode,
        config: ConfigNode,
        unresolved: UnresolvedNode,
        route: SurfaceNode,
        middleware: SurfaceNode,
        credential: SurfaceNode,
        proxy: SurfaceNode,
        behavior_test: SurfaceNode,
    }

    impl ReadApiFixture {
        fn read_only(&self) -> ReadOnlyGraphStore {
            GraphStore::open_read_only(&self.root).expect("read-only")
        }
    }

    impl Drop for ReadApiFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn read_api_fixture(name: &str) -> ReadApiFixture {
        let root = tmp_root(name);
        let store = GraphStore::open(&root).expect("open");

        let repository = RepositoryNode::new("Repo", root.to_str().unwrap());
        let area = AreaNode::new("Repo", "src", false);
        let directory = sample_directory("Repo", "src", Some("area:Repo:src"));
        let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
        let test_file = sample_file("Repo", "tests/test_lib.rs", None);
        let class = sample_class(&file, "TokenLoader");
        let function = sample_function(&file, "LoadToken", Some(class.id.clone()));
        let unresolved = sample_unresolved(&file, "missing_call");
        let route = surface_node(SurfaceKind::RouteSurface, "src/lib.rs", "GET /api/token", 3);
        let middleware = surface_node(
            SurfaceKind::MiddlewareInstallation,
            "src/lib.rs",
            "TokenAuthMiddleware",
            4,
        );
        let credential = surface_node(
            SurfaceKind::CredentialOperation,
            "src/lib.rs",
            "issue token",
            18,
        );
        let proxy = surface_node(
            SurfaceKind::ProxySurface,
            "src/lib.rs",
            "https://auth.example.test",
            24,
        );
        let behavior_test = surface_node(
            SurfaceKind::BehaviorTestSurface,
            "tests/test_lib.rs",
            "test token auth",
            6,
        );
        let doc = DocNode::new(
            "Repo",
            "file:Repo:docs/auth.md",
            "docs/auth.md",
            "Auth",
            "markdown",
            Some(area.id.clone()),
        );
        let config = ConfigNode::new(
            "Repo",
            "file:Repo:pyproject.toml",
            "pyproject.toml",
            "toml",
            None,
        );

        let mut session = store.begin_index().expect("session");
        insert_repository(&mut session, &repository).expect("repository");
        insert_area(&mut session, &area).expect("area");
        insert_directory(&mut session, &directory).expect("directory");
        insert_file(&mut session, &file).expect("file");
        insert_file(&mut session, &test_file).expect("test file");
        insert_class(&mut session, &class).expect("class");
        insert_function(&mut session, &function).expect("function");
        insert_doc(&mut session, &doc).expect("doc");
        insert_config(&mut session, &config).expect("config");
        insert_unresolved(&mut session, &unresolved).expect("unresolved");
        for surface in [&route, &middleware, &credential, &proxy, &behavior_test] {
            insert_surface(&mut session, surface).expect("surface");
        }
        insert_edge(
            &mut session,
            &Edge::new(
                repository.id.as_str(),
                &area.id,
                EdgeKind::Contains,
                1000,
                "structure",
            ),
        )
        .expect("repo contains area");
        insert_edge(
            &mut session,
            &Edge::new(
                &area.id,
                &directory.id,
                EdgeKind::Contains,
                1000,
                "structure",
            ),
        )
        .expect("area contains directory");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                function.id.as_str(),
                EdgeKind::Contains,
                1000,
                "structure",
            ),
        )
        .expect("file contains function");
        insert_edge(
            &mut session,
            &Edge::new(
                function.id.as_str(),
                &doc.id,
                EdgeKind::Documents,
                900,
                "docs",
            ),
        )
        .expect("function documents doc");
        insert_edge(
            &mut session,
            &Edge::new(
                function.id.as_str(),
                &config.id,
                EdgeKind::Configures,
                900,
                "config",
            ),
        )
        .expect("function configures config");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                unresolved.id.as_str(),
                EdgeKind::Imports,
                850,
                "import",
            ),
        )
        .expect("file imports unresolved");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                &area.id,
                EdgeKind::EntrypointFor,
                800,
                "entrypoint",
            ),
        )
        .expect("entrypoint");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                route.id.as_str(),
                EdgeKind::Exposes,
                900,
                "surface-flow",
            ),
        )
        .expect("exposes route");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                middleware.id.as_str(),
                EdgeKind::InstallsMiddleware,
                900,
                "surface-flow",
            ),
        )
        .expect("installs middleware");
        insert_edge(
            &mut session,
            &Edge::new(
                route.id.as_str(),
                credential.id.as_str(),
                EdgeKind::ValidatesCredential,
                900,
                "surface-flow",
            ),
        )
        .expect("route validates credential");
        insert_edge(
            &mut session,
            &Edge::new(
                middleware.id.as_str(),
                credential.id.as_str(),
                EdgeKind::Authorizes,
                900,
                "surface-flow",
            ),
        )
        .expect("middleware authorizes");
        insert_edge(
            &mut session,
            &Edge::new(
                credential.id.as_str(),
                route.id.as_str(),
                EdgeKind::IssuesCredential,
                900,
                "surface-flow",
            ),
        )
        .expect("issues credential");
        insert_edge(
            &mut session,
            &Edge::new(
                credential.id.as_str(),
                file.id.as_str(),
                EdgeKind::StoresCredential,
                900,
                "surface-flow",
            ),
        )
        .expect("stores credential");
        insert_edge(
            &mut session,
            &Edge::new(
                middleware.id.as_str(),
                credential.id.as_str(),
                EdgeKind::UsesCredential,
                900,
                "surface-flow",
            ),
        )
        .expect("uses credential");
        insert_edge(
            &mut session,
            &Edge::new(
                route.id.as_str(),
                proxy.id.as_str(),
                EdgeKind::ForwardsTo,
                900,
                "surface-flow",
            ),
        )
        .expect("forwards to proxy");
        insert_edge(
            &mut session,
            &Edge::new(
                proxy.id.as_str(),
                credential.id.as_str(),
                EdgeKind::RewritesHeader,
                900,
                "surface-flow",
            ),
        )
        .expect("rewrites header");
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                behavior_test.id.as_str(),
                EdgeKind::TestedBy,
                900,
                "surface-flow",
            ),
        )
        .expect("tested by");
        insert_risk(
            &mut session,
            &RiskFlag::new("src/", RiskArea::SharedCore, RiskLevel::Medium, "core path"),
        )
        .expect("risk");
        session.commit().expect("commit");

        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.to_string_lossy().to_string(),
                commit_hash: Some("abc123".to_string()),
                indexed_at_unix: 1,
                file_count: 2,
                languages: vec!["rust".to_string()],
            })
            .expect("metadata");
        drop(store);

        ReadApiFixture {
            root,
            repository,
            directory,
            file,
            test_file,
            class,
            function,
            doc,
            config,
            unresolved,
            route,
            middleware,
            credential,
            proxy,
            behavior_test,
        }
    }

    #[test]
    fn read_api_get_node_resolves_typed_node_and_missing_id() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        match readonly
            .get_node(fixture.repository.id.as_str())
            .expect("repository node")
            .expect("present")
        {
            StoredNode::Repository(got) => assert_eq!(got, fixture.repository),
            other => panic!("expected repository node, got {other:?}"),
        }
        match readonly
            .get_node(fixture.directory.id.as_str())
            .expect("directory node")
            .expect("present")
        {
            StoredNode::Directory(got) => assert_eq!(got, fixture.directory),
            other => panic!("expected directory node, got {other:?}"),
        }
        match readonly
            .get_node(fixture.function.id.as_str())
            .expect("function node")
            .expect("present")
        {
            StoredNode::Function(got) => assert_eq!(got, fixture.function),
            other => panic!("expected function node, got {other:?}"),
        }
        match readonly
            .get_node(fixture.unresolved.id.as_str())
            .expect("unresolved node")
            .expect("present")
        {
            StoredNode::Unresolved(got) => assert_eq!(got, fixture.unresolved),
            other => panic!("expected unresolved node, got {other:?}"),
        }
        assert!(readonly
            .get_node("unknown:Repo:x")
            .expect("unknown")
            .is_none());
    }

    #[test]
    fn read_api_batch_display_and_area_projection() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();
        let ids = vec![
            fixture.repository.id.as_str(),
            "missing:Repo:x",
            fixture.function.id.as_str(),
        ];

        let nodes = readonly.get_nodes(&ids).expect("batch get");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id(), fixture.repository.id);
        assert_eq!(nodes[1].id(), fixture.function.id.as_str());

        let display = readonly
            .node_display(fixture.function.id.as_str())
            .expect("display")
            .expect("function display");
        assert_eq!(display.kind, StoredNodeKind::Function);
        assert_eq!(display.display, "src/lib.rs::LoadToken");
        assert_eq!(display.area_id.as_deref(), Some("area:Repo:src"));
        assert_eq!(
            readonly
                .area_for_node(fixture.function.id.as_str())
                .expect("area"),
            Some("area:Repo:src".to_string())
        );
        assert_eq!(
            readonly.area_for_node("src/lib.rs").expect("path area"),
            Some("area:Repo:src".to_string())
        );
    }

    #[test]
    fn read_api_find_symbols_is_case_insensitive_and_kind_filterable() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let symbols = readonly
            .find_symbols("LoadToken", None)
            .expect("find symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].id, fixture.function.id.to_string());
        assert_eq!(symbols[0].kind, StoredNodeKind::Function);
        assert!(readonly
            .find_symbols("LoadToken", Some(StoredNodeKind::Class))
            .expect("class filter")
            .is_empty());
        assert!(readonly
            .find_symbols("missing_call", Some(StoredNodeKind::Unresolved))
            .expect("unresolved filter")
            .is_empty());
    }

    #[test]
    fn read_api_symbols_matching_returns_bounded_signal_candidates() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let exact = readonly.symbols_matching("LoadToken").expect("exact");
        let function_exact = exact
            .iter()
            .find(|candidate| candidate.symbol.id == fixture.function.id.to_string())
            .expect("function exact candidate");
        assert!(function_exact.signals.exact);
        assert!(function_exact.signals.case_insensitive);
        assert!(function_exact.rank > 0);

        let case_insensitive = readonly.symbols_matching("loadtoken").expect("case");
        let function_case = case_insensitive
            .iter()
            .find(|candidate| candidate.symbol.id == fixture.function.id.to_string())
            .expect("function case-insensitive candidate");
        assert!(!function_case.signals.exact);
        assert!(function_case.signals.case_insensitive);

        let prefix = readonly.symbols_matching("Load").expect("prefix");
        assert!(prefix.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id.to_string() && candidate.signals.prefix
        }));

        let component = readonly.symbols_matching("token").expect("component");
        assert!(component.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id.to_string() && candidate.signals.component
        }));
        assert!(component.iter().any(|candidate| {
            candidate.symbol.id == fixture.class.id.to_string() && candidate.signals.component
        }));

        let path = readonly
            .symbols_matching_with(
                "src/",
                SymbolMatchOptions {
                    limit: 10,
                    ..SymbolMatchOptions::default()
                },
            )
            .expect("path");
        assert!(path.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id.to_string() && candidate.signals.path
        }));

        let area = readonly.symbols_matching("src").expect("area");
        assert!(area.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id.to_string() && candidate.signals.area
        }));

        let basename = readonly.symbols_matching("lib").expect("basename");
        assert!(basename.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id.to_string()
                && candidate.signals.path
                && candidate.signals.basename
        }));

        let class_only = readonly
            .symbols_matching_with(
                "token",
                SymbolMatchOptions {
                    limit: 10,
                    kind: Some(StoredNodeKind::Class),
                    ..SymbolMatchOptions::default()
                },
            )
            .expect("class kind filter");
        assert!(!class_only.is_empty());
        assert!(class_only
            .iter()
            .all(|candidate| candidate.symbol.kind == StoredNodeKind::Class));
    }

    #[test]
    fn read_api_symbols_matching_collects_stem_component_candidates() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let area = AreaNode::new("Repo", "includes", false);
        let file = sample_file(
            "Repo",
            "includes/Page/WikiPage.php",
            Some("area:Repo:includes"),
        );
        let function = sample_function(&file, "doViewUpdates", None);

        let mut session = store.begin_index().expect("session");
        insert_area(&mut session, &area).expect("area");
        insert_file(&mut session, &file).expect("file");
        insert_function(&mut session, &function).expect("function");
        session.commit().expect("commit");
        drop(store);

        let readonly = GraphStore::open_read_only(&root).expect("read-only");
        let hits = readonly
            .symbols_matching("viewing page")
            .expect("stem symbol match");
        let hit = hits
            .iter()
            .find(|candidate| candidate.symbol.id == function.id.to_string())
            .expect("doViewUpdates should be recalled via the view/viewing stem");
        assert!(hit.signals.component);
        assert!(hit.rank > 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_api_nodes_under_path_returns_typed_path_rows() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let nodes = readonly.nodes_under_path("src/").expect("nodes under src");
        let node_ids: BTreeSet<String> = nodes.iter().map(|node| node.id().to_string()).collect();
        assert!(node_ids.contains(&fixture.file.id));
        assert!(node_ids.contains(fixture.function.id.as_str()));
        assert!(node_ids.contains(fixture.class.id.as_str()));
        assert!(node_ids.contains(fixture.unresolved.id.as_str()));
        assert!(!node_ids.contains(&fixture.test_file.id));

        let src_nodes = readonly.nodes_under_path("src").expect("nodes under src");
        let src_ids: BTreeSet<String> =
            src_nodes.iter().map(|node| node.id().to_string()).collect();
        assert!(src_ids.contains(&fixture.directory.id));
    }

    #[test]
    fn read_api_functions_under_path_returns_function_rows() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let functions = readonly
            .functions_under_path("src/")
            .expect("functions under src");
        assert_eq!(functions, vec![fixture.function.clone()]);
    }

    #[test]
    fn read_api_resolve_file_path_is_exact() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let resolved = readonly
            .resolve_file_path("src/lib.rs")
            .expect("resolve file")
            .expect("present");
        assert_eq!(resolved, fixture.file);
        assert!(readonly
            .resolve_file_path("src")
            .expect("prefix is not exact")
            .is_none());
        assert!(readonly
            .resolve_file_path("src/missing.rs")
            .expect("missing")
            .is_none());
    }

    #[test]
    fn read_api_neighbors_filters_direction_and_kind() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let incoming = readonly
            .neighbors(
                fixture.function.id.as_str(),
                NeighborDirection::Incoming,
                Some(EdgeKind::Contains),
            )
            .expect("incoming");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].other.as_str(), fixture.file.id.as_str());

        let outgoing = readonly
            .neighbors(
                fixture.function.id.as_str(),
                NeighborDirection::Outgoing,
                Some(EdgeKind::Configures),
            )
            .expect("outgoing");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].other.as_str(), fixture.config.id.as_str());
        let unresolved_imports = readonly
            .neighbors(
                &fixture.file.id,
                NeighborDirection::Outgoing,
                Some(EdgeKind::Imports),
            )
            .expect("unresolved imports");
        assert_eq!(unresolved_imports.len(), 1);
        assert_eq!(
            unresolved_imports[0].other.as_str(),
            fixture.unresolved.id.as_str()
        );
        assert!(readonly
            .neighbors(
                fixture.function.id.as_str(),
                NeighborDirection::Outgoing,
                Some(EdgeKind::Imports),
            )
            .expect("wrong kind")
            .is_empty());
    }

    #[test]
    fn read_api_relation_docs_configs_and_risks() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let children = readonly
            .children(&fixture.file.id, Some(StoredNodeKind::Function))
            .expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, fixture.function.id.to_string());

        let parents = readonly
            .parents(fixture.function.id.as_str(), Some(StoredNodeKind::File))
            .expect("parents");
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].id, fixture.file.id);

        let config_view = readonly
            .relation_view(fixture.function.id.as_str(), GraphRelation::Configs)
            .expect("config relation");
        assert_eq!(
            config_view.target.as_ref().map(|node| node.id.as_str()),
            Some(fixture.function.id.as_str())
        );
        assert_eq!(config_view.items.len(), 1);
        assert_eq!(config_view.items[0].node.id, fixture.config.id);

        let docs = readonly
            .docs_for(fixture.function.id.as_str())
            .expect("docs");
        assert_eq!(docs, vec![fixture.doc.clone()]);
        let configs = readonly
            .configs_for(fixture.function.id.as_str())
            .expect("configs");
        assert_eq!(configs, vec![fixture.config.clone()]);

        let risks = readonly
            .risk_for_node_or_path(fixture.function.id.as_str())
            .expect("risks");
        assert_eq!(risks.len(), 1);
        assert_eq!(risks[0].scope, "src/");
    }

    #[test]
    fn read_api_task_anchor_and_usage_boundary_candidates_are_bounded() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let anchors = readonly
            .task_anchor_candidates(&["token", "src"], 5)
            .expect("anchors");
        let function_anchor = anchors
            .iter()
            .find(|candidate| candidate.node.id == fixture.function.id.to_string())
            .expect("function anchor");
        assert!(function_anchor
            .matched_tokens
            .contains(&"token".to_string()));
        assert!(function_anchor.matched_tokens.contains(&"src".to_string()));
        assert!(anchors.len() <= 5);

        let usage = readonly
            .usage_boundary_candidates("src/", Some(StoredNodeKind::Function), 5)
            .expect("usage");
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].node.id, fixture.function.id.to_string());
        assert_eq!(
            usage[0].symbol.as_ref().map(|symbol| symbol.name.as_str()),
            Some("LoadToken")
        );
    }

    #[test]
    fn read_api_bounded_surface_flow_candidates_use_edge_kind_index() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let entrypoints = readonly
            .entrypoints_for_task(&["token"])
            .expect("entrypoints");
        let route = entrypoints
            .iter()
            .find(|candidate| candidate.node.id == fixture.route.id.to_string())
            .expect("route entrypoint");
        assert!(route.relation_kinds.contains(&EdgeKind::Exposes));
        assert!(entrypoints.len() <= FLOW_QUERY_LIMIT);

        let paths = readonly
            .surface_paths_for_behavior(&["token"])
            .expect("surface paths");
        let src_path = paths
            .iter()
            .find(|candidate| candidate.path == "src/lib.rs")
            .expect("src/lib.rs surface path");
        assert!(src_path
            .surfaces
            .iter()
            .any(|surface| surface.id == fixture.route.id.to_string()));
        assert!(src_path
            .relation_kinds
            .iter()
            .any(|kind| matches!(kind, EdgeKind::Exposes | EdgeKind::ValidatesCredential)));

        let credential = readonly
            .credential_flow_candidates(&["token"])
            .expect("credential flows");
        let credential_candidate = credential
            .iter()
            .find(|candidate| candidate.node.id == fixture.credential.id.to_string())
            .expect("credential operation");
        assert!(credential_candidate
            .relation_kinds
            .contains(&EdgeKind::IssuesCredential));
        assert!(credential_candidate
            .relation_kinds
            .contains(&EdgeKind::StoresCredential));

        let subsystems = readonly
            .subsystems_matching(&["token"])
            .expect("subsystems");
        assert!(subsystems
            .iter()
            .any(|candidate| candidate.path_prefix == "src"));
    }

    #[test]
    fn read_api_flow_chains_tests_and_coverage_are_bounded() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let middleware = readonly
            .middleware_chain_for_route(fixture.route.id.as_str())
            .expect("middleware chain");
        assert!(middleware
            .roots
            .iter()
            .any(|root| root.id == fixture.route.id.to_string()));
        assert!(middleware
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::InstallsMiddleware
                && step.to.id == fixture.middleware.id.to_string()));
        assert!(middleware
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::ValidatesCredential
                && step.to.id == fixture.credential.id.to_string()));
        assert!(middleware.steps.len() <= FLOW_CHAIN_STEP_LIMIT);

        let forwarding = readonly
            .forwarding_chain_for_surface(fixture.route.id.as_str())
            .expect("forwarding chain");
        assert!(forwarding
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::ForwardsTo
                && step.to.id == fixture.proxy.id.to_string()));
        assert!(forwarding
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::RewritesHeader
                && step.to.id == fixture.credential.id.to_string()));

        let tests = readonly
            .tests_for_surface_or_symbol(fixture.function.id.as_str())
            .expect("tests for symbol");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].id, fixture.behavior_test.id.to_string());

        let coverage = readonly
            .coverage_for_task_class("token auth behavior")
            .expect("task-class coverage");
        assert!(!coverage.entrypoints.is_empty());
        assert!(!coverage.surface_paths.is_empty());
        assert!(!coverage.credential_flows.is_empty());
        assert_eq!(coverage.tests.len(), 1);
        assert!(!coverage.missing.contains(&"credential_flows".to_string()));
    }

    #[test]
    fn read_api_overview_v2_returns_bounded_navigation_slice() {
        let fixture = read_api_fixture(function_name!());
        let readonly = fixture.read_only();

        let overview = readonly
            .overview_v2(OverviewV2Limits {
                directory_limit: 10,
                file_limit: 10,
                function_limit: 10,
                class_limit: 10,
                doc_limit: 10,
                config_limit: 10,
                unresolved_limit: 10,
                ..OverviewV2Limits::default()
            })
            .expect("overview v2");
        assert_eq!(overview.repo.as_ref().unwrap().file_count, 2);
        assert_eq!(overview.repository, Some(fixture.repository.clone()));
        assert_eq!(overview.directories, vec![fixture.directory.clone()]);
        assert_eq!(overview.entrypoint_paths, vec!["src/lib.rs".to_string()]);
        assert_eq!(overview.files.len(), 2);
        assert_eq!(overview.functions, vec![fixture.function.clone()]);
        assert_eq!(overview.classes, vec![fixture.class.clone()]);
        assert_eq!(overview.docs, vec![fixture.doc.clone()]);
        assert_eq!(overview.configs, vec![fixture.config.clone()]);
        assert_eq!(overview.unresolved, vec![fixture.unresolved.clone()]);
    }

    #[test]
    fn overview_assembles_repo_areas_entrypoints_risks() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");

        let mut session = store.begin_index().expect("session");
        // 2 top-level areas + 1 nested.
        insert_area(&mut session, &AreaNode::new("R", "src", false)).expect("src");
        insert_area(&mut session, &AreaNode::new("R", "docs", false)).expect("docs");
        insert_area(&mut session, &AreaNode::new("R", "src/bin", false)).expect("nest");

        // Two files. main.rs is an entrypoint via an EntrypointFor edge.
        let main = sample_file("R", "src/main.rs", Some("area:R:src"));
        let lib = sample_file("R", "src/lib.rs", Some("area:R:src"));
        insert_file(&mut session, &main).expect("main");
        insert_file(&mut session, &lib).expect("lib");
        insert_edge(
            &mut session,
            &Edge::new(&main.id, "area:R:src", EdgeKind::EntrypointFor, 100, "ep"),
        )
        .expect("entrypoint");

        // Risks: one Low and one High. High should sort first.
        insert_risk(
            &mut session,
            &RiskFlag::new("low/", RiskArea::Auth, RiskLevel::Low, "minor"),
        )
        .expect("low");
        insert_risk(
            &mut session,
            &RiskFlag::new("high/", RiskArea::Secrets, RiskLevel::High, "secret"),
        )
        .expect("high");
        session.commit().expect("commit");

        store
            .set_repo_metadata(&RepoMetadata {
                root_path: "/tmp/r".into(),
                commit_hash: None,
                indexed_at_unix: 0,
                file_count: 2,
                languages: vec!["rust".into()],
            })
            .expect("meta");

        let ov = store.overview(20, 10, 20).expect("overview");
        assert_eq!(ov.repo.as_ref().unwrap().file_count, 2);

        // Only depth-1 areas in the overview.
        let area_prefixes: Vec<&str> = ov.areas.iter().map(|a| a.path_prefix.as_str()).collect();
        assert_eq!(area_prefixes, vec!["docs", "src"]);

        // Entrypoint resolves the file_id back to its path.
        assert_eq!(ov.entrypoint_paths, vec!["src/main.rs".to_string()]);

        // Risks sorted High-first.
        assert_eq!(ov.risks.len(), 2);
        assert_eq!(ov.risks[0].level, RiskLevel::High);
        assert_eq!(ov.risks[1].level, RiskLevel::Low);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overview_respects_limits() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        let mut session = store.begin_index().expect("session");
        for i in 0..5 {
            insert_area(&mut session, &AreaNode::new("R", &format!("a{i}"), false)).expect("area");
            insert_risk(
                &mut session,
                &RiskFlag::new(format!("r{i}"), RiskArea::Auth, RiskLevel::Low, "x"),
            )
            .expect("risk");
        }
        session.commit().expect("commit");

        let ov = store.overview(2, 10, 3).expect("overview");
        assert_eq!(ov.areas.len(), 2);
        assert_eq!(ov.risks.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_mismatch_is_reported() {
        let root = tmp_root(function_name!());
        // Open once at the real version.
        let _ = GraphStore::open(&root).expect("open");

        // Stomp the sentinel to a different version, then reopen.
        {
            let db_path = root.join(".aethyme").join(DB_FILE_NAME);
            let db = Database::create(&db_path).expect("reopen raw");
            let txn = db.begin_write().expect("write txn");
            {
                let mut meta = txn.open_table(META).expect("META");
                meta.insert(META_KEY_SCHEMA_VERSION, &999u32.to_le_bytes()[..])
                    .expect("stomp");
            }
            txn.commit().expect("commit");
        }

        match GraphStore::open(&root) {
            Ok(_) => panic!("expected SchemaMismatch, got Ok"),
            Err(GraphStoreError::SchemaMismatch { found, expected }) => {
                assert_eq!(found, 999);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            Err(other) => panic!("expected SchemaMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn incompatible_redb_file_format_is_reported() {
        let root = tmp_root(function_name!());
        let store = GraphStore::open(&root).expect("open");
        drop(store);
        mark_graph_store_as_redb_v2(&root);

        match GraphStore::open(&root) {
            Ok(_) => panic!("expected IncompatibleRedbFileFormat, got Ok"),
            Err(GraphStoreError::IncompatibleRedbFileFormat { path, found }) => {
                assert_eq!(found, 2);
                assert_eq!(path, root.join(".aethyme").join(DB_FILE_NAME));
            }
            Err(other) => panic!("expected IncompatibleRedbFileFormat, got {other:?}"),
        }

        let message = match GraphStore::open(&root) {
            Ok(_) => panic!("expected old redb format to fail"),
            Err(err) => err.to_string(),
        };
        assert!(message.contains("aethyme-engine-cli index --repo <repo>"));
        assert!(message.contains(".aethyme/graph/"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reset_replaces_incompatible_graph_store_without_touching_fragments() {
        let root = tmp_root(function_name!());
        let fragment_marker = root.join(".aethyme/graph/fragments.marker");
        std::fs::create_dir_all(fragment_marker.parent().unwrap()).expect("fragment dir");
        std::fs::write(&fragment_marker, b"source-of-truth").expect("fragment marker");

        let store = GraphStore::open(&root).expect("open");
        drop(store);
        mark_graph_store_as_redb_v2(&root);

        let incompatible =
            GraphStore::detect_incompatible_file_format(&root).expect("old redb format detected");
        assert_eq!(incompatible.found_redb_format, 2);

        let rebuilt = GraphStore::reset(&root).expect("reset");
        assert!(rebuilt.path().exists(), "graph_store.redb recreated");
        assert_eq!(
            std::fs::read(&fragment_marker).expect("fragment marker"),
            b"source-of-truth"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
