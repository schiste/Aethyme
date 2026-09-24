//! Explore observability: enrichment, compaction, ranking explainability,
//! degraded-ranking reasons and readiness.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn enrich_explore_observability(
    mut observability: serde_json::Value,
    request: &str,
    trust_policy: &TrustPolicy,
    degraded_reasons: &[String],
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
) -> serde_json::Value {
    let top_signals_used = explore_top_signals_used(
        answers,
        nav_hints,
        symbol_items,
        text_items,
        callsite_items,
        surface_flow,
    );
    let top_signals_absent = explore_top_signals_absent(
        request,
        symbol_items,
        text_items,
        callsite_items,
        surface_flow,
        &observability,
    );
    let readiness = explore_observability_readiness(
        request,
        trust_policy,
        &top_signals_used,
        degraded_reasons,
        surface_flow,
        &observability,
    );
    let answer_safe_after_observability = readiness
        .get("answer_safe_after_observability")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let navigation_only_after_observability = readiness
        .get("navigation_only_after_observability")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mode = if answer_safe_after_observability {
        "answer_safe"
    } else if navigation_only_after_observability {
        "navigation_only"
    } else {
        "failed"
    };

    if let Some(obj) = observability.as_object_mut() {
        obj.insert(
            "ranking_explainability".into(),
            serde_json::json!({
                "degraded_ranking_reasons": degraded_reasons,
                "top_signals_used": top_signals_used,
                "top_signals_absent": top_signals_absent,
            }),
        );
        obj.insert(
            "answer_safety".into(),
            serde_json::json!({
                "mode": mode,
                "answer_safe_by_evidence": trust_policy.safe_to_use_as_answer,
                "answer_safe_after_observability": answer_safe_after_observability,
                "navigation_only_after_observability": navigation_only_after_observability,
                "trust_policy": trust_policy.trust_policy,
                "evidence_level": trust_policy.evidence_level,
                "reason": trust_policy.reason,
            }),
        );
        obj.insert("readiness".into(), readiness);
    }

    observability
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compact_explore_observability(
    observability: serde_json::Value,
    request: &str,
    trust_policy: &TrustPolicy,
    degraded_reasons: &[String],
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
) -> serde_json::Value {
    let enriched = enrich_explore_observability(
        observability,
        request,
        trust_policy,
        degraded_reasons,
        answers,
        nav_hints,
        symbol_items,
        text_items,
        callsite_items,
        surface_flow,
    );
    let mut compact = serde_json::Map::new();
    if let Some(value) = enriched.get("graph_store").cloned() {
        compact.insert("graph_store".into(), value);
    }
    if let Some(value) = enriched.get("surface_flow_graph") {
        compact.insert(
            "surface_flow_graph".into(),
            compact_surface_flow_graph(value),
        );
    }
    if let Some(value) = enriched.get("answer_safety").cloned() {
        compact.insert("answer_safety".into(), value);
    }
    if let Some(value) = enriched.get("readiness").cloned() {
        compact.insert("readiness".into(), value);
    }
    if let Some(value) = enriched.get("ranking_explainability") {
        compact.insert(
            "ranking_explainability".into(),
            compact_ranking_explainability(value),
        );
    }
    compact.insert("output_profile".into(), serde_json::json!("agent_compact"));
    compact.insert(
        "full_observability_hint".into(),
        serde_json::json!("rerun with --detail full --show-observability"),
    );
    serde_json::Value::Object(compact)
}

pub(super) fn compact_surface_flow_graph(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    for key in [
        "schema_version",
        "status",
        "source_of_truth",
        "derived_query_artifact",
        "source_path_count_scanned",
        "indexed_path_count_scanned",
        "semantic_fragment_hit_count",
        "source_scan_truncated",
        "indexed_scan_truncated",
        "indexed_languages",
        "indexed_frameworks",
        "surface_type_count",
        "source_present_surface_count",
        "covered_surface_count",
    ] {
        if let Some(child) = value.get(key).cloned() {
            compact.insert(key.to_string(), child);
        }
    }
    if let Some(coverage) = value.get("coverage") {
        compact.insert("coverage".into(), compact_surface_flow_coverage(coverage));
    }
    if let Some(missing) = value.get("missing_expected_surfaces") {
        compact.insert(
            "missing_expected_surfaces".into(),
            compact_missing_expected_surfaces(missing),
        );
    }
    serde_json::Value::Object(compact)
}

pub(super) fn compact_surface_flow_coverage(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    if let Some(entries) = value.as_object() {
        for (surface_type, entry) in entries {
            let mut surface = serde_json::Map::new();
            for key in ["label", "source_present", "indexed", "status"] {
                if let Some(child) = entry.get(key).cloned() {
                    surface.insert(key.to_string(), child);
                }
            }
            compact.insert(surface_type.clone(), serde_json::Value::Object(surface));
        }
    }
    serde_json::Value::Object(compact)
}

pub(super) fn compact_missing_expected_surfaces(value: &serde_json::Value) -> serde_json::Value {
    let Some(items) = value.as_array() else {
        return serde_json::json!([]);
    };
    serde_json::Value::Array(
        items
            .iter()
            .take(AGENT_OUTPUT_MAX_MISSING_SURFACES)
            .map(|item| {
                let mut surface = serde_json::Map::new();
                for key in ["surface_type", "label"] {
                    if let Some(child) = item.get(key).cloned() {
                        surface.insert(key.to_string(), child);
                    }
                }
                serde_json::Value::Object(surface)
            })
            .collect(),
    )
}

pub(super) fn compact_ranking_explainability(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    if let Some(child) = value.get("degraded_ranking_reasons").cloned() {
        compact.insert("degraded_ranking_reasons".to_string(), child);
    }
    if let Some(items) = value
        .get("top_signals_used")
        .and_then(|value| value.as_array())
    {
        compact.insert(
            "top_signals_used".into(),
            compact_signal_items(items, &["signal", "count"]),
        );
    }
    if let Some(items) = value
        .get("top_signals_absent")
        .and_then(|value| value.as_array())
    {
        compact.insert(
            "top_signals_absent".into(),
            compact_signal_items(items, &["signal"]),
        );
    }
    serde_json::Value::Object(compact)
}

pub(super) fn compact_signal_items(
    items: &[serde_json::Value],
    keys: &[&str],
) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .take(AGENT_OUTPUT_MAX_SYMBOLS)
            .map(|item| {
                let mut compact = serde_json::Map::new();
                for key in keys {
                    if let Some(child) = item.get(*key).cloned() {
                        compact.insert((*key).to_string(), child);
                    }
                }
                serde_json::Value::Object(compact)
            })
            .collect(),
    )
}

