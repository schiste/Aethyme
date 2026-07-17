//! Callsite expansion - redb-backed callers-of evidence pass.
//!
//! Given a batch of symbol-search hits, read incoming `calls` adjacency from
//! the graph store, group results by caller file, and rank by how many of the
//! candidate symbols converge on the same file. Files called from 2+ candidate
//! symbols rank as strong (0.86 confidence) vs single-symbol weak (0.74).

use crate::model::edge::EdgeKind;
use crate::store::redb::graph_store::{NeighborDirection, ReadOnlyGraphStore};

use super::{AnswerItem, ExploreError, SymbolBatchResults};

/// Pick the strongest symbol hits, look up callers via redb, and emit one
/// `call_site_file` AnswerItem per distinct caller file.
///
/// Strategy:
///   1. Walk symbol_matches in query-order, collect distinct symbol names up to
///      `max_symbols`. Prefer high-score hits.
///   2. Resolve each name to persisted function/class nodes.
///   3. Read incoming `calls` adjacency for those nodes.
///   4. Group caller paths by file, dedup, score by how many distinct symbols
///      routed to that file.
///   5. Take top `max_results`.
pub(super) fn compute_callsite_files(
    store: &ReadOnlyGraphStore,
    symbol_matches: &SymbolBatchResults,
    max_symbols: usize,
    max_results: usize,
) -> Result<Vec<AnswerItem>, ExploreError> {
    if max_symbols == 0 || max_results == 0 {
        return Ok(Vec::new());
    }

    let symbol_names = collect_symbol_names(symbol_matches, max_symbols);
    if symbol_names.is_empty() {
        return Ok(Vec::new());
    }

    // file_path -> (Set<symbol_name>, hit_count, sample_callers)
    let mut by_file: std::collections::BTreeMap<
        String,
        (
            std::collections::BTreeSet<String>,
            usize,
            Vec<serde_json::Value>,
        ),
    > = std::collections::BTreeMap::new();

    for symbol in &symbol_names {
        let matches = store
            .find_symbols(symbol, None)
            .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
        for matched_symbol in matches {
            let callers = store
                .neighbors(
                    &matched_symbol.id,
                    NeighborDirection::Incoming,
                    Some(EdgeKind::Calls),
                )
                .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
            for caller in callers {
                let caller_id = caller.other.as_str();
                let Some(display) = store
                    .node_display(caller_id)
                    .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?
                else {
                    continue;
                };
                let Some(file_path) = display
                    .path
                    .clone()
                    .or_else(|| file_path_from_caller_id(&display.id))
                else {
                    continue;
                };
                if file_path == matched_symbol.path {
                    continue;
                }
                let entry = by_file
                    .entry(file_path.clone())
                    .or_insert_with(|| (std::collections::BTreeSet::new(), 0, Vec::new()));
                entry.0.insert(symbol.clone());
                entry.1 += 1;
                if entry.2.len() < 5 {
                    entry.2.push(serde_json::json!({
                        "symbol": symbol,
                        "caller_id": caller_id,
                        "display": {
                            "id": display.id,
                            "kind": format!("{:?}", display.kind).to_ascii_lowercase(),
                            "display": display.display,
                            "path": display.path,
                        },
                    }));
                }
            }
        }
    }

    let mut ranked: Vec<(
        String,
        std::collections::BTreeSet<String>,
        usize,
        Vec<serde_json::Value>,
    )> = by_file
        .into_iter()
        .map(|(path, (syms, hits, samples))| (path, syms, hits, samples))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(max_results);

    let result: Vec<AnswerItem> = ranked
        .into_iter()
        .map(|(path, symbols, hit_count, samples)| {
            let symbol_count = symbols.len();
            let multi = symbol_count >= 2;
            let confidence = if multi { 0.86 } else { 0.74 };
            let reason = if multi {
                "Multiple candidate symbols are called from this file; \
                 likely a usage entry point or dispatch hub."
            } else {
                "This file calls one of the candidate symbols; verify \
                 whether it's the primary caller or one of many."
            };
            let symbols_list: Vec<&String> = symbols.iter().collect();
            AnswerItem {
                kind: "call_site_file".into(),
                target: path.clone(),
                path: Some(path),
                status: "candidate".into(),
                confidence,
                reason: reason.into(),
                role: "callsite".into(),
                evidence: serde_json::json!({
                    "source": "redb.calls",
                    "symbols": symbols_list,
                    "hit_count": hit_count,
                    "samples": samples,
                }),
            }
        })
        .collect();

    debug_assert!(
        result.len() <= max_results,
        "callsite cap violated: {} > {}",
        result.len(),
        max_results
    );
    debug_assert!(
        result.iter().all(|item| item.kind == "call_site_file"),
        "callsite item with unexpected kind"
    );
    debug_assert_eq!(
        result.len(),
        result
            .iter()
            .filter_map(|i| i.path.as_deref())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "callsite items have duplicate paths"
    );
    Ok(result)
}

fn collect_symbol_names(symbol_matches: &SymbolBatchResults, max_symbols: usize) -> Vec<String> {
    // Round-robin across queries so one broad query cannot consume the entire
    // symbol budget before a narrower user concept contributes.
    let mut symbol_names: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pass = 0usize;
    loop {
        if symbol_names.len() >= max_symbols {
            break;
        }
        let mut added_this_pass = false;
        for query in &symbol_matches.query_order {
            if symbol_names.len() >= max_symbols {
                break;
            }
            let Some(hits) = symbol_matches.by_query.get(query) else {
                continue;
            };
            let Some(hit) = hits.get(pass) else {
                continue;
            };
            if seen.insert(hit.name.clone()) {
                symbol_names.push(hit.name.clone());
                added_this_pass = true;
            }
        }
        if !added_this_pass {
            break;
        }
        pass += 1;
    }
    symbol_names
}

/// Parse a caller's structured id of the form
/// `<kind>:<repo>:<path>:<symbol>` and return the path segment.
fn file_path_from_caller_id(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let path = parts[2];
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}
