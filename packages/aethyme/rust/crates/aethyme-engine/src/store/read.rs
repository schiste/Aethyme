//! Query operations for the graph store.
//!
//! Each function runs a targeted SurrealQL query and returns
//! small, focused JSON-serializable results.

use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::local::Db;
use surrealdb::types::SurrealValue;

// ── Structural queries ──────────────────────────────────────────────

/// List areas, optionally filtered by depth.
pub async fn list_areas(
    db: &Surreal<Db>,
    depth: Option<u32>,
) -> Result<Vec<AreaRecord>, surrealdb::Error> {
    let mut result = if let Some(d) = depth {
        db.query("SELECT * FROM area WHERE depth = $d ORDER BY file_count DESC")
            .bind(("d", d as i64))
            .await?
    } else {
        db.query("SELECT * FROM area ORDER BY depth, file_count DESC")
            .await?
    };
    let areas: Vec<AreaRecord> = result.take(0)?;
    Ok(areas)
}

/// List files in a specific area.
pub async fn files_in_area(
    db: &Surreal<Db>,
    area_id: &str,
) -> Result<Vec<FileRecord>, surrealdb::Error> {
    let id = super::write::sanitize_id(area_id);
    let mut result = db
        .query("SELECT * FROM file WHERE area = type::record('area', $id) ORDER BY path")
        .bind(("id", id))
        .await?;
    let files: Vec<FileRecord> = result.take(0)?;
    Ok(files)
}

// ── Edge / graph queries ────────────────────────────────────────────

/// Get all outgoing imports from a file (what does this file import?).
pub async fn edges_from(
    db: &Surreal<Db>,
    entity_id: &str,
) -> Result<Vec<EdgeRecord>, surrealdb::Error> {
    let (table, key) = super::write::resolve_record_parts(entity_id);
    let query = format!(
        "SELECT ->imports->file.path AS import_targets \
         FROM {table}:`{key}`"
    );
    let mut result = db.query(&query).await?;
    let edges: Vec<EdgeRecord> = result.take(0)?;
    Ok(edges)
}

/// Get all incoming imports TO a file (who imports me?).
pub async fn edges_to(
    db: &Surreal<Db>,
    entity_id: &str,
) -> Result<Vec<EdgeRecord>, surrealdb::Error> {
    let (table, key) = super::write::resolve_record_parts(entity_id);
    let query = format!(
        "SELECT <-imports<-file.path AS imported_by \
         FROM {table}:`{key}`"
    );
    let mut result = db.query(&query).await?;
    let edges: Vec<EdgeRecord> = result.take(0)?;
    Ok(edges)
}

/// Extract a sub-graph: all nodes and edges within N hops of a seed.
pub async fn subgraph(
    db: &Surreal<Db>,
    seed_id: &str,
    hops: u32,
) -> Result<SubgraphResult, surrealdb::Error> {
    let record_ref = super::write::resolve_record_id(seed_id);

    // Collect files within N hops via imports
    let query = format!(
        "SELECT <->imports<->(file, {}).path AS connected_files FROM {}",
        hops, record_ref
    );
    let mut result = db.query(&query).await?;
    let connected: Vec<SubgraphResult> = result.take(0)?;
    Ok(connected.into_iter().next().unwrap_or_default())
}

/// Get the structural overview: top areas, entrypoints, key configs.
pub async fn overview(db: &Surreal<Db>) -> Result<OverviewResult, surrealdb::Error> {
    let mut areas_result = db
        .query("SELECT * FROM area WHERE depth = 1 ORDER BY file_count DESC LIMIT 20")
        .await?;
    let areas: Vec<AreaRecord> = areas_result.take(0)?;

    // Entrypoints: get files marked as entry points via the entrypoint_for relation.
    // The `in` side is the file record; query the file table for entries that have
    // an outgoing entrypoint_for edge.
    let mut entrypoints_result = db
        .query("SELECT path FROM file WHERE count(->entrypoint_for) > 0 LIMIT 10")
        .await?;
    let entrypoints: Vec<PathRecord> = entrypoints_result.take(0).unwrap_or_default();

    let mut risks_result = db
        .query("SELECT * FROM risk ORDER BY level DESC LIMIT 20")
        .await?;
    let risks: Vec<RiskRecord> = risks_result.take(0)?;

    let mut repo_result = db.query("SELECT * FROM repo LIMIT 1").await?;
    let repo: Vec<RepoRecord> = repo_result.take(0)?;

    Ok(OverviewResult {
        repo: repo.into_iter().next(),
        areas,
        entrypoints: entrypoints.into_iter().map(|p| p.path).collect(),
        risks,
    })
}

// ── Record types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct AreaRecord {
    pub name: String,
    pub depth: i64,
    pub file_count: i64,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct FileRecord {
    pub path: String,
    pub role: String,
    pub language: Option<String>,
    pub line_count: Option<i64>,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct EdgeRecord {
    pub import_targets: Option<Vec<String>>,
    pub imported_by: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, SurrealValue)]
pub struct SubgraphResult {
    pub connected_files: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct PathRecord {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct RiskRecord {
    pub scope: String,
    pub level: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct RepoRecord {
    pub root_path: String,
    pub commit_hash: Option<String>,
    pub file_count: i64,
    pub languages: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewResult {
    pub repo: Option<RepoRecord>,
    pub areas: Vec<AreaRecord>,
    pub entrypoints: Vec<String>,
    pub risks: Vec<RiskRecord>,
}
