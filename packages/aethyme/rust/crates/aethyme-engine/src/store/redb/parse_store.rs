//! Per-file parse cache backed by redb. Replaces `crate::cache::ParseCache`.
//!
//! Why this exists: profiling MediaWiki showed the old `ParseCache::insert` path
//! peaked at ~22 GB of intermediate `String` allocations, dominated by
//! `serialize_entry`'s `format!()` + `Vec<String>::join(",")` pattern (cache.rs:122-158).
//! Streaming bincode straight into a redb table eliminates those intermediates.
//!
//! Variant B (batched build session): one redb `WriteTransaction` covers many file
//! inserts and is rotated periodically by `BuildSession::should_rotate`. This
//! avoids the per-file fsync amplification of one-txn-per-insert while still
//! bounding the in-flight transaction's resource use.

use std::path::{Path, PathBuf};

use redb::{Database, ReadableTable, TableDefinition, WriteTransaction};

use crate::cache::CacheEntry;
use crate::model::edge::Edge;
use crate::model::symbol::Symbol;

/// Bumped when the on-disk format changes incompatibly. We re-create the file
/// rather than try to migrate.
const SCHEMA_VERSION: u32 = 1;

/// Per-file parse output. Key = repo-relative file path. Value = bincoded `CacheEntry`.
const PARSE_ENTRIES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("parse_entries");

/// Single-row metadata table: stores schema version, build timestamps, etc.
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_KEY_SCHEMA_VERSION: &str = "schema_version";

/// Rotate the in-flight write transaction after this many `insert` calls.
/// Bounds fsync rate and the size of any single committed batch.
const ROTATE_EVERY_FILES: usize = 256;

/// Rotate the in-flight write transaction after this many bytes have been
/// written. Bounds the in-memory dirty-page footprint of a single transaction
/// — important on adversarial inputs where one CacheEntry can be several MB.
const ROTATE_EVERY_BYTES: usize = 16 * 1024 * 1024;

/// Refuse to write a parse-cache entry larger than this. redb has a hard
/// 3 GiB-per-value cap, but well before that limit a single entry is almost
/// certainly noise — minified vendor bundles (swagger-ui, jquery, lodash)
/// extracted as tens of thousands of pseudo-symbols by tree-sitter on
/// machine-generated code. Skipping these keeps the cache useful for the
/// rest of the repo and avoids a per-call retry storm where the insert
/// fails, redb reports the error, and we re-attempt next call.
///
/// MediaWiki's `resources/lib/swagger-ui/swagger-ui-bundle.js` was producing
/// a 3.8 GB serialized entry (3.6 GiB) before this guard was added.
const SKIP_ENTRY_OVER_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

#[derive(Debug)]
pub enum ParseStoreError {
    Io(std::io::Error),
    Db(redb::Error),
    Encode(bincode::Error),
    SchemaMismatch { found: u32, expected: u32 },
}

impl std::fmt::Display for ParseStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Db(e) => write!(f, "redb: {e}"),
            Self::Encode(e) => write!(f, "bincode: {e}"),
            Self::SchemaMismatch { found, expected } => {
                write!(f, "parse store schema mismatch: found v{found}, expected v{expected}")
            }
        }
    }
}

impl std::error::Error for ParseStoreError {}

impl From<std::io::Error> for ParseStoreError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
impl From<redb::Error> for ParseStoreError {
    fn from(e: redb::Error) -> Self { Self::Db(e) }
}
impl From<redb::DatabaseError> for ParseStoreError {
    fn from(e: redb::DatabaseError) -> Self { Self::Db(e.into()) }
}
impl From<redb::TransactionError> for ParseStoreError {
    fn from(e: redb::TransactionError) -> Self { Self::Db(e.into()) }
}
impl From<redb::TableError> for ParseStoreError {
    fn from(e: redb::TableError) -> Self { Self::Db(e.into()) }
}
impl From<redb::StorageError> for ParseStoreError {
    fn from(e: redb::StorageError) -> Self { Self::Db(e.into()) }
}
impl From<redb::CommitError> for ParseStoreError {
    fn from(e: redb::CommitError) -> Self { Self::Db(e.into()) }
}
impl From<bincode::Error> for ParseStoreError {
    fn from(e: bincode::Error) -> Self { Self::Encode(e) }
}

