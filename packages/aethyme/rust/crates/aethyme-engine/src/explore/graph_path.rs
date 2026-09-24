//! The graph-backed Explore path: the orchestration entry point and the
//! redb reads (task localization, symbol batch, Surface/Flow evidence) it
//! feeds into response building.

use super::*;

/// Run an explore intent with explicit intent selection.
///
/// `task_localization` is the read-only default ("where is X?").
/// `behavior_localization` is for change-tasks ("what do I edit to
/// make X happen?") — wider param defaults, otherwise same path.
pub fn explore_with_intent(
    repo: &Path,
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    params: &ExploreParams,
) -> Result<ExploreResponse, ExploreError> {
    let started = std::time::Instant::now();
    let mut effective_params = params.clone();
    // Order: detail widening first (compact → standard/full caps),
    // then intent overrides (behavior_localization wider than
    // task_localization). Intent overrides apply MIN-bound semantics
    // — they widen but never shrink — so order is fine either way,
    // but doing detail first feels like the user-visible flag should
    // dominate.
    let detail = effective_params.detail;
    detail.apply_param_widening(&mut effective_params);
    intent.apply_param_defaults(&mut effective_params);
    let params = &effective_params;

    let discovery_started = std::time::Instant::now();
    let canonical_repo = repo
        .canonicalize()
        .map_err(|e| ExploreError::EngineAnalyzer(format!("canonicalize repo: {e}")))?;
    let repository_discovery_elapsed_us = discovery_started.elapsed().as_micros();
    let store_started = std::time::Instant::now();
    let store = GraphStore::open_read_only(&canonical_repo).map_err(graph_store_explore_error)?;
    let graph_store_open_elapsed_us = store_started.elapsed().as_micros();
    let observability = graph_store_observability(&canonical_repo);
    let query_started = std::time::Instant::now();

    // 1. Graph-derived view (anchors + scope + next).
    let view = task_localize_redb(&store, request)?;

    // 2. Symbol-search evidence. If a local store read fails after the
    //    task view has succeeded, keep going with anchors/text evidence
    //    rather than block the whole request.
    let symbol_queries = extract_symbol_queries(request);
    let symbol_queries = if symbol_queries.len() > params.max_symbol_queries {
        symbol_queries[..params.max_symbol_queries].to_vec()
    } else {
        symbol_queries
    };
    let symbol_matches = if symbol_queries.is_empty() {
        SymbolBatchResults::default()
    } else {
        symbol_batch_redb(&store, &symbol_queries, params.max_symbol_results).unwrap_or_default()
    };

    // 3. Source-text evidence. Runs ripgrep client-side against the repo
    //    filesystem; doesn't need redb. Tolerates ripgrep absence:
    //    we degrade to symbol-only without failing the request.
    let text_terms = extract_text_search_terms(request);
    let text_items = text_search::source_text_files(
        &canonical_repo,
        &text_terms,
        params.max_text_files,
        params.max_text_line_refs,
    );
    let surface_flow = surface_flow_evidence_redb(&store, request, &symbol_queries, &text_terms);

    // 4. Filename-token matches. These are navigation_hints, not
    //    answers — a filename match alone is a "look here next"
    //    signal, not authoritative. Catches the case where the
    //    canonical file's NAME contains the request terms but its
    //    symbols don't (e.g. `suppliers_grader.py` for "find
    //    suppliers grader" — its functions are named
    //    `_default_graders` etc).
    let filename_items =
        filename_token_matches(&canonical_repo, &symbol_queries, params.max_filename_hints);

    // 5. Callsite expansion. For each strong symbol hit, look up
    //    its incoming redb `calls` adjacency and emit `call_site_file`
    //    AnswerItems for the caller files. This is the deepest evidence
    //    layer: not "this file defines X" but "these files actually call
    //    X." A file appearing in BOTH symbol matches AND
    //    someone-else's-callsite is the strongest cross-corroboration we
    //    produce without running tests. Store read failure here degrades
    //    silently, matching the old evidence-layer tolerance.
    let callsite_items = compute_callsite_files(
        &store,
        &symbol_matches,
        params.max_callsite_symbols,
        params.max_callsite_results,
    )
    .unwrap_or_default();

    let query_execution_elapsed_us = query_started.elapsed().as_micros();
    let mut response = build_response_with_surface_flow(
        request,
        intent,
        intent_source,
        &view,
        &symbol_matches,
        &text_items,
        &filename_items,
        &callsite_items,
        &surface_flow,
        params,
        observability,
    );
    if let Some(serde_json::Value::Object(observability)) = response.observability.as_mut() {
        observability.insert(
            "performance".into(),
            serde_json::json!({
                "repository_discovery_elapsed_us": repository_discovery_elapsed_us,
                "graph_store_open_elapsed_us": graph_store_open_elapsed_us,
                "query_execution_elapsed_us": query_execution_elapsed_us,
                "total_elapsed_us": started.elapsed().as_micros(),
                "store_bytes": std::fs::metadata(GraphStore::final_path(&canonical_repo))
                    .ok()
                    .map(|metadata| metadata.len()),
                "peak_memory_bytes": aethyme_graph_storage::peak_memory_bytes(),
            }),
        );
    }
    Ok(response)
}