pub(super) fn explore_top_signals_used(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
) -> Vec<serde_json::Value> {
    let mut counts = std::collections::BTreeMap::new();
    if !answers.is_empty() {
        add_signal_count(&mut counts, "ranked_answer_candidates");
    }
    if !nav_hints.is_empty() {
        add_signal_count(&mut counts, "navigation_hints");
    }
    for item in answers
        .iter()
        .chain(nav_hints.iter())
        .chain(symbol_items.iter())
        .chain(text_items.iter())
        .chain(callsite_items.iter())
    {
        collect_answer_item_signals(item, &mut counts);
    }
    if !surface_flow.entrypoints.is_empty() {
        add_signal_count(&mut counts, "surface_flow_entrypoints");
    }
    if !surface_flow.surface_paths.is_empty() {
        add_signal_count(&mut counts, "surface_flow_paths");
    }
    if !surface_flow.credential_flows.is_empty() {
        add_signal_count(&mut counts, "surface_flow_credential_flows");
    }
    if !surface_flow.tests.is_empty() {
        add_signal_count(&mut counts, "surface_flow_behavior_tests");
    }
    signal_count_values(counts, 14)
}

pub(super) fn collect_answer_item_signals(
    item: &AnswerItem,
    counts: &mut std::collections::BTreeMap<String, usize>,
) {
    match item.kind.as_str() {
        "anchor" | "anchor_area" => add_signal_count(counts, "graph_anchor"),
        "in_scope_file" | "in_scope_area" => add_signal_count(counts, "graph_scope"),
        "source_text_file" => add_signal_count(counts, "source_text_match"),
        "symbol_search_file" | "symbol_search" => add_signal_count(counts, "symbol_search_match"),
        "call_site_file" => add_signal_count(counts, "callsite_adjacency"),
        "filesystem_file" => add_signal_count(counts, "filesystem_filename_match"),
        _ => {}
    }
    collect_evidence_signals(&item.evidence, counts);
}