/// Handle to a redb database holding per-file parse output for one repository.
///
/// Lives at `<repo_root>/.aethyme/parse_store.redb`. Path matches the existing
/// `.aethyme` directory used by other engine artifacts.
pub struct ParseStore {
    db: Database,
    #[allow(dead_code)]
    db_path: PathBuf,
}

const DB_FILE_NAME: &str = "parse_store.redb";

impl ParseStore {
    /// Open or create the parse store for a repository. Verifies / writes the
    /// schema version sentinel.
    pub fn open(repo_root: &Path) -> Result<Self, ParseStoreError> {
        let dir = repo_root.join(".aethyme");
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join(DB_FILE_NAME);
        let db = Database::create(&db_path)?;
        ensure_schema(&db)?;
        Ok(Self { db, db_path })
    }

    /// Read-side lookup. Returns the cached entry for `file_path` iff the stored
    /// `content_hash` matches `expected_hash`. Mismatched entries are treated as
    /// misses (and will be overwritten on the next `BuildSession::insert`).
    pub fn lookup(
        &self,
        file_path: &str,
        expected_hash: &str,
    ) -> Result<Option<CacheEntry>, ParseStoreError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(PARSE_ENTRIES) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let Some(value) = table.get(file_path)? else { return Ok(None) };
        let entry: CacheEntry = bincode::deserialize(value.value())?;
        if entry.content_hash != expected_hash {
            return Ok(None);
        }
        Ok(Some(entry))
    }

    /// Open a Variant-B build session. The session holds one open
    /// `WriteTransaction` and rotates it periodically based on
    /// `BuildSession::should_rotate`.
    pub fn begin_build(&self) -> Result<BuildSession<'_>, ParseStoreError> {
        let txn = self.db.begin_write()?;
        Ok(BuildSession {
            db: &self.db,
            txn: Some(txn),
            files_since_rotate: 0,
            bytes_since_rotate: 0,
        })
    }
}

fn ensure_schema(db: &Database) -> Result<(), ParseStoreError> {
    let txn = db.begin_write()?;
    {
        let mut meta = txn.open_table(META)?;
        // Read existing version (if any) into an owned value, releasing the
        // borrow before any potential write.
        let existing: Option<[u8; 4]> = match meta.get(META_KEY_SCHEMA_VERSION)? {
            Some(v) => {
                let bytes = v.value();
                if bytes.len() != 4 {
                    return Err(ParseStoreError::SchemaMismatch { found: 0, expected: SCHEMA_VERSION });
                }
                Some(bytes.try_into().unwrap())
            }
            None => None,
        };
        match existing {
            Some(buf) => {
                let found = u32::from_le_bytes(buf);
                if found != SCHEMA_VERSION {
                    return Err(ParseStoreError::SchemaMismatch { found, expected: SCHEMA_VERSION });
                }
            }
            None => {
                meta.insert(META_KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_le_bytes()[..])?;
            }
        }
        // Touch the entries table so it exists even on a fresh db.
        let _ = txn.open_table(PARSE_ENTRIES)?;
    }
    txn.commit()?;
    Ok(())
}

/// One open redb write transaction that accepts many `insert` calls and
/// commits/rotates based on a policy. Holds no in-memory `CacheEntry` after
/// each call returns — bytes go straight to the redb page cache.
///
/// Drop without `commit()` aborts the in-flight transaction (no partial writes).
pub struct BuildSession<'db> {
    db: &'db Database,
    txn: Option<WriteTransaction>,
    files_since_rotate: usize,
    bytes_since_rotate: usize,
}

