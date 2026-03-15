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
use crate::model::symbol::{Symbol, SymbolKind};

/// Insert an area record.
pub async fn insert_area(db: &Surreal<Db>, area: &AreaNode) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(&area.id);
    let depth = area.path_prefix.matches('/').count() as i64 + 1;
    let role: Option<String> = if area.inferred { Some("inferred".to_string()) } else { None };
    db.query("CREATE type::record('area', $id) SET name = $name, depth = $depth, file_count = $fc, role = $role")
        .bind(("id", id))
        .bind(("name", area.name.clone()))
        .bind(("depth", depth))
        .bind(("fc", 0i64))  // will be updated after files are inserted
        .bind(("role", role))
        .await?;
    Ok(())
}

/// Insert a file record.
pub async fn insert_file(db: &Surreal<Db>, file: &FileNode) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(&file.path);
    let area_ref = file.area_id.as_ref().map(|a| format!("area:{}", sanitize_id(a)));
    let role = role_to_str(&file.role);
    db.query(
        "CREATE type::record('file', $id) SET \
         path = $path, area = $area, role = $role, language = $lang, \
         line_count = $lines, size_bytes = $size, content_hash = $hash"
    )
        .bind(("id", id))
        .bind(("path", file.path.clone()))
        .bind(("area", area_ref))
        .bind(("role", role.to_string()))
        .bind(("lang", file.language.clone()))
        .bind(("lines", file.line_count as i64))
        .bind(("size", file.size_bytes as i64))
        .bind(("hash", Option::<String>::None))
        .await?;
    Ok(())
}

/// Insert a symbol record.
pub async fn insert_symbol(db: &Surreal<Db>, symbol: &Symbol) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(&symbol.id);
    let file_ref = format!("file:{}", sanitize_id(&symbol.file));
    let kind = symbol_kind_to_str(&symbol.kind);
    db.query(
        "CREATE type::record('symbol', $id) SET \
         name = $name, kind = $kind, file = $file_ref, line = $line, \
         signature = $sig, language = $lang"
    )
        .bind(("id", id))
        .bind(("name", symbol.name.clone()))
        .bind(("kind", kind.to_string()))
        .bind(("file_ref", file_ref))
        .bind(("line", symbol.line as i64))
        .bind(("sig", Some(symbol.signature.clone())))
        .bind(("lang", symbol.language.clone()))
        .await?;
    Ok(())
}

/// Insert a typed edge relation.
pub async fn insert_edge(db: &Surreal<Db>, edge: &Edge) -> Result<(), surrealdb::Error> {
    let table = edge_kind_to_table(&edge.kind);
    let from_ref = resolve_record_id(&edge.from);
    let to_ref = resolve_record_id(&edge.to);

    let query = format!(
        "RELATE {} -> {} -> {} SET confidence = $conf, source = $src",
        from_ref, table, to_ref
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

/// Delete all data for a specific file and its symbols/edges.
/// Used for incremental re-indexing.
pub async fn delete_file_data(db: &Surreal<Db>, file_path: &str) -> Result<(), surrealdb::Error> {
    let id = sanitize_id(file_path);
    // Delete symbols belonging to this file
    db.query("DELETE symbol WHERE file = type::record('file', $id)")
        .bind(("id", id.clone()))
        .await?;
    // Delete edges from/to this file or its symbols
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
fn edge_kind_to_table(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
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

/// Convert SymbolKind to string.
fn symbol_kind_to_str(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Constant => "constant",
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

/// Resolve an entity ID to a SurrealDB record reference.
/// Heuristic: if the ID contains "::" it's a symbol, otherwise a file or area.
pub(crate) fn resolve_record_id(id: &str) -> String {
    if id.contains("::") {
        format!("symbol:{}", sanitize_id(id))
    } else if id.contains('/') || id.contains('.') {
        format!("file:{}", sanitize_id(id))
    } else {
        format!("area:{}", sanitize_id(id))
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
