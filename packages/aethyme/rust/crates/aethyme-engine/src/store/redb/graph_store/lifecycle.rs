//! Opening, creating, staging, publishing and schema-checking the redb
//! store file.

use super::*;

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

    /// Copy an immutable cached artifact into private staging, validate its
    /// schema, bind its repository metadata to this worktree, and publish it.
    /// The shared cache file is never opened writable or hard-linked.
    pub fn install_cached_artifact(
        repo_root: &Path,
        cached_artifact: &Path,
        source_commit: &str,
    ) -> Result<RepoMetadata, GraphStoreError> {
        let cached_metadata = std::fs::symlink_metadata(cached_artifact)?;
        if !cached_metadata.file_type().is_file() {
            return Err(invalid_cached_artifact("artifact is not a regular file"));
        }
        let canonical = repo_root.canonicalize()?;
        let staging_path = Self::staging_path(&canonical);
        if let Some(parent) = staging_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::symlink_metadata(&staging_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(&staging_path)?;
            }
            Ok(_) => {
                return Err(invalid_cached_artifact(
                    "staging destination is not a regular file",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::copy(cached_artifact, &staging_path)?;

        let store = Self::open_path(staging_path)?;
        let mut metadata = store
            .repo_metadata()?
            .ok_or_else(|| invalid_cached_artifact("repository metadata is missing"))?;
        let root_path = canonical.to_string_lossy().into_owned();
        let txn = store.db.begin_write()?;
        {
            let mut table = txn.open_table(REPOSITORIES)?;
            let mut repositories = Vec::new();
            for row in table.iter()? {
                let (key, value) = row?;
                repositories.push((
                    key.value().to_string(),
                    bincode::deserialize::<RepositoryNode>(value.value())?,
                ));
            }
            if repositories.len() != 1 {
                return Err(invalid_cached_artifact(format!(
                    "expected one repository row, found {}",
                    repositories.len()
                )));
            }
            for (key, mut repository) in repositories {
                repository.root_path.clone_from(&root_path);
                let bytes = bincode::serialize(&repository)?;
                table.insert(key.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        metadata.root_path = root_path;
        metadata.commit_hash = Some(source_commit.to_string());
        metadata.indexed_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        store.set_repo_metadata(&metadata)?;
        drop(store);
        Self::publish_staging(&canonical)?;
        Ok(metadata)
    }

    pub(super) fn open_path(db_path: PathBuf) -> Result<Self, GraphStoreError> {
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

pub(super) fn open_or_create_database(db_path: &Path) -> Result<Database, GraphStoreError> {
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

pub(super) fn invalid_cached_artifact(reason: impl Into<String>) -> GraphStoreError {
    GraphStoreError::Io(std::io::Error::new(
        ErrorKind::InvalidData,
        format!("invalid cached graph store: {}", reason.into()),
    ))
}

pub(super) fn open_read_only_database(db_path: &Path) -> Result<ReadOnlyDatabase, GraphStoreError> {
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

pub(super) fn ensure_schema(db: &Database) -> Result<(), GraphStoreError> {
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

pub(super) fn verify_schema_read_only(db: &ReadOnlyDatabase) -> Result<(), GraphStoreError> {
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