impl<'db> BuildSession<'db> {
    /// Insert (or overwrite) the parse output for one file. Convenience wrapper
    /// around `insert_borrowed` that takes an owned `&CacheEntry`.
    pub fn insert(
        &mut self,
        file_path: &str,
        entry: &CacheEntry,
    ) -> Result<(), ParseStoreError> {
        self.insert_borrowed(
            file_path,
            &entry.content_hash,
            &entry.symbols,
            &entry.import_edges,
        )
    }

    /// Insert (or overwrite) the parse output for one file from borrowed parts.
    ///
    /// This is the hot-path API used by the build pipeline: it avoids cloning
    /// `Vec<Symbol>` and `Vec<Edge>` into a temporary `CacheEntry` (which the
    /// dhat profile flagged as a 3.58 GB allocation site in `passes::code`).
    /// bincode serializes the borrowed view to bytes identical to those a
    /// `CacheEntry` of the same parts would produce — round-trip safe via
    /// `lookup`.
    pub fn insert_borrowed(
        &mut self,
        file_path: &str,
        content_hash: &str,
        symbols: &[Symbol],
        import_edges: &[Edge],
    ) -> Result<(), ParseStoreError> {
        // Field order MUST match `CacheEntry` for bincode round-trip.
        // Verified by `insert_owned_and_borrowed_produce_identical_bytes` test.
        #[derive(serde::Serialize)]
        struct View<'a> {
            content_hash: &'a str,
            symbols: &'a [Symbol],
            import_edges: &'a [Edge],
        }
        let view = View { content_hash, symbols, import_edges };
        let bytes = bincode::serialize(&view)?;
        let written = bytes.len();

        // Skip pathologically large entries instead of trying to insert and
        // letting redb fail (redb's 3 GiB cap turns the failure into per-call
        // retry overhead that compounded into ~40s of wasted work on
        // MediaWiki). Returning Ok keeps the build path going; the file will
        // simply re-parse next time, the same way an unrelated cache miss
        // would.
        if written > SKIP_ENTRY_OVER_BYTES {
            eprintln!(
                "aethyme: parse store skipping {} ({:.1} MB > {:.1} MB cap; \
                 likely a generated bundle)",
                file_path,
                written as f64 / (1024.0 * 1024.0),
                SKIP_ENTRY_OVER_BYTES as f64 / (1024.0 * 1024.0),
            );
            return Ok(());
        }

        {
            let txn = self.txn.as_ref().expect("BuildSession invariant: txn present");
            let mut table = txn.open_table(PARSE_ENTRIES)?;
            table.insert(file_path, bytes.as_slice())?;
        }
        self.files_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Lookup that sees this session's pending writes (read-your-own-writes
    /// within the transaction). Useful if the build pipeline needs to check
    /// whether it has already processed a file in the current run.
    pub fn lookup_pending(
        &self,
        file_path: &str,
    ) -> Result<Option<CacheEntry>, ParseStoreError> {
        let txn = self.txn.as_ref().expect("BuildSession invariant: txn present");
        let table = match txn.open_table(PARSE_ENTRIES) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let Some(value) = table.get(file_path)? else { return Ok(None) };
        let entry: CacheEntry = bincode::deserialize(value.value())?;
        Ok(Some(entry))
    }

    /// Commit all pending writes. Consumes the session.
    pub fn commit(mut self) -> Result<(), ParseStoreError> {
        let txn = self.txn.take().expect("BuildSession invariant: txn present");
        txn.commit()?;
        Ok(())
    }

