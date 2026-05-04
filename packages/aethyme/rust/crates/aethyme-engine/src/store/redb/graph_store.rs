//! redb-backed graph store for the Aethyme engine.
//!
//! Replaces `super::super::GraphStore` (SurrealDB). See
//! `docs/architecture/phase3-redb-graph-store-plan.md` for context.
//!
//! Phase 3.1 (this file): schema, error type, `open()`, schema-version
//! sentinel. Insert / query APIs land in 3.2–3.4.

use std::path::{Path, PathBuf};

use redb::{
    Database, MultimapTableDefinition, ReadableMultimapTable, ReadableTable, TableDefinition,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};

use crate::model::area::AreaNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::model::file::FileNode;
use crate::model::intern::InternedStr;
use crate::model::risk::RiskFlag;

/// Bumped when the on-disk format changes incompatibly. We re-create the file
/// rather than try to migrate.
const SCHEMA_VERSION: u32 = 1;

/// Single-row metadata table: schema version, build timestamps, repo root, ...
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_KEY_SCHEMA_VERSION: &str = "schema_version";

// ── Node tables ─────────────────────────────────────────────────────────────
// One table per kind keeps tablespaces separate so prefix-range scans on
// `path/` don't have to skip over unrelated kinds. Key = node id (raw &str so
// scope queries can range over it). Value = bincoded entity record.

const FILES:     TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const AREAS:     TableDefinition<&str, &[u8]> = TableDefinition::new("areas");
const FUNCTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("functions");
const CLASSES:   TableDefinition<&str, &[u8]> = TableDefinition::new("classes");
const DOCS:      TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
const CONFIGS:   TableDefinition<&str, &[u8]> = TableDefinition::new("configs");

// ── Adjacency (the wedge for ego/impact/dead-code queries) ──────────────────
// Both directions are first-class (informed by the `edges_by_target`
// algorithmic fix that turned MediaWiki dead-code from O(F·E) to O(F·in_deg)).
// Value = bincoded AdjacencyRecord (kind, other_node_id, confidence, source).

const EDGES_OUT: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("edges_out");
const EDGES_IN: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("edges_in");

// ── Scope-bounded lookups (raw paths give free prefix range reads) ──────────
// Key = file_path. Value = node id. A range scan from "includes/" to
// "includes/\xff" yields all symbols under that scope.

const FUNCTIONS_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("functions_by_path");
const NODES_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("nodes_by_path");

// ── Symbol search ───────────────────────────────────────────────────────────
// Key = lowercased name. Value = node id.

const SYMBOL_BY_NAME: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_name");

// ── Risk overlays ───────────────────────────────────────────────────────────

const RISK_FLAGS: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("risk_flags");

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

#[derive(Debug)]
pub enum GraphStoreError {
    Io(std::io::Error),
    Db(redb::Error),
    Encode(bincode::Error),
    SchemaMismatch { found: u32, expected: u32 },
}

impl std::fmt::Display for GraphStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Db(e) => write!(f, "redb: {e}"),
            Self::Encode(e) => write!(f, "bincode: {e}"),
            Self::SchemaMismatch { found, expected } => {
                write!(f, "graph store schema mismatch: found v{found}, expected v{expected}")
            }
        }
    }
}

impl std::error::Error for GraphStoreError {}

impl From<std::io::Error> for GraphStoreError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
impl From<redb::Error> for GraphStoreError {
    fn from(e: redb::Error) -> Self { Self::Db(e) }
}
impl From<redb::DatabaseError> for GraphStoreError {
    fn from(e: redb::DatabaseError) -> Self { Self::Db(e.into()) }
}
impl From<redb::TransactionError> for GraphStoreError {
    fn from(e: redb::TransactionError) -> Self { Self::Db(e.into()) }
}
impl From<redb::TableError> for GraphStoreError {
    fn from(e: redb::TableError) -> Self { Self::Db(e.into()) }
}
impl From<redb::StorageError> for GraphStoreError {
    fn from(e: redb::StorageError) -> Self { Self::Db(e.into()) }
}
impl From<redb::CommitError> for GraphStoreError {
    fn from(e: redb::CommitError) -> Self { Self::Db(e.into()) }
}
impl From<bincode::Error> for GraphStoreError {
    fn from(e: bincode::Error) -> Self { Self::Encode(e) }
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

const DB_FILE_NAME: &str = "graph_store.redb";

impl GraphStore {
    /// Open or create the graph store for a repository. Verifies / writes the
    /// schema version sentinel and ensures every table exists so downstream
    /// reads on a fresh DB don't trip on `TableDoesNotExist`.
    pub fn open(repo_root: &Path) -> Result<Self, GraphStoreError> {
        let dir = repo_root.join(".aethyme");
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join(DB_FILE_NAME);
        let db = Database::create(&db_path)?;
        ensure_schema(&db)?;
        Ok(Self { db, db_path })
    }

