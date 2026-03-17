//! Streaming write operations for the graph store.
//!
//! All writes are designed for streaming: insert one entity at a time
//! during file parsing, never accumulating the full graph in memory.

use surrealdb::Surreal;
use surrealdb::engine::local::Db;

use crate::model::area::AreaNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::model::file::{FileNode, FileRole};
use crate::model::risk::{RiskFlag, RiskLevel};

/// Insert an area record.
pub async fn insert_area(db: &Surreal<Db>, area: &AreaNode) -> Result<(), surrealdb::Error> {
    // Use path_prefix as the canonical ID — matches what edges reference after stripping kind+repo
    let id = sanitize_id(&area.path_prefix);
    let depth = area.path_prefix.matches('/').count() as i64 + 1;
    let role: Option<String> = if area.inferred { Some("inferred".to_string()) } else { None };
    db.query("CREATE type::record('area', $id) SET name = $name, depth = $depth, file_count = $fc, role = $role")
        .bind(("id", id))
        .bind(("name", area.name.clone()))
        .bind(("depth", depth))
        .bind(("fc", 0i64))
        .bind(("role", role))
        .await?;
    Ok(())
}

/// Insert a file record.
pub async fn insert_file(db: &Surreal<Db>, file: &FileNode) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(&file.path);
    let role = role_to_str(&file.role);

    // Build query dynamically based on whether area_id is present
    // area field is typed option<record<area>> — must use type::record(), not a string
    if let Some(ref area_id) = file.area_id {
        let (_, area_key) = resolve_record_parts(area_id);
        db.query(
            "CREATE type::record('file', $id) SET \
             path = $path, area = type::record('area', $area_key), role = $role, language = $lang, \
             line_count = $lines, size_bytes = $size, content_hash = $hash"
        )
            .bind(("id", id))
            .bind(("path", file.path.clone()))
            .bind(("area_key", area_key))
            .bind(("role", role.to_string()))
            .bind(("lang", file.language.clone()))
            .bind(("lines", file.line_count as i64))
            .bind(("size", file.size_bytes as i64))
            .bind(("hash", Option::<String>::None))
            .await?;
    } else {
        db.query(
            "CREATE type::record('file', $id) SET \
             path = $path, role = $role, language = $lang, \
             line_count = $lines, size_bytes = $size, content_hash = $hash"
        )
            .bind(("id", id))
            .bind(("path", file.path.clone()))
            .bind(("role", role.to_string()))
            .bind(("lang", file.language.clone()))
            .bind(("lines", file.line_count as i64))
            .bind(("size", file.size_bytes as i64))
            .bind(("hash", Option::<String>::None))
            .await?;
    }
    Ok(())
}

/// Insert a typed edge relation.
pub async fn insert_edge(db: &Surreal<Db>, edge: &Edge) -> Result<(), surrealdb::Error> {
    let table = edge_kind_to_table(&edge.kind);
    let (from_table, from_id) = resolve_record_parts(&edge.from);
    let (to_table, to_id) = resolve_record_parts(&edge.to);

    // RELATE requires raw record refs: table:`id`
    let query = format!(
        "RELATE {from_table}:`{from_id}` -> {table} -> {to_table}:`{to_id}` SET confidence = $conf, source = $src"
    );
    db.query(&query)
        .bind(("conf", edge.confidence as f64))
        .bind(("src", Some(edge.source.clone())))
        .await?;
    Ok(())
}

/// Insert a risk flag.
pub async fn insert_risk(db: &Surreal<Db>, risk: &RiskFlag) -> Result<(), surrealdb::Error> {
    let area_ref: Option<String> = None; // RiskArea is a category enum, not an area ID
    let level = risk_level_to_str(&risk.level);
    db.query("CREATE risk SET scope = $scope, area = $area, level = $level, reason = $reason")
        .bind(("scope", risk.scope.clone()))
        .bind(("area", area_ref))
        .bind(("level", level.to_string()))
        .bind(("reason", risk.reason.clone()))
        .await?;
    Ok(())
}

/// Delete all data for a specific file and its edges.
/// Used for incremental re-indexing.
pub async fn delete_file_data(db: &Surreal<Db>, file_path: &str) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(file_path);
    // Delete edges from/to this file
    db.query("DELETE imports WHERE in = type::record('file', $id) OR out = type::record('file', $id)")
        .bind(("id", id.clone()))
        .await?;
    // Delete the file record itself
    db.query("DELETE type::record('file', $id)")
        .bind(("id", id))
        .await?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Map EdgeKind to SurrealDB relation table names.
