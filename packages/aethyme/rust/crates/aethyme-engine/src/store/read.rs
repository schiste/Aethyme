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

/// Get file info with its symbols.
pub async fn file_info(
    db: &Surreal<Db>,
    file_path: &str,
) -> Result<Option<FileWithSymbols>, surrealdb::Error> {
    let id = super::write::sanitize_id(file_path);
    let mut result = db
        .query(
            "SELECT *, (SELECT name, kind, line, signature FROM symbol WHERE file = $parent.id) AS symbols \
             FROM type::record('file', $id)"
        )
        .bind(("id", id))
        .await?;
    let files: Vec<FileWithSymbols> = result.take(0)?;
    Ok(files.into_iter().next())
}

// ── Edge / graph queries ────────────────────────────────────────────

/// Get all outgoing edges from a file or symbol.
pub async fn edges_from(
    db: &Surreal<Db>,
    entity_id: &str,
) -> Result<Vec<EdgeRecord>, surrealdb::Error> {
    let record_ref = super::write::resolve_record_id(entity_id);
    let query = format!(
        "SELECT ->imports->file.path AS import_targets, \
                ->calls->symbol.{{name, file, line}} AS call_targets \
         FROM {}",
        record_ref
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

    let mut entrypoints_result = db
        .query("SELECT out.path AS path FROM entrypoint_for ORDER BY confidence DESC LIMIT 10")
        .await?;
    let entrypoints: Vec<PathRecord> = entrypoints_result.take(0)?;

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
pub struct SymbolRecord {
    pub name: String,
    pub kind: String,
    pub line: Option<i64>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct FileWithSymbols {
    pub path: String,
    pub role: String,
    pub language: Option<String>,
    pub line_count: Option<i64>,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct EdgeRecord {
    pub import_targets: Option<Vec<String>>,
    pub call_targets: Option<Vec<serde_json::Value>>,
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