    /// Path to the DB file on disk.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Borrow the underlying redb `Database` — used by the build session and
    /// query primitives that land in 3.2–3.4.
    #[allow(dead_code)]
    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    /// Open a Variant-B index session. The session holds one open
    /// `WriteTransaction` and rotates it periodically based on
    /// `IndexSession::should_rotate`. Drop without `commit()` aborts.
    pub fn begin_index(&self) -> Result<IndexSession<'_>, GraphStoreError> {
        let txn = self.db.begin_write()?;
        Ok(IndexSession {
            db: &self.db,
            txn: Some(txn),
            ops_since_rotate: 0,
            bytes_since_rotate: 0,
        })
    }

    /// Drop all data and re-apply the schema. Mirrors the SurrealDB version's
    /// `reset()`, used at the start of every full index pass. Implemented as
    /// "delete the file, recreate it" — cheaper and simpler than range-deleting
    /// every table.
    pub fn reset(repo_root: &Path) -> Result<Self, GraphStoreError> {
        let db_path = repo_root.join(".aethyme").join(DB_FILE_NAME);
        if db_path.exists() {
            std::fs::remove_file(&db_path)?;
        }
        Self::open(repo_root)
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
        let txn = self.db.begin_read()?;
        let t = txn.open_table(META)?;
        let Some(value) = t.get(META_KEY_REPO_METADATA)? else { return Ok(None) };
        let meta: RepoMetadata = bincode::deserialize(value.value())?;
        Ok(Some(meta))
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

            // The file row itself.
            let mut files = txn.open_table(FILES)?;
            files.remove(file_id)?;
        }
        txn.commit()?;
        Ok(())
    }
}

// ── Typed write wrappers ────────────────────────────────────────────────────
// Thin shims over IndexSession primitives. The mapping is structured ID →
// raw redb key (no sanitization), entity → bincoded value, plus the secondary
// indexes each kind requires. Mirrors the surface of `super::super::write`.

/// Insert (or overwrite) an area. Key = `area.id` (e.g. `area:Repo:src`).
pub fn insert_area(
    session: &mut IndexSession<'_>,
    area: &AreaNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(AREAS, &area.id, area)
}

/// Insert (or overwrite) a file. Key = `file.id` (e.g. `file:Repo:src/lib.rs`).
/// Also adds `path → file.id` to NODES_BY_PATH so a path can be resolved to
/// the structured ID.
pub fn insert_file(
    session: &mut IndexSession<'_>,
    file: &FileNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(FILES, &file.id, file)?;
    session.add_path_index(NODES_BY_PATH, &file.path, &file.id)?;
    Ok(())
}

/// Insert one logical edge. Composes into `IndexSession::insert_edge` with
/// `Edge`'s fields unpacked.
pub fn insert_edge(
    session: &mut IndexSession<'_>,
    edge: &Edge,
) -> Result<(), GraphStoreError> {
    session.insert_edge(
        edge.from.as_str(),
        edge.to.as_str(),
        edge.kind.clone(),
        edge.confidence,
        edge.source.clone(),
    )
}

/// Insert a risk flag under its scope.
pub fn insert_risk(
    session: &mut IndexSession<'_>,
    risk: &RiskFlag,
) -> Result<(), GraphStoreError> {
    session.add_risk(&risk.scope, risk)
}

