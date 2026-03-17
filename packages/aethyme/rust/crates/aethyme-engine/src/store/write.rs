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
    // area_id is the engine's full ID like "area:Repo:path" — parse to get just the path
    let area_ref = file.area_id.as_ref().map(|a| {
        let (_, key) = resolve_record_parts(a);
        format!("area:`{key}`")
    });
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
    // symbol.id format: "includes/Page/Article.php::541::view"
    // symbol.file format: "includes/Page/Article.php"
    let id = sanitize_id(&symbol.id);
    let file_ref = format!("file:`{}`", sanitize_id(&symbol.file));
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

/// Insert a symbol from a FunctionNode or ClassNode (with engine-format ID).
pub async fn insert_symbol_raw(
    db: &Surreal<Db>,
    id: &str,
    name: &str,
    kind: &str,
    file_path: &str,
    line: usize,
    signature: &str,
    language: Option<&str>,
) -> Result<(), surrealdb::Error> {
    let file_ref = format!("file:`{}`", sanitize_id(file_path));
    db.query(
        "CREATE type::record('symbol', $id) SET \
         name = $name, kind = $kind, file = $file_ref, line = $line, \
         signature = $sig, language = $lang"
    )
        .bind(("id", id.to_string()))
        .bind(("name", name.to_string()))
        .bind(("kind", kind.to_string()))
        .bind(("file_ref", file_ref))
        .bind(("line", line as i64))
        .bind(("sig", Some(signature.to_string())))
        .bind(("lang", language.map(|s| s.to_string())))
        .await?;
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
                // Symbol: extract path and symbol name after repo prefix
                let path_part = strip_repo_prefix(rest);
                return ("symbol".to_string(), sanitize_id(path_part));
            }
            "file" | "doc" => {
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