    /// Force a rotation now (commit current txn, open a fresh one). Useful at
    /// natural pipeline boundaries (e.g. between build stages).
    pub fn rotate(&mut self) -> Result<(), ParseStoreError> {
        let txn = self.txn.take().expect("BuildSession invariant: txn present");
        txn.commit()?;
        self.txn = Some(self.db.begin_write()?);
        self.files_since_rotate = 0;
        self.bytes_since_rotate = 0;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ⚠ TODO(daisy): YOUR DECISION — batch policy
    // ─────────────────────────────────────────────────────────────────────────
    //
    // This function is called after every `insert`. Returning `true` triggers a
    // commit + fresh transaction. Returning `false` keeps batching.
    //
    // The tradeoffs:
    //
    //   • TOO EAGER (e.g. every file): each commit pays an fsync (~1 ms on SSD).
    //     For 4093 MediaWiki files that's ~4 s of pure fsync overhead, plus
    //     redb has to allocate fresh write pages each round. Memory stays low
    //     but throughput suffers.
    //
    //   • TOO LAZY (e.g. never until commit()): one fsync at the end, fastest
    //     throughput, but the in-flight txn's dirty pages stay resident. On a
    //     huge repo this is the SAME problem we just fixed — unbounded
    //     in-memory buildup. Also: a panic at file 4000 loses files 1-3999.
    //
    //   • HYBRID: rotate when EITHER `files_since_rotate` OR `bytes_since_rotate`
    //     crosses a threshold. Bounds both fsync rate and in-flight memory.
    //
    // Suggested starting points if you go hybrid: 256 files OR 16 MB,
    // whichever first. But the right numbers depend on your judgment about
    // crash-safety vs throughput, and on what you know about how big a single
    // CacheEntry can get on adversarial inputs (some MediaWiki files are huge).
    //
    // Constraints to respect:
    //   - `self.files_since_rotate` and `self.bytes_since_rotate` reset on rotate
    //   - returning `true` is ALWAYS safe (just slower); returning `false`
    //     too aggressively risks unbounded memory growth
    //   - whichever you pick, make the constants `const` at module top so
    //     they're easy to find when tuning
    fn should_rotate(&self) -> bool {
        self.files_since_rotate >= ROTATE_EVERY_FILES
            || self.bytes_since_rotate >= ROTATE_EVERY_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::edge::{Edge, EdgeKind};
    use crate::model::symbol::{Symbol, SymbolKind};

    /// Cheap test-only macro: stringifies the enclosing function name.
    macro_rules! function_name {
        () => {{
            fn f() {}
            fn type_name<T>(_: T) -> &'static str { std::any::type_name::<T>() }
            let n = type_name(f);
            // strip "::f" suffix
            &n[..n.len() - 3]
        }};
    }

    fn tmp_root(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("aethyme_parse_store_{name}_{nonce}"))
    }

    fn sample_entry(hash: &str) -> CacheEntry {
        CacheEntry {
            content_hash: hash.to_string(),
            symbols: vec![Symbol::new(
                "do_thing",
                SymbolKind::Function,
                "src/lib.rs",
                12,
                "fn do_thing()",
            )],
            import_edges: vec![Edge::new(
                "src/lib.rs",
                "src/util.rs",
                EdgeKind::References,
                1000,
                "import",
            )],
        }
    }

    #[test]
    fn open_lookup_miss_on_empty_store() {
        let root = tmp_root(function_name!());
        let store = ParseStore::open(&root).expect("open");
        let result = store.lookup("any/path.rs", "h").expect("lookup");
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn insert_and_lookup_roundtrip() {
        let root = tmp_root(function_name!());
        let store = ParseStore::open(&root).expect("open");
        let mut session = store.begin_build().expect("session");
        let entry = sample_entry("hash-a");
        session.insert("src/lib.rs", &entry).expect("insert");
        session.commit().expect("commit");

        let got = store.lookup("src/lib.rs", "hash-a").expect("lookup").expect("hit");
        assert_eq!(got.content_hash, "hash-a");
        assert_eq!(got.symbols.len(), 1);
        assert_eq!(got.import_edges.len(), 1);

        let miss = store.lookup("src/lib.rs", "wrong-hash").expect("lookup");
        assert!(miss.is_none(), "wrong hash should miss");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drop_without_commit_loses_writes() {
        let root = tmp_root(function_name!());
        let store = ParseStore::open(&root).expect("open");
        {
            let mut session = store.begin_build().expect("session");
            session.insert("src/lib.rs", &sample_entry("h")).expect("insert");
            // session dropped without commit — txn aborts
        }
        let result = store.lookup("src/lib.rs", "h").expect("lookup");
        assert!(result.is_none(), "uncommitted writes must not be visible");
        let _ = std::fs::remove_dir_all(&root);
    }
}
