//! redb-backed graph store for the Aethyme engine.
//!
//! Replaces `super::super::GraphStore` (SurrealDB). See
//! `docs/architecture/phase3-redb-graph-store-plan.md` for context.
//!
//! Phase 3.1 (this file): schema, error type, `open()`, schema-version
//! sentinel. Insert / query APIs land in 3.2–3.4.

use std::path::{Path, PathBuf};

use redb::{Database, MultimapTableDefinition, ReadableTable, TableDefinition};

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
    use redb::ReadableTableMetadata;

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