/// Symbol-level edges (Calls) are filtered out before reaching this function.
fn edge_kind_to_table(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "imports", // symbol-level calls are skipped; fallback for safety
        EdgeKind::References => "imports", // fallback to imports
        EdgeKind::Configures => "configures",
        EdgeKind::Contains => "contains",
        EdgeKind::EntrypointFor => "entrypoint_for",
        EdgeKind::BelongsTo => "contains",
        EdgeKind::Defines => "contains",
        EdgeKind::Documents => "imports",
    }
}

/// Convert FileRole to string.
fn role_to_str(role: &FileRole) -> &'static str {
    match role {
        FileRole::Source => "source",
        FileRole::Test => "test",
        FileRole::Doc => "doc",
        FileRole::Config => "config",
        FileRole::Asset => "asset",
        FileRole::Generated => "generated",
        FileRole::Binary => "binary",
        FileRole::Cache => "cache",
        FileRole::Unknown => "unknown",
    }
}

/// Convert RiskLevel to string.
fn risk_level_to_str(level: &RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

/// Resolve an engine entity ID to a SurrealDB record reference string.
pub(crate) fn resolve_record_id(id: &str) -> String {
    let (table, key) = resolve_record_parts(id);
    format!("{table}:`{key}`")
}

/// Parse an engine entity ID into (surreal_table, sanitized_key).
///
/// Engine IDs follow the format `{kind}:{repo}:{path}[:{symbol}]` or `{kind}:{repo}`.
/// Examples:
///   fn:MyRepo:includes/Page/Article.php:view        → symbol, includes_Page_Article_php__view
///   class:MyRepo:includes/Page/WikiPage.php:WikiPage → symbol, includes_Page_WikiPage_php__WikiPage
///   file:MyRepo:README.md                           → file, README_md
///   dir:MyRepo:maintenance                          → area, maintenance
///   area:MyRepo:.phan                               → area, _phan
///   repo:MyRepo                                     → area, MyRepo
///   doc:MyRepo:docs/README.md                       → file, docs_README_md
///
/// For IDs that don't have a kind prefix (plain paths), fall back to heuristics.
pub fn resolve_record_parts(id: &str) -> (String, String) {
    // Try to parse as kind:repo:path[:symbol]
    if let Some(first_colon) = id.find(':') {
        let kind = &id[..first_colon];
        let rest = &id[first_colon + 1..];

        match kind {
            "fn" | "class" | "const" => {
                let path_part = strip_repo_prefix(rest);
                return ("symbol".to_string(), sanitize_id(path_part));
            }
            "file" | "doc" | "import" => {
                let path_part = strip_repo_prefix(rest);
                return ("file".to_string(), sanitize_id(path_part));
            }
            "dir" | "area" | "repo" => {
                let path_part = strip_repo_prefix(rest);
                return ("area".to_string(), sanitize_id(path_part));
            }
            _ => {
                // Not a known kind prefix — might be a raw ID with colons
            }
        }
    }

    // Fallback: heuristic for plain IDs without kind prefix
    if id.contains("::") {
        ("symbol".to_string(), sanitize_id(id))
    } else if id.contains('/') || id.contains('.') {
        ("file".to_string(), sanitize_id(id))
    } else {
        ("area".to_string(), sanitize_id(id))
    }
}

/// Strip the repo name prefix from an engine ID's rest part.
/// Input: "MyRepo:includes/Page/Article.php:view" or "MyRepo" (no further colons)
/// Output: "includes/Page/Article.php:view" or ""
fn strip_repo_prefix(rest: &str) -> &str {
    // The repo name goes up to the second colon (first colon after repo name)
    if let Some(colon_pos) = rest.find(':') {
        &rest[colon_pos + 1..]
    } else {
        // No further colon — this IS the repo name (e.g., "repo:MyRepo")
        rest
    }
}

/// Sanitize an ID for use as a SurrealDB record key.
/// Replaces characters that are problematic in record IDs.
pub(crate) fn sanitize_id(id: &str) -> String {
    id.replace('/', "_")
        .replace('\\', "_")
        .replace(' ', "_")
        .replace(':', "_")
        .replace('.', "_")
}