pub(super) fn collect_evidence_signals(
    evidence: &serde_json::Value,
    counts: &mut std::collections::BTreeMap<String, usize>,
) {
    if let Some(source) = evidence.get("source").and_then(|value| value.as_str()) {
        match source {
            "source-text-search" => add_signal_count(counts, "source_text_match"),
            "task-localize.anchors" => add_signal_count(counts, "graph_anchor"),
            "task-localize.scope" => add_signal_count(counts, "graph_scope"),
            other => add_signal_count(counts, format!("evidence_source:{other}")),
        }
    }
    if evidence
        .get("line_refs")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "source_line_refs");
    }
    if evidence
        .get("matched_terms")
        .and_then(|value| value.as_array())
        .is_some_and(|items| items.len() >= 2)
    {
        add_signal_count(counts, "multi_term_source_text");
    }
    if evidence
        .get("matched_queries")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "symbol_name_match");
    }
    if evidence
        .get("matched_queries")
        .and_then(|value| value.as_array())
        .is_some_and(|items| items.len() >= 2)
    {
        add_signal_count(counts, "multi_query_symbol_match");
    }
    if evidence
        .get("symbols")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "callsite_or_symbol_rows");
    }
    if let Some(signals) = evidence
        .get("ranking_signals")
        .and_then(|value| value.as_array())
    {
        for signal in signals.iter().filter_map(|value| value.as_str()) {
            add_signal_count(counts, format!("ranking_signal:{signal}"));
        }
    }
    if let Some(symbol_evidence) = evidence.get("also_symbol_search") {
        collect_evidence_signals(symbol_evidence, counts);
    }
}

pub(super) fn explore_top_signals_absent(
    request: &str,
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
    observability: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut absent = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if graph_freshness_status(observability) != Some("fresh") {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "fresh_graph_store",
            "The redb graph store is missing, stale, or freshness could not be proven.",
        );
    }
    if surface_flow_relevant_for_request(request, surface_flow)
        && !surface_flow_complete_for_request(observability)
    {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "complete_surface_flow_coverage",
            "The request depends on ingress/middleware/credential surfaces, but coverage is partial or unknown.",
        );
    }
    if symbol_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "symbol_search_match",
            "No redb symbol candidates matched the bounded query set.",
        );
    } else if !has_multi_query_symbol_file(symbol_items) {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "multi_query_symbol_match",
            "Symbol evidence matched only single request terms per file.",
        );
    }
    if text_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "source_text_match",
            "No bounded source-text candidate matched enough request terms.",
        );
    }
    if !has_symbol_text_corroboration(symbol_items, text_items) {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "symbol_text_corroboration",
            "No candidate file was confirmed by both multi-query symbol search and multi-term source text.",
        );
    }
    if callsite_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "callsite_adjacency",
            "No caller/callee expansion evidence was emitted for the ranked symbols.",
        );
    }
    absent.into_iter().take(14).collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn explore_degraded_ranking_reasons(
    request: &str,
    trust_policy: &TrustPolicy,
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
    observability: &serde_json::Value,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if answers.is_empty() && nav_hints.is_empty() {
        push_unique_reason(&mut reasons, "no_ranked_candidates");
    } else if !trust_policy.safe_to_use_as_answer {
        push_unique_reason(
            &mut reasons,
            "navigation_only_without_authoritative_evidence",
        );
    }
    if !trust_policy.safe_to_use_as_answer
        && !has_symbol_text_corroboration(symbol_items, text_items)
    {
        push_unique_reason(&mut reasons, "missing_symbol_text_corroboration");
    }
    if !trust_policy.safe_to_use_as_answer && symbol_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_symbol_search_evidence");
    }
    if !trust_policy.safe_to_use_as_answer && text_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_source_text_evidence");
    }
    if !trust_policy.safe_to_use_as_answer && callsite_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_callsite_evidence");
    }
    if !surface_flow.coverage_missing.is_empty() {
        push_unique_reason(&mut reasons, "surface_flow_task_coverage_missing");
    }
    match graph_freshness_status(observability) {
        Some("fresh") => {}
        Some(status) => push_unique_reason(&mut reasons, format!("graph_store_{status}")),
        None => push_unique_reason(&mut reasons, "graph_store_status_unknown"),
    }
    if surface_flow_relevant_for_request(request, surface_flow)
        && !surface_flow_complete_for_request(observability)
    {
        push_unique_reason(&mut reasons, "surface_flow_coverage_not_complete_enough");
    }
    reasons
}

