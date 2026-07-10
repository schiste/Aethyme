//! Callsite expansion — Rust port of the callers-of evidence pass.
//!
//! Given a batch of symbol-search hits, ask the engine daemon "who
//! calls these?" via the `callers-of` RPC, group results by caller
//! file, and rank by how many of the candidate symbols converge on
//! the same file. Files called from ≥2 candidate symbols rank as
//! strong (0.86 confidence) vs single-symbol weak (0.74).

use std::path::Path;

use crate::daemon;

use super::{AnswerItem, ExploreError, SymbolBatchResults};

/// Pick the strongest symbol hits, look up callers via the daemon,
/// and emit one `call_site_file` AnswerItem per distinct caller file.
///
/// Strategy:
///   1. Walk symbol_matches in query-order, collect distinct symbol
///      ids up to `max_symbols`. Prefer high-score hits.
///   2. Issue one `callers-of` RPC with the batch (single roundtrip).
///   3. Group caller paths by file, dedup, score by how many distinct
///      symbols routed to that file (signal multiplier — a file
///      calling 2+ of our candidate symbols is stronger evidence).
///   4. Take top `max_results`.
pub(super) fn compute_callsite_files(
    socket: &Path,
    symbol_matches: &SymbolBatchResults,
    max_symbols: usize,
    max_results: usize,
) -> Result<Vec<AnswerItem>, ExploreError> {
    if max_symbols == 0 || max_results == 0 {
        return Ok(Vec::new());
    }
    // Collect distinct symbol ids round-robin across queries: take the
    // highest-scoring hit from each query first, then the second, etc.
    // This is critical when one query (e.g. "suppliers") returns 20
    // hits while another ("grader") returns 2 — depth-first iteration
    // would burn the entire `max_symbols` budget on the first query and
    // never reach the second. The whole point of callsite expansion is
    // to find files that bridge the user's distinct concepts, so each
    // concept must contribute.
    let mut symbol_ids: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pass = 0usize;
    loop {
        if symbol_ids.len() >= max_symbols {
            break;
        }
        let mut added_this_pass = false;
        for query in &symbol_matches.query_order {
            if symbol_ids.len() >= max_symbols {
                break;
            }
            let Some(hits) = symbol_matches.by_query.get(query) else {
                continue;
            };
            let Some(hit) = hits.get(pass) else { continue };
            // SymbolHit name is what the engine accepts via callers-of
            // (matched against the symbol-name index). For canonical
            // tightness we could parse and pass full ids, but names
            // route correctly today.
            if seen.insert(hit.name.clone()) {
                symbol_ids.push(hit.name.clone());
                added_this_pass = true;
            }
        }
        if !added_this_pass {
            // No query had a hit at this depth — nothing left to drain.
            break;
        }
        pass += 1;
    }
    if symbol_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rpc = serde_json::json!({
        "command": "callers-of",
        "targets": symbol_ids,
    });
    let response_text = daemon::send_request(socket, &rpc).map_err(ExploreError::DaemonRpc)?;
    let envelope: serde_json::Value = serde_json::from_str(response_text.trim())
        .map_err(|e| ExploreError::InvalidResponse(format!("not JSON: {e}")))?;
    if envelope.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Ok(Vec::new());
    }
    let result_obj = match envelope.get("result").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(Vec::new()),
    };

    // file_path -> (Set<symbol_name>, hit_count, sample_callers)
    let mut by_file: std::collections::BTreeMap<
        String,
        (
            std::collections::BTreeSet<String>,
            usize,
            Vec<serde_json::Value>,
        ),
    > = std::collections::BTreeMap::new();
    for (symbol, callers) in result_obj {
        let arr = match callers.as_array() {
            Some(a) => a,
            None => continue,
        };
        for caller in arr {
            // The id format is `<kind>:<repo>:<path>:<symbol>` —
            // splitting on ':' once, the file path lives between the
            // 2nd and last segment. Easier: use the `path` we asked
            // for if the daemon provided it directly... actually we
            // didn't include path in the response. Fall back to
            // parsing `id`.
            let id = match caller.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let Some(file_path) = file_path_from_caller_id(id) else {
                continue;
            };
            let entry = by_file
                .entry(file_path.clone())
                .or_insert_with(|| (std::collections::BTreeSet::new(), 0, Vec::new()));
            entry.0.insert(symbol.clone());
            entry.1 += 1;
            if entry.2.len() < 5 {
                entry.2.push(serde_json::json!({
                    "symbol": symbol,
                    "caller_id": id,
                    "display": caller.get("display"),
                }));
            }
        }
    }

    // Rank: more distinct symbols routing through this file = stronger
    // evidence. Tiebreak on hit_count then path (alphabetical).
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
                    "source": "callers-of",
                    "symbols": symbols_list,
                    "hit_count": hit_count,
                    "samples": samples,
                }),
            }
        })
        .collect();

    // Post-conditions documenting the contract for downstream callers in
    // `build_response`:
    //   - cap respected: never emit more than `max_results` items
    //   - kind invariant: every item is a `call_site_file` (build_response
    //     uses .kind to route into the answer/nav-hint partitioning)
    //   - distinct paths: no two items share the same `path` (the by_file
    //     BTreeMap collapses duplicates by construction; this check would
    //     have caught a regression if the dedup were ever weakened)
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

/// Parse a caller's structured id of the form
/// `<kind>:<repo>:<path>:<symbol>` and return the path segment.
/// Returns `None` if the id doesn't match the expected shape, which
/// happens when the engine emits a node id with no file (e.g.
/// area-level nodes).
fn file_path_from_caller_id(id: &str) -> Option<String> {
    // The engine canonicalizes path-bearing ids as
    //   `kind:repo_name:relative/path:symbol`
    // For our purposes we want the third colon-separated segment.
    // Split with limit so a colon inside the symbol name doesn't
    // break the parse.
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
