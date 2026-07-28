//! Native `aethyme query` front end (python-retirement Phase 1).
//!
//! Replaces the delegated Click group `query symbol|deps|impact` with
//! in-process redb reads. Output is byte-compatible with the Python
//! renderers it replaces (verified by the golden corpus):
//!
//! - `symbol` text mode: `- {name} ({kind}) at {file}:{line} [score={score}]`
//! - `symbol --json-output`: the engine's search-hit JSON, pretty-printed
//!   with 2-space indent (Python did `json.dumps(engine_json, indent=2)`)
//! - `deps` / `impact`: one `- {item}` line per frontier entry
//!
//! Lives in the engine lib (same pattern as [`crate::explore_cli`]) so
//! the router binary stays a thin shell and the eventual `aethyme-cli`
//! crate move is mechanical.

use std::path::PathBuf;

use crate::graph::neighborhood::impact_frontier_redb;
use crate::graph::search::symbol_search_redb;
use crate::store::redb::graph_store::{GraphStore, NeighborDirection};

const SYMBOL_LIMIT: usize = 20;

/// Run `query <subcommand> ...`. `args` excludes the leading `query`.
///
/// Errors are returned as messages; the router prints them in Click's
/// `Error: {msg}` shape with exit 1 so scripted consumers see the same
/// failure surface the Python CLI produced.
pub fn run(args: &[String]) -> Result<(), String> {
    let Some(subcommand) = args.first() else {
        return Err("missing query subcommand (symbol | deps | impact)".to_string());
    };
    let rest = &args[1..];
    match subcommand.as_str() {
        "symbol" => run_symbol(rest),
        "deps" => run_deps(rest),
        "impact" => run_impact(rest),
        other => Err(format!("unsupported query subcommand: {other}")),
    }
}

fn repo_arg(rest: &[String], usage: &str) -> Result<PathBuf, String> {
    let raw = rest.first().ok_or_else(|| usage.to_string())?;
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(format!("repository path is not a directory: {raw}"));
    }
    path.canonicalize().map_err(|e| e.to_string())
}

fn run_symbol(rest: &[String]) -> Result<(), String> {
    let usage = "usage: aethyme query symbol <repo_path> <query> [--json-output]";
    let repo = repo_arg(rest, usage)?;
    let query = rest
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| usage.to_string())?;
    let json_output = rest.iter().any(|a| a == "--json-output");

    let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
    let hits = symbol_search_redb(&store, query, SYMBOL_LIMIT).map_err(|e| e.to_string())?;

    if json_output {
        // Python re-emitted the engine JSON via json.dumps(..., indent=2);
        // round-trip through serde_json::Value for the same 2-space shape.
        let value: serde_json::Value = serde_json::from_str(&crate::json::search_hits(&hits))
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for hit in &hits {
        println!(
            "- {} ({}) at {}:{} [score={}]",
            hit.name, hit.kind, hit.file, hit.line, hit.score
        );
    }
    Ok(())
}

fn run_deps(rest: &[String]) -> Result<(), String> {
    let usage = "usage: aethyme query deps <repo_path> <target-file>";
    let repo = repo_arg(rest, usage)?;
    let target = rest.get(1).ok_or_else(|| usage.to_string())?;

    let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
    let edges = store
        .neighbors(target, NeighborDirection::Outgoing, None)
        .map_err(|e| e.to_string())?;
    for edge in &edges {
        if let Some(path) = file_path_from_id(edge.other.as_str()) {
            println!("- {path}");
        }
    }
    Ok(())
}

fn run_impact(rest: &[String]) -> Result<(), String> {
    let usage = "usage: aethyme query impact <repo_path> <target>";
    let repo = repo_arg(rest, usage)?;
    let target = rest.get(1).ok_or_else(|| usage.to_string())?;

    let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
    let impact = impact_frontier_redb(&store, target).map_err(|e| e.to_string())?;
    for item in &impact {
        println!("- {item}");
    }
    Ok(())
}

/// `file:<repo>:<path>` → `<path>`; other node kinds are filtered out —
/// the same projection the engine CLI's `deps` arm applies.
fn file_path_from_id(id: &str) -> Option<String> {
    let rest = id.strip_prefix("file:")?;
    let after_repo = rest.split_once(':')?.1;
    Some(after_repo.to_string())
}

#[cfg(test)]
mod tests {
    use super::file_path_from_id;

    #[test]
    fn file_id_projects_to_path() {
        assert_eq!(
            file_path_from_id("file:repo:src/auth/session.py").as_deref(),
            Some("src/auth/session.py")
        );
        assert_eq!(file_path_from_id("dir:repo:src"), None);
        assert_eq!(file_path_from_id("fn:repo:src/a.py:issue"), None);
    }
}