pub(super) fn task_localize_redb(
    store: &ReadOnlyGraphStore,
    request: &str,
) -> Result<serde_json::Value, ExploreError> {
    let task = TaskInput::from_task_text(request);
    let anchors = task_anchors_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let scope = task_scope_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let next = task_next_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let rendered = crate::json::task_localization_view(&anchors, &scope, &next);
    serde_json::from_str(&rendered)
        .map_err(|e| ExploreError::InvalidResponse(format!("redb task-localize JSON: {e}")))
}

pub(super) fn symbol_batch_redb(
    store: &ReadOnlyGraphStore,
    queries: &[String],
    limit: usize,
) -> Result<SymbolBatchResults, ExploreError> {
    let mut by_query: std::collections::BTreeMap<String, Vec<SymbolHit>> =
        std::collections::BTreeMap::new();
    for query in queries {
        let parsed = symbol_search_redb(store, query, limit)
            .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?
            .into_iter()
            .map(SymbolHit::from_search_hit)
            .collect();
        by_query.insert(query.clone(), parsed);
    }
    Ok(SymbolBatchResults {
        query_order: queries.to_vec(),
        by_query,
    })
}

pub(super) fn surface_flow_evidence_redb(
    store: &ReadOnlyGraphStore,
    request: &str,
    symbol_queries: &[String],
    text_terms: &[String],
) -> SurfaceFlowExploreEvidence {
    let tokens = surface_flow_query_tokens(symbol_queries, text_terms);
    if tokens.is_empty() || !should_query_surface_flow(&tokens) {
        return SurfaceFlowExploreEvidence::default();
    }

    let mut evidence = SurfaceFlowExploreEvidence::default();
    if let Ok(entrypoints) = store.entrypoints_for_task(&tokens) {
        evidence.entrypoints = entrypoints;
    }
    if let Ok(surface_paths) = store.surface_paths_for_behavior(&tokens) {
        evidence.surface_paths = surface_paths;
    }
    if let Ok(credential_flows) = store.credential_flow_candidates(&tokens) {
        evidence.credential_flows = credential_flows;
    }
    if let Ok(coverage) = store.coverage_for_task_class(request) {
        evidence.tests = coverage.tests;
        evidence.coverage_missing = coverage.missing;
    }
    evidence
}

pub(super) fn surface_flow_query_tokens(
    symbol_queries: &[String],
    text_terms: &[String],
) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in symbol_queries.iter().chain(text_terms.iter()) {
        push_unique_token(&mut tokens, token);
    }
    tokens.truncate(16);
    tokens
}

pub(super) fn push_unique_token(tokens: &mut Vec<String>, token: &str) {
    let normalized = token.trim().to_ascii_lowercase();
    if normalized.len() < 3 {
        return;
    }
    if !tokens.iter().any(|existing| existing == &normalized) {
        tokens.push(normalized);
    }
}

pub(super) fn should_query_surface_flow(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        contains_any_text(
            token,
            &[
                "auth",
                "credential",
                "entrypoint",
                "middleware",
                "proxy",
                "route",
                "surface",
                "token",
                "webhook",
                "worker",
            ],
        )
    })
}

#[derive(Debug, Default)]
pub(super) struct SymbolBatchResults {
    /// Original query order — preserves user-intent order across the
    /// alphabetical BTreeMap iteration.
    pub(super) query_order: Vec<String>,
    pub(super) by_query: std::collections::BTreeMap<String, Vec<SymbolHit>>,
}

#[derive(Debug, Clone)]
pub(super) struct SymbolHit {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) file: String,
    pub(super) line: u64,
    pub(super) score: i64,
}

impl SymbolHit {
    fn from_search_hit(hit: SearchHit) -> Self {
        SymbolHit {
            name: hit.name,
            kind: hit.kind,
            file: hit.file,
            line: hit.line as u64,
            score: i64::from(hit.score),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct SurfaceFlowExploreEvidence {
    pub(super) entrypoints: Vec<SurfaceFlowCandidate>,
    pub(super) surface_paths: Vec<SurfacePathCandidate>,
    pub(super) credential_flows: Vec<SurfaceFlowCandidate>,
    pub(super) tests: Vec<NodeDisplay>,
    pub(super) coverage_missing: Vec<String>,
}