/// One open redb write transaction that accepts many node/edge inserts and
/// commits/rotates based on a policy. Mirrors `parse_store::BuildSession`.
///
/// "Op" counts every primary-table or adjacency insert; secondary-index
/// updates are folded into the parent op (one logical edge insert =
/// 2 adjacency rows, counted as 1 op). Bytes count actual bincode payload.
pub struct IndexSession<'db> {
    db: &'db Database,
    txn: Option<WriteTransaction>,
    ops_since_rotate: usize,
    bytes_since_rotate: usize,
}

impl<'db> IndexSession<'db> {
    /// Insert (or overwrite) a node row in the given primary table.
    /// Caller picks the table; 3.3 will provide typed wrappers
    /// (`insert_file`, `insert_function`, …) on top of this primitive.
    pub fn insert_node(
        &mut self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &impl Serialize,
    ) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self.txn.as_ref().expect("IndexSession invariant: txn present");
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
        let out_bytes = bincode::serialize(&out_record)?;
        let in_bytes = bincode::serialize(&in_record)?;
        let written = out_bytes.len() + in_bytes.len();
        {
            let txn = self.txn.as_ref().expect("IndexSession invariant: txn present");
            let mut out = txn.open_multimap_table(EDGES_OUT)?;
            out.insert(src, out_bytes.as_slice())?;
            let mut inv = txn.open_multimap_table(EDGES_IN)?;
            inv.insert(dst, in_bytes.as_slice())?;
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
        let txn = self.txn.as_ref().expect("IndexSession invariant: txn present");
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
        let txn = self.txn.as_ref().expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        t.insert(name_lower, node_id)?;
        Ok(())
    }

    /// Append a bincoded risk record under `scope` in RISK_FLAGS.
    pub fn add_risk(
        &mut self,
        scope: &str,
        value: &impl Serialize,
    ) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self.txn.as_ref().expect("IndexSession invariant: txn present");
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
        let txn = self.txn.take().expect("IndexSession invariant: txn present");
        txn.commit()?;
        Ok(())
    }

    /// Force a rotation now (commit current txn, open a fresh one). Useful at
    /// natural pipeline boundaries (e.g. between indexing passes).
    pub fn rotate(&mut self) -> Result<(), GraphStoreError> {
        let txn = self.txn.take().expect("IndexSession invariant: txn present");
        txn.commit()?;
        self.txn = Some(self.db.begin_write()?);
        self.ops_since_rotate = 0;
        self.bytes_since_rotate = 0;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ⚠ TODO(daisy): YOUR DECISION — rotate policy thresholds
    // ─────────────────────────────────────────────────────────────────────────
    //
    // Returning `true` after an insert triggers commit + fresh transaction.
    // Returning `false` keeps batching.
    //
    // Workload differs from parse_store: graph_store sees many small writes
    // (one node = one row, one edge = two adjacency rows). On MediaWiki the
    // ballpark is ~25k files + ~80k functions + ~10k classes + ~1M edges =
    // O(1M) ops. Per-op payload is small (tens to a few hundred bytes).
    //
    // Three counter shapes to consider:
    //
    //   • OPS-only: `ops_since_rotate >= N`. Predictable rotate cadence,
    //     ignores payload heterogeneity. Good when you trust ops to be
    //     uniformly small.
    //   • BYTES-only: `bytes_since_rotate >= N`. Bounds dirty-page memory
    //     directly, which is the actual scarce resource. But on tiny-row
    //     workloads, a 16 MiB threshold could mean 200k+ ops in one txn —
    //     long fsync stall when it finally rotates.
    //   • HYBRID (parse_store style): rotate on either threshold. Caps both.
    //
    // Constants today: ROTATE_EVERY_OPS = 4096, ROTATE_EVERY_BYTES = 8 MiB.
    // For ~1M ops on MediaWiki that's ~244 commits, ~244 ms of fsync
    // overhead — tolerable but tunable. Adjust the consts at module top.
    //
    // Constraints to respect:
    //   - `ops_since_rotate` and `bytes_since_rotate` reset on rotate.
    //   - returning `true` is ALWAYS safe (just slower); returning `false`
    //     too aggressively risks unbounded memory growth on big repos.
    fn should_rotate(&self) -> bool {
        self.ops_since_rotate >= ROTATE_EVERY_OPS
            || self.bytes_since_rotate >= ROTATE_EVERY_BYTES
    }
}

