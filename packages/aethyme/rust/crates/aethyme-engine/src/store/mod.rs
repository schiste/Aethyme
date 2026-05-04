//! SurrealDB graph store for the Aethyme engine.
//!
//! Replaces the in-memory `RepositoryMap` + JSON serialization architecture
//! with a persistent, queryable graph database.

pub mod prompt;
pub mod read;
pub mod redb;
pub mod schema;
pub mod snippets;
pub mod write;

use std::path::{Path, PathBuf};

use surrealdb::Surreal;
use surrealdb::engine::local::{Db, SurrealKv};

/// Handle to an open SurrealDB graph store for one repository.
pub struct GraphStore {
    db: Surreal<Db>,
    db_path: PathBuf,
}

const DB_DIR_NAME: &str = "graph.db";
const DB_NAMESPACE: &str = "aethyme";
const DB_DATABASE: &str = "graph";

impl GraphStore {
    /// Open (or create) the graph store at `<repo_root>/.aethyme/graph.db`.
    pub async fn open(repo_root: &Path) -> Result<Self, surrealdb::Error> {
        let db_path = repo_root.join(".aethyme").join(DB_DIR_NAME);
        std::fs::create_dir_all(&db_path).ok();

        let db: Surreal<Db> = Surreal::new::<SurrealKv>(db_path.as_path()).await?;
        db.use_ns(DB_NAMESPACE).use_db(DB_DATABASE).await?;

        Ok(Self { db, db_path })
    }

    /// Apply the graph schema (idempotent — safe to call on every open).
    pub async fn ensure_schema(&self) -> Result<(), surrealdb::Error> {
        schema::apply(&self.db).await
    }

    /// Delete all data and re-apply schema. Used for full re-index.
    pub async fn reset(&self) -> Result<(), surrealdb::Error> {
        self.db.query("REMOVE DATABASE graph").await?;
        self.db.use_ns(DB_NAMESPACE).use_db(DB_DATABASE).await?;
        self.ensure_schema().await
    }

    /// Reference to the underlying SurrealDB instance for direct queries.
    pub fn db(&self) -> &Surreal<Db> {
        &self.db
    }

    /// Path to the DB directory on disk.
    pub fn path(&self) -> &Path {
        &self.db_path
    }
}