pub(super) fn explore_observability_readiness(
    request: &str,
    trust_policy: &TrustPolicy,
    top_signals_used: &[serde_json::Value],
    degraded_reasons: &[String],
    surface_flow: &SurfaceFlowExploreEvidence,
    observability: &serde_json::Value,
) -> serde_json::Value {
    let graph_status = graph_freshness_status(observability).unwrap_or("unknown");
    let graph_fresh = graph_status == "fresh";
    let surface_status = surface_flow_status(observability).unwrap_or("unknown");
    let surface_relevant = surface_flow_relevant_for_request(request, surface_flow);
    let surface_complete = surface_flow_complete_for_request(observability);
    let complete_enough = if surface_relevant {
        surface_complete
    } else {
        graph_status != "missing"
    };
    let explainable = !top_signals_used.is_empty();
    let answer_safe_after_observability =
        trust_policy.safe_to_use_as_answer && graph_fresh && complete_enough && explainable;
    let navigation_only_after_observability =
        trust_policy.safe_to_use_as_navigation && !answer_safe_after_observability;
    let status = if answer_safe_after_observability {
        "answer_safe"
    } else if navigation_only_after_observability {
        "navigation_only"
    } else {
        "degraded"
    };

    serde_json::json!({
        "status": status,
        "fresh_enough": graph_fresh,
        "complete_enough": complete_enough,
        "surface_flow_relevant": surface_relevant,
        "surface_flow_complete": surface_complete,
        "explainable": explainable,
        "answer_safe_by_evidence": trust_policy.safe_to_use_as_answer,
        "answer_safe_after_observability": answer_safe_after_observability,
        "navigation_only_after_observability": navigation_only_after_observability,
        "graph_freshness_status": graph_status,
        "surface_flow_graph_status": surface_status,
        "degraded_reasons": degraded_reasons,
    })
}

pub(super) fn surface_flow_relevant_for_request(
    request: &str,
    surface_flow: &SurfaceFlowExploreEvidence,
) -> bool {
    !surface_flow.entrypoints.is_empty()
        || !surface_flow.surface_paths.is_empty()
        || !surface_flow.credential_flows.is_empty()
        || contains_any_text(
            &request.to_ascii_lowercase(),
            &[
                "credential",
                "entrypoint",
                "middleware",
                "proxy",
                "route",
                "surface",
                "webhook",
                "worker",
            ],
        )
}

pub(super) fn surface_flow_complete_for_request(observability: &serde_json::Value) -> bool {
    let missing_count = observability
        .get("missing_expected_surfaces")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    matches!(
        surface_flow_status(observability),
        Some("covered") | Some("no_surface_signals")
    ) && missing_count == 0
}

pub(super) fn graph_freshness_status(observability: &serde_json::Value) -> Option<&str> {
    observability
        .get("graph_freshness")
        .or_else(|| observability.get("graph_store"))
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
}

pub(super) fn surface_flow_status(observability: &serde_json::Value) -> Option<&str> {
    observability
        .get("surface_flow_graph")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
}

pub(super) fn has_multi_query_symbol_file(symbol_items: &[AnswerItem]) -> bool {
    symbol_items.iter().any(|item| {
        item.evidence
            .get("matched_queries")
            .and_then(|value| value.as_array())
            .is_some_and(|items| items.len() >= 2)
    })
}

pub(super) fn has_symbol_text_corroboration(
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
) -> bool {
    let symbol_paths: std::collections::BTreeSet<&str> = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|value| value.as_array())
                .is_some_and(|items| items.len() >= 2)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    text_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_terms")
                .and_then(|value| value.as_array())
                .is_some_and(|items| items.len() >= 2)
        })
        .filter_map(|item| item.path.as_deref())
        .any(|path| symbol_paths.contains(path))
}

pub(super) fn add_signal_count(
    counts: &mut std::collections::BTreeMap<String, usize>,
    signal: impl Into<String>,
) {
    let signal = signal.into();
    if !signal.trim().is_empty() {
        *counts.entry(signal).or_insert(0) += 1;
    }
}

pub(super) fn signal_count_values(
    counts: std::collections::BTreeMap<String, usize>,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left_signal, left_count), (right_signal, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_signal.cmp(right_signal))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(signal, count)| serde_json::json!({"signal": signal, "count": count}))
        .collect()
}

pub(super) fn push_absent_signal(
    absent: &mut Vec<serde_json::Value>,
    seen: &mut std::collections::BTreeSet<String>,
    signal: &str,
    reason: &str,
) {
    if seen.insert(signal.to_string()) {
        absent.push(serde_json::json!({
            "signal": signal,
            "reason": reason,
        }));
    }
}

pub(super) fn push_unique_reason(reasons: &mut Vec<String>, reason: impl Into<String>) {
    let reason = reason.into();
    if !reasons.iter().any(|existing| existing == &reason) {
        reasons.push(reason);
    }
}