fn ensure_schema(db: &Database) -> Result<(), GraphStoreError> {
    let txn = db.begin_write()?;
    {
        // Schema-version check. Same shape as parse_store: read existing into
        // an owned [u8;4], release the borrow, then write only if absent.
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
                meta.insert(
                    META_KEY_SCHEMA_VERSION,
                    &SCHEMA_VERSION.to_le_bytes()[..],
                )?;
            }
        }
        // Touch every table so they exist on a fresh DB.
        let _ = txn.open_table(FILES)?;
        let _ = txn.open_table(AREAS)?;
        let _ = txn.open_table(FUNCTIONS)?;
        let _ = txn.open_table(CLASSES)?;
        let _ = txn.open_table(DOCS)?;
        let _ = txn.open_table(CONFIGS)?;
        let _ = txn.open_multimap_table(EDGES_OUT)?;
        let _ = txn.open_multimap_table(EDGES_IN)?;
        let _ = txn.open_multimap_table(FUNCTIONS_BY_PATH)?;
        let _ = txn.open_multimap_table(NODES_BY_PATH)?;
        let _ = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        let _ = txn.open_multimap_table(RISK_FLAGS)?;
    }
    txn.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::{ReadableMultimapTable, ReadableTableMetadata};
    use serde::Deserialize;

    macro_rules! function_name {
        () => {{
            fn f() {}
            fn type_name<T>(_: T) -> &'static str { std::any::type_name::<T>() }
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
        txn.open_table(FILES).expect("FILES");
        txn.open_table(AREAS).expect("AREAS");
        txn.open_table(FUNCTIONS).expect("FUNCTIONS");
        txn.open_table(CLASSES).expect("CLASSES");
        txn.open_table(DOCS).expect("DOCS");
        txn.open_table(CONFIGS).expect("CONFIGS");
        txn.open_table(META).expect("META");

        // Multimap tables.
        let edges_out = txn.open_multimap_table(EDGES_OUT).expect("EDGES_OUT");
        let edges_in = txn.open_multimap_table(EDGES_IN).expect("EDGES_IN");
        assert_eq!(edges_out.len().unwrap(), 0);
        assert_eq!(edges_in.len().unwrap(), 0);
        txn.open_multimap_table(FUNCTIONS_BY_PATH).expect("FUNCTIONS_BY_PATH");
        txn.open_multimap_table(NODES_BY_PATH).expect("NODES_BY_PATH");
        txn.open_multimap_table(SYMBOL_BY_NAME).expect("SYMBOL_BY_NAME");
        txn.open_multimap_table(RISK_FLAGS).expect("RISK_FLAGS");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_version_is_persisted() {
        let root = tmp_root(function_name!());
        let _ = GraphStore::open(&root).expect("open");
        // Reopen and confirm the sentinel reads back as v1.
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

        let bytes = read_node_bytes(store.db(), FILES, "file:Repo:src/lib.rs")
            .expect("present");
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

        let a = SampleNode { id: "a".into(), path: "a.rs".into() };
        session.insert_node(FILES, &a.id, &a).expect("insert a");
        session.rotate().expect("rotate");

        // After rotate, `a` is durable; subsequent inserts continue in a fresh txn.
        assert!(read_node_bytes(store.db(), FILES, "a").is_some(), "a is durable");

        let b = SampleNode { id: "b".into(), path: "b.rs".into() };
        session.insert_node(FILES, &b.id, &b).expect("insert b");
        session.commit().expect("commit");

        assert!(read_node_bytes(store.db(), FILES, "b").is_some(), "b after second commit");
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
        let by_path = txn.open_multimap_table(NODES_BY_PATH).expect("NODES_BY_PATH");
        let hits: Vec<String> = by_path
            .get("src/lib.rs")
            .expect("get")
            .map(|r| r.expect("row").value().to_string())
            .collect();
        assert_eq!(hits, vec![id]);
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
}
