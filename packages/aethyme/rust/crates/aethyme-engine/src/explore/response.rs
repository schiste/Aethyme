//! Response building: turning graph, symbol, text, filename and call-site
//! evidence into the answer-json envelope, plus output budgets, output
//! adapters and verification steps.

use super::*;

pub(super) const AGENT_OUTPUT_MAX_ANSWER_ITEMS: usize = 4;
pub(super) const AGENT_OUTPUT_MAX_NAVIGATION_HINTS: usize = 0;
pub(super) const AGENT_OUTPUT_MAX_DEGRADED_REASONS: usize = 6;
pub(super) const AGENT_OUTPUT_MAX_NEXT_ACTIONS: usize = 3;
pub(super) const AGENT_OUTPUT_MAX_VERIFICATION_STEPS: usize = 2;
pub(super) const AGENT_OUTPUT_MAX_EVIDENCE_ARRAY_ITEMS: usize = 4;
pub(super) const AGENT_OUTPUT_MAX_RANKING_SIGNALS: usize = 4;
pub(super) const AGENT_OUTPUT_MAX_MATCHED_TERMS: usize = 6;
pub(super) const AGENT_OUTPUT_MAX_MATCHED_QUERIES: usize = 4;
pub(super) const AGENT_OUTPUT_MAX_SYMBOLS: usize = 3;
pub(super) const AGENT_OUTPUT_MAX_LINE_REFS: usize = 2;
pub(super) const AGENT_OUTPUT_MAX_MISSING_SURFACES: usize = 8;

// ── response synthesis ──────────────────────────────────────────────────

pub(super) fn contains_any_text(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Translate the redb task-localize view into the answer-json
/// envelope the agent contract expects.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn build_response(
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    view: &serde_json::Value,
    symbol_matches: &SymbolBatchResults,
    text_items: &[AnswerItem],
    filename_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    params: &ExploreParams,
    observability: serde_json::Value,
) -> ExploreResponse {
    build_response_with_surface_flow(
        request,
        intent,
        intent_source,
        view,
        symbol_matches,
        text_items,
        filename_items,
        callsite_items,
        &SurfaceFlowExploreEvidence::default(),
        params,
        observability,
    )
}

/// Translate the redb task-localize view into the answer-json
/// envelope the agent contract expects. Surface/Flow evidence feeds the
/// observability and degraded-reason signals only; it does not rank answers.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_response_with_surface_flow(
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    view: &serde_json::Value,
    symbol_matches: &SymbolBatchResults,
    text_items: &[AnswerItem],
    filename_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
    params: &ExploreParams,
    observability: serde_json::Value,
) -> ExploreResponse {
    let anchors = view
        .get("anchors")
        .and_then(|a| a.get("anchors"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let scope = view.get("scope");
    let in_scope_files: Vec<String> = scope
        .and_then(|s| s.get("in_scope_files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let in_scope_areas: Vec<String> = scope
        .and_then(|s| s.get("in_scope_areas"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Synthesize answer items.
    //
    // Insertion order = priority order (each loop respects the
    // `max_answer_items` cap and skips paths already added):
    //
    //   1. source_text_file  — line-level evidence, strongest signal
    //   2. symbol_search_file — name-match evidence
    //   3. anchor             — graph-derived seed (heuristic, weaker)
    //   4. in_scope_file      — area-membership-only (weakest)
    //
    // This matters: anchors are heuristic seeds (e.g. "package.json"
    // matched a generic config-anchor weight). They're weaker
    // evidence than a line that literally contains the request's
    // terms in executable code. Putting them last among
    // answer-track items reflects that.
    //
    // anchors with `kind = "folder" | "area"` and in_scope_areas
    // are routed to `navigation_hints[]` because the agent is asking
    // for FILES to act on, not directories.
    let mut answers: Vec<AnswerItem> = Vec::new();
    let mut nav_hints: Vec<AnswerItem> = Vec::new();
    // Anchors that should land in `answer[]` rather than
    // `navigation_hints[]`. Renamed from `anchor_file_items` on
    // 2026-05-12 when `"symbol"` anchors started being promoted
    // alongside `"file"` anchors. Same merge pass; same downstream
    // handling (kind="anchor"); the `evidence.anchor_kind` field
    // distinguishes which sub-kind produced the item.
    let mut anchor_items: Vec<AnswerItem> = Vec::new();

    for anchor in &anchors {
        let kind = anchor.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let id = anchor.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let file = anchor.get("file").and_then(|v| v.as_str());
        let reason = anchor
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("anchor match")
            .to_string();
        match kind {
            "file" => {
                let path = file.map(String::from).or_else(|| Some(id.to_string()));
                anchor_items.push(AnswerItem {
                    kind: "anchor".into(),
                    target: id.to_string(),
                    path,
                    status: "candidate".into(),
                    confidence: 0.85,
                    reason,
                    role: "anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": "file",
                    }),
                });
            }
            "symbol" => {
                // Promote symbol-kind anchors into `answer[]` as
                // first-class candidates (2026-05-12). Pre-fix these
                // landed in `navigation_hints[]` via the `other` arm
                // — agents reading only `answer[]` never saw them,
                // even though symbol-name match is at least as
                // specific as filename match.
                //
                // Confidence 0.80: slightly below file anchors
                // (0.85) to acknowledge that today's `symbol_search`
                // is token-substring-based and can produce noisy
                // matches (e.g. "marks → GrammarKsh"). When the
                // symbol-search ranking gets stricter (a separate
                // follow-up), this can move to 0.85 — one-line
                // change. The intermediate value also lets the
                // trust_policy machinery flag symbol anchors as
                // "candidate but verify" without special-casing.
                //
                // Dedup at merge step (path-based) means symbol
                // anchors for files ALREADY in `answer[]` via
                // text-match are dropped silently. The real impact
                // is on files NOT yet in answer[] — the long tail
                // of graph-derived candidates that text-match
                // missed.
                let path = file.map(String::from);
                anchor_items.push(AnswerItem {
                    kind: "anchor".into(),
                    target: id.to_string(),
                    path,
                    status: "candidate".into(),
                    confidence: 0.80,
                    reason,
                    role: "anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": "symbol",
                    }),
                });
            }
            "folder" | "area" => {
                nav_hints.push(AnswerItem {
                    kind: "anchor_area".into(),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.6,
                    reason,
                    role: "navigation_anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": kind,
                    }),
                });
            }
            other if !other.is_empty() => {
                nav_hints.push(AnswerItem {
                    kind: format!("anchor_{other}"),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.55,
                    reason,
                    role: other.to_string(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": other,
                    }),
                });
            }
            _ => {}
        }
    }

    // Slot budgeting: if symbol search has multi-query hits, reserve up
    // to 2 slots so they always land in `answer[]` even when text
    // matches are plentiful. Without this, a query like "find suppliers
    // grader scoring logic" gets 5 weak text matches and zero symbol
    // matches in the response — even when the most relevant file
    // (suppliers_grader.py) was found by symbol search.
    //
    // The reservation is conservative: only ≥2 slots, only when symbol
    // has multi-query hits. Single-query symbol matches stay weakly
    // ranked.
    let symbol_items = build_symbol_file_items(symbol_matches, params.max_symbol_files);
    let multi_query_symbol_count = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .count();
    let symbol_reserved = multi_query_symbol_count.min(2);
    let text_budget = params.max_answer_items.saturating_sub(symbol_reserved);

    for item in text_items.iter().take(text_budget.max(1)) {
        if answers.len() >= text_budget {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &symbol_items {
        if let Some(existing) = answers
            .iter_mut()
            .find(|a| a.path.as_deref() == item.path.as_deref())
        {
            merge_symbol_search_evidence(existing, item);
            continue;
        }
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    // Callsite evidence: files that CALL one of our candidate symbols.
    // Ranks just after symbol_search_file because "this file calls X"
    // is behavioural evidence (similar strength to "this file's
    // source-text contains X's name"). Multi-symbol callsites
    // (rank ≥0.86 confidence) often surface dispatch hubs the agent
    // cares about more than the symbol's home file.
    for item in callsite_items {
        // Always allow merging into an existing answer (no new slot
        // consumed); only the push-new branch checks the cap.
        if let Some(existing) = answers
            .iter_mut()
            .find(|a| a.path.as_deref() == item.path.as_deref())
        {
            // Pull symbols list from the callsite item's evidence and
            // attach it to the existing item under `also_callsite_for`.
            // Bump confidence by a small amount (capped at 0.9) since
            // multiple corroborating sources increase trust.
            if let Some(syms) = item.evidence.get("symbols").cloned()
                && let Some(obj) = existing.evidence.as_object_mut()
            {
                obj.insert("also_callsite_for".to_string(), syms);
                obj.insert(
                    "callsite_hit_count".to_string(),
                    item.evidence
                        .get("hit_count")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            existing.confidence = ((existing.confidence + 0.05).min(0.9) * 100.0).round() / 100.0;
            continue;
        }
        if answers.len() >= params.max_answer_items {
            continue;
        }
        answers.push(item.clone());
    }

    // Backfill text again now that symbol items have landed — the
    // budget cap above held remaining text out; if there's room left
    // (no symbol items, or symbol items dedup'd against text), let
    // text fill the rest.
    for item in text_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &anchor_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for file in &in_scope_files {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == Some(file.as_str()))
        {
            continue;
        }
        answers.push(AnswerItem {
            kind: "in_scope_file".into(),
            target: file.clone(),
            path: Some(file.clone()),
            status: "candidate".into(),
            confidence: 0.7,
            reason: "Within graph-derived scope for this task".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    for area in &in_scope_areas {
        if nav_hints.iter().any(|h| h.target == *area) {
            continue;
        }
        nav_hints.push(AnswerItem {
            kind: "in_scope_area".into(),
            target: area.clone(),
            path: None,
            status: "navigation_only".into(),
            confidence: 0.5,
            reason: "In-scope area suggested by graph navigation".into(),
            role: "navigation_area".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    // Filename-token matches: navigation hints, NOT answers. The
    // contract is "look here next" rather than "this IS the answer".
    // Skip files that already appear in answer[] (those have stronger
    // evidence and the agent has them in context already).
    for item in filename_items {
        if nav_hints
            .iter()
            .any(|h| h.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        nav_hints.push(item.clone());
    }

    // Cap answer count after dedup so we hit the user's intent for
    // `max_answer_items` exactly.
    answers.truncate(params.max_answer_items);

    let mut output_budget = OutputBudgetReport::default();
    if agent_output_profile(params) {
        apply_agent_output_budget(&mut answers, &mut nav_hints, &mut output_budget);
    }

    let answer_count = answers.len();
    let navigation_hint_count = nav_hints.len();

    // Confidence summary: trivial bucketing on the answer items.
    let answer_summary = bucket_confidence(&answers);

    // Trust policy. Tightens as more evidence sources land:
    //
    //   session 1: anchors + scope only          → needs_verification
    //   session 2: + symbol search                → answer_candidate when
    //                                              ≥2 distinct query terms
    //                                              matched in the same file
    //   session 3 (this commit):
    //              + source-text + corroboration → answer_candidate raised
    //                                              when text + symbol agree
    //                                              on the same file (the
    //                                              strongest signal short
    //                                              of running the test
    //                                              suite); weaker shapes
    //                                              degrade gracefully.
    //   session 4+: callsite expansion            → tighter still
    let high_confidence_count = answers.iter().filter(|a| a.confidence >= 0.85).count();
    let multi_query_symbol_files: Vec<&str> = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    let strong_text_files: Vec<&str> = text_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_terms")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    // Symbol + text agree on the same file = a strong local signal.
    let cross_corroborated: Vec<&&str> = multi_query_symbol_files
        .iter()
        .filter(|p| strong_text_files.contains(p))
        .collect();

    // Callsite evidence raises trust further: a file with multi-query
    // symbol matches AND callsite evidence is approaching test-suite
    // territory. We track multi-symbol callsite files separately
    // because that's the strongest dispatch signal we produce.
    let strong_callsite_files: Vec<&str> = callsite_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("symbols")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    let triple_corroborated: bool = !strong_callsite_files.is_empty()
        && (!cross_corroborated.is_empty() || !multi_query_symbol_files.is_empty());

    let policy_kind = if answers.is_empty() && nav_hints.is_empty() {
        "failed"
    } else if !cross_corroborated.is_empty() || !multi_query_symbol_files.is_empty() {
        "answer_candidate"
    } else if !text_items.is_empty() || !symbol_items.is_empty() {
        // Some text or symbol evidence but not strong enough to defend.
        "needs_verification"
    } else {
        "needs_verification"
    };
    let evidence_level = if triple_corroborated {
        "graph+symbol+text+callsite"
    } else if !cross_corroborated.is_empty() {
        "graph+symbol+text"
    } else if !strong_callsite_files.is_empty() {
        "graph+symbol+callsite"
    } else if !multi_query_symbol_files.is_empty() && !text_items.is_empty() {
        "graph+symbol+text-weak"
    } else if !multi_query_symbol_files.is_empty() {
        "graph+symbol"
    } else if !callsite_items.is_empty() {
        "graph+callsite-weak"
    } else if !text_items.is_empty() {
        "graph+text"
    } else if !symbol_items.is_empty() {
        "graph+symbol-weak"
    } else {
        "graph"
    };
    let safe_to_use_as_answer = matches!(policy_kind, "answer_candidate");
    let trust_reason = match policy_kind {
        "answer_candidate" if !cross_corroborated.is_empty() => format!(
            "Symbol search and source-text both matched {} candidate file(s); \
             cross-corroborated evidence treated as authoritative.",
            cross_corroborated.len()
        ),
        "answer_candidate" => format!(
            "Symbol search matched {} distinct request terms in the same \
             file; treating as authoritative answer candidate.",
            symbol_items
                .iter()
                .filter_map(|item| item
                    .evidence
                    .get("matched_queries")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len()))
                .max()
                .unwrap_or(2)
        ),
        "failed" => "No anchors, in-scope files, symbol matches, or source-text hits.".to_string(),
        _ => "Evidence present but not strong enough to defend as an \
              authoritative answer. Verify before acting."
            .to_string(),
    };
    let mut trust_policy = TrustPolicy {
        safe_to_use_as_answer,
        safe_to_use_as_navigation: !answers.is_empty() || !nav_hints.is_empty(),
        evidence_level: evidence_level.to_string(),
        authoritative_answer_count: high_confidence_count,
        navigation_hint_count,
        degraded: false,
        trust_policy: policy_kind,
        reason: trust_reason,
    };
    let mut degraded_reasons = explore_degraded_ranking_reasons(
        request,
        &trust_policy,
        &answers,
        &nav_hints,
        &symbol_items,
        text_items,
        callsite_items,
        surface_flow,
        &observability,
    );
    trust_policy.degraded = !degraded_reasons.is_empty();

    let status = if answers.is_empty() && nav_hints.is_empty() {
        "degraded"
    } else {
        "complete"
    };

    let mut next_actions = if answers.is_empty() && nav_hints.is_empty() {
        vec![
            "Refine the request — graph navigation found no anchors.".into(),
            "Try a more specific keyword from the codebase domain.".into(),
        ]
    } else {
        vec![
            "Read the top answer[] item to verify it matches the task.".into(),
            "If unsure, run `aethyme explore --detail standard` for richer \
             evidence (symbol search + source-text)."
                .into(),
        ]
    };

    // Compute fields that need by-ref reads BEFORE moving the values
    // into the response struct.
    let safe_to_use_as_answer = trust_policy.safe_to_use_as_answer;
    let safe_to_use_as_navigation = trust_policy.safe_to_use_as_navigation;
    let mut verification_steps =
        build_verification_steps(&answers, &nav_hints, &trust_policy, text_items);

    // Build output_adapters and resolved_parameters only when the
    // caller has asked for the explicit full profile. `--show-observability`
    // in compact/standard now emits a compact trust/coverage block instead
    // of the full debug envelope; agents need safety, coverage, and warnings on
    // the first call, not adapters and long path-hint arrays.
    let full_profile = matches!(params.detail, Detail::Full);

    // At the agent-facing profile, truncate tail arrays to keep the first
    // Explore call under the command-output budget. Full detail remains the
    // deliberate escape hatch for exhaustive debugging.
    if agent_output_profile(params) {
        apply_agent_tail_budget(
            &mut degraded_reasons,
            &mut verification_steps,
            &mut next_actions,
            &mut output_budget,
        );
    }

    let output_adapters = if full_profile {
        Some(build_output_adapters(
            &answers,
            &nav_hints,
            &next_actions,
            &verification_steps,
            params.detail,
        ))
    } else {
        None
    };
    let resolved_parameters = if full_profile {
        Some(params.to_json())
    } else {
        None
    };
    let observability = if full_profile {
        Some(enrich_explore_observability(
            observability,
            request,
            &trust_policy,
            &degraded_reasons,
            &answers,
            &nav_hints,
            &symbol_items,
            text_items,
            callsite_items,
            surface_flow,
        ))
    } else if params.show_observability {
        Some(compact_explore_observability(
            observability,
            request,
            &trust_policy,
            &degraded_reasons,
            &answers,
            &nav_hints,
            &symbol_items,
            text_items,
            callsite_items,
            surface_flow,
        ))
    } else {
        None
    };

    // Post-conditions for `answers[]`. These are debug-only; they
    // document the response contract that a downstream agent or scoring
    // pipeline can rely on.
    //
    //   - cap: `answers.len() <= max_answer_items`. The dedup loop
    //     enforces this on every push; this assert guards against a
    //     future contributor adding an unguarded `answers.push(...)`
    //     without a cap check.
    //   - distinct paths: no two items share the same `Some(path)`.
    //     The merge-into-existing branch in the callsite dedup loop
    //     depends on this — if the same path appeared twice, only
    //     the first match would receive merged evidence.
    //   - kinds belong to the answer-track set (no nav_hint kinds
    //     leaking into `answer[]`).
    debug_assert!(
        answers.len() <= params.max_answer_items,
        "answer cap violated: {} > {}",
        answers.len(),
        params.max_answer_items
    );
    {
        let mut paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for item in &answers {
            if let Some(p) = item.path.as_deref() {
                debug_assert!(
                    paths.insert(p),
                    "duplicate path in answers[]: {p:?}; dedup contract violated"
                );
            }
        }
    }
    debug_assert!(
        answers.iter().all(|item| matches!(
            item.kind.as_str(),
            "anchor"
                | "in_scope_file"
                | "in_scope_symbol"
                | "symbol_search"
                | "symbol_search_file"
                | "source_text_file"
                | "call_site_file"
                | "filesystem_file"
        )),
        "answer item with unexpected kind: {:?}",
        answers.iter().map(|i| i.kind.as_str()).collect::<Vec<_>>()
    );

    let response = ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent: intent.as_str(),
        intent_source: intent_source.as_str(),
        status,
        request: ExploreRequest {
            raw: request.to_string(),
            parameters: serde_json::Value::Object(serde_json::Map::new()),
        },
        answer: answers,
        navigation_hints: nav_hints,
        excluded: Vec::new(),
        // `ambiguous` and `subsystems` stay in the envelope because
        // cross-process readers (`explore-summary`, `verify-targets`, the
        // eval harness) read them. The task-localization path has no
        // producer for either; the bounded source fallback fills
        // `subsystems` when the graph is unavailable.
        ambiguous: Vec::new(),
        subsystems: Vec::new(),
        evidence: Evidence {
            answer_count,
            navigation_hint_count,
            excluded_count: 0,
        },
        confidence: Confidence {
            // Aggregate "overall" confidence is intentionally None for the
            // task/behavior path: each AnswerItem carries its own
            // confidence, and a single weighted aggregate would obscure
            // the distinction between "one strong + four weak" and "five
            // medium." Consumers should read per-item confidence + the
            // trust_policy verdict.
            overall: None,
            answer_summary,
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({}),
        },
        safe_to_use_as_answer,
        safe_to_use_as_navigation,
        trust_policy,
        degraded_reasons,
        verification_steps,
        next_actions,
        available_specialized_intents: vec!["behavior_localization_query", "usage_boundary_query"],
        output_chars_estimate: 0,
        truncated: output_budget.truncated,
        output_adapters,
        resolved_parameters,
        observability,
    };
    response_with_output_estimate(response)
}

#[derive(Debug, Default)]
pub(super) struct OutputBudgetReport {
    pub(super) truncated: bool,
}

pub(super) fn agent_output_profile(params: &ExploreParams) -> bool {
    !matches!(params.detail, Detail::Full) && (params.show_observability || params.depth.is_some())
}

pub(super) fn cap_vec<T>(items: &mut Vec<T>, max: usize, report: &mut OutputBudgetReport) {
    if items.len() > max {
        items.truncate(max);
        report.truncated = true;
    }
}

pub(super) fn apply_agent_output_budget(
    answers: &mut Vec<AnswerItem>,
    nav_hints: &mut Vec<AnswerItem>,
    report: &mut OutputBudgetReport,
) {
    cap_vec(answers, AGENT_OUTPUT_MAX_ANSWER_ITEMS, report);
    cap_vec(nav_hints, AGENT_OUTPUT_MAX_NAVIGATION_HINTS, report);

    for item in answers.iter_mut().chain(nav_hints.iter_mut()) {
        budget_answer_item(item, report);
    }
}

pub(super) fn apply_agent_tail_budget(
    degraded_reasons: &mut Vec<String>,
    verification_steps: &mut Vec<serde_json::Value>,
    next_actions: &mut Vec<String>,
    report: &mut OutputBudgetReport,
) {
    cap_vec(degraded_reasons, AGENT_OUTPUT_MAX_DEGRADED_REASONS, report);
    cap_vec(
        verification_steps,
        AGENT_OUTPUT_MAX_VERIFICATION_STEPS,
        report,
    );
    cap_vec(next_actions, AGENT_OUTPUT_MAX_NEXT_ACTIONS, report);
}

pub(super) fn budget_answer_item(item: &mut AnswerItem, report: &mut OutputBudgetReport) {
    budget_evidence_value(&mut item.evidence, None, report);
}

pub(super) fn budget_evidence_value(
    value: &mut serde_json::Value,
    key: Option<&str>,
    report: &mut OutputBudgetReport,
) {
    match value {
        serde_json::Value::Array(items) => {
            let max = match key {
                Some("ranking_signals") => AGENT_OUTPUT_MAX_RANKING_SIGNALS,
                Some("matched_terms") => AGENT_OUTPUT_MAX_MATCHED_TERMS,
                Some("matched_queries") => AGENT_OUTPUT_MAX_MATCHED_QUERIES,
                Some("symbols") => AGENT_OUTPUT_MAX_SYMBOLS,
                Some("line_refs") => AGENT_OUTPUT_MAX_LINE_REFS,
                _ => AGENT_OUTPUT_MAX_EVIDENCE_ARRAY_ITEMS,
            };
            cap_vec(items, max, report);
            for item in items {
                budget_evidence_value(item, None, report);
            }
        }
        serde_json::Value::Object(map) => {
            for (child_key, child_value) in map {
                budget_evidence_value(child_value, Some(child_key.as_str()), report);
            }
        }
        _ => {}
    }
}

pub(super) fn response_with_output_estimate(mut response: ExploreResponse) -> ExploreResponse {
    response.output_chars_estimate = serde_json::to_string_pretty(&response)
        .map(|json| json.len())
        .unwrap_or(0);
    response
}

/// Build the `output_adapters.task_localization_json` structure that
/// downstream consumers (skills, eval scoring, agent post-processing)
/// read instead of poking through the heterogeneous `answer[]` list.
///
/// Filtering rules mirror Python at `cli.py:2088-2118`:
///   - candidate_files  → kinds {symbol_search_file, source_text_file,
///     call_site_file, filesystem_file, anchor, in_scope_file} that have
///     a `path`.
///   - candidate_symbols → kinds {symbol_search, in_scope_symbol} OR
///     items whose evidence carries `anchor_kind == "symbol"`.
///   - navigation_hints → empty when `detail == compact`; otherwise
///     echoes the response's nav_hints.
pub(super) fn build_output_adapters(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    next_actions: &[String],
    verification_steps: &[serde_json::Value],
    detail: Detail,
) -> serde_json::Value {
    let candidate_files: Vec<&AnswerItem> = answers
        .iter()
        .filter(|item| item.path.is_some())
        .filter(|item| {
            matches!(
                item.kind.as_str(),
                "symbol_search_file"
                    | "source_text_file"
                    | "call_site_file"
                    | "filesystem_file"
                    | "anchor"
                    | "in_scope_file"
            )
        })
        .collect();
    let candidate_symbols: Vec<&AnswerItem> = answers
        .iter()
        .filter(|item| {
            matches!(item.kind.as_str(), "symbol_search" | "in_scope_symbol")
                || item.evidence.get("anchor_kind").and_then(|v| v.as_str()) == Some("symbol")
        })
        .collect();
    let navigation_hints_field: Vec<&AnswerItem> = match detail {
        Detail::Compact => Vec::new(),
        _ => nav_hints.iter().collect(),
    };
    serde_json::json!({
        "task_localization_json": {
            "candidate_files": candidate_files,
            "candidate_symbols": candidate_symbols,
            "next_actions": next_actions,
            "verification_steps": verification_steps,
            "navigation_hints": navigation_hints_field,
        }
    })
}

pub(super) fn merge_symbol_search_evidence(existing: &mut AnswerItem, symbol_item: &AnswerItem) {
    let mut merged = serde_json::Map::new();
    for key in [
        "matched_queries",
        "symbols",
        "combined_score",
        "ranking_bonus",
        "ranking_signals",
    ] {
        if let Some(value) = symbol_item.evidence.get(key).cloned() {
            merged.insert(key.to_string(), value);
        }
    }
    if merged.is_empty() {
        return;
    }
    if let Some(obj) = existing.evidence.as_object_mut() {
        obj.insert(
            "also_symbol_search".to_string(),
            serde_json::Value::Object(merged),
        );
    }
}

/// Tailor the verification steps to the evidence we actually produced.
///
/// Mirrors the philosophy of Python's
/// `_task_localization_verification_steps`: the steps an agent should
/// take depend on what we found and how confident we are. Generic
/// "verify before acting" is honest but unhelpful; pointing at a
/// specific line ref or symbol gives the agent a concrete thing to do.
///
/// Step priority (we emit at most 4):
///   1. If text evidence with line_refs → read the cited line(s)
///   2. If symbol evidence → grep callers/dispatch sites of the symbol
///   3. If failed/no answers → suggest broadening the request or increasing
///      native detail for richer evidence
///   4. Generic "open top answer and confirm" as a final fallback
pub(super) fn build_verification_steps(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    trust_policy: &TrustPolicy,
    text_items: &[AnswerItem],
) -> Vec<serde_json::Value> {
    let mut steps: Vec<serde_json::Value> = Vec::new();

    // Step 1: cite a specific line ref the agent can read.
    if let Some(top_text) = text_items.first()
        && let Some(line_refs) = top_text
            .evidence
            .get("line_refs")
            .and_then(|v| v.as_array())
        && let Some(first_ref) = line_refs.first()
    {
        let line = first_ref.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let path = top_text.path.as_deref().unwrap_or("(unknown)");
        steps.push(serde_json::json!({
            "step": format!(
                "Read {}:{} and confirm the matched terms appear in \
                 executable code (not a comment or stringified \
                 translation).",
                path, line
            ),
            "rationale": "Source-text evidence is line-level; verifying \
                          the line context takes one Read tool call.",
        }));
    }

    // Step 2: when symbol-search anchored a file (different from text
    // top), suggest checking the symbol's call sites.
    let symbol_file = answers
        .iter()
        .find(|a| a.evidence.get("also_symbol_search").is_some())
        .or_else(|| answers.iter().find(|a| a.kind == "symbol_search_file"))
        .or_else(|| nav_hints.iter().find(|h| h.kind == "anchor_symbol"));
    if let Some(item) = symbol_file {
        let path = item.path.as_deref().unwrap_or(item.target.as_str());
        let matched: Option<Vec<String>> = item
            .evidence
            .get("matched_queries")
            .or_else(|| {
                item.evidence
                    .get("also_symbol_search")
                    .and_then(|value| value.get("matched_queries"))
            })
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            });
        let term_hint = match matched {
            Some(t) if !t.is_empty() => format!(" (matched {})", t.join(", ")),
            _ => String::new(),
        };
        steps.push(serde_json::json!({
            "step": format!(
                "Search the codebase for callers of the symbol(s) Aethyme \
                 found in {}{}; the call sites confirm whether this file is \
                 the entry point or just one of many implementations.",
                path, term_hint
            ),
            "rationale": "Symbol-name matches show definition; callers show \
                          actual usage and surface dispatch.",
        }));
    }

    // Step 3: degraded/failed → suggest rerun.
    if trust_policy.trust_policy == "failed" || trust_policy.trust_policy == "needs_verification" {
        if answers.is_empty() && nav_hints.is_empty() {
            steps.push(serde_json::json!({
                "step": "Broaden the request: include domain terms (entity \
                         names, file types) or rerun with `--detail standard` \
                         for wider symbol/text coverage.",
                "rationale": "No candidates surfaced — the request may not \
                              tokenize into useful query terms.",
            }));
        } else if trust_policy.trust_policy == "needs_verification" {
            steps.push(serde_json::json!({
                "step": "If the task requires high confidence, rerun with \
                         `--detail standard`; use `--detail full \
                         --show-observability` only when the wider native \
                         evidence still needs diagnosis.",
                "rationale": "The compact native path prioritizes bounded \
                              output; higher native detail levels expose \
                              wider evidence and diagnostics.",
            }));
        }
    }

    // Final fallback if we somehow produced nothing actionable above.
    if steps.is_empty() {
        steps.push(serde_json::json!({
            "step": "Open the top answer[] item and confirm it matches the \
                     task before relying on it.",
            "rationale": "Graph navigation found this candidate; verifying \
                          that the file genuinely handles the task is fast.",
        }));
    }

    // Cap at 4: longer lists are noise; the first 1-2 are typically the
    // strongest moves an agent can take.
    steps.truncate(4);
    steps
}

/// Group symbol-search hits by file, rank by query coverage + cumulative
/// score, emit AnswerItems with `kind = "symbol_search_file"`. Mirrors
/// `_task_localization_symbol_file_items` in the Python orchestrator so
/// downstream consumers see the same shape.
///
/// Confidence scoring:
///   - 2+ distinct queries matched in this file → 0.88 (multi-term match)
///   - 1 query matched                          → 0.76
///
/// These are the same numbers Python uses; preserving them keeps the
/// trust-policy heuristics consistent across implementations.
pub(super) fn build_symbol_file_items(
    symbol_matches: &SymbolBatchResults,
    cap: usize,
) -> Vec<AnswerItem> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct PerFile {
        queries: BTreeSet<String>,
        symbols: Vec<serde_json::Value>,
        score: i64,
    }

    let mut by_file: BTreeMap<String, PerFile> = BTreeMap::new();
    // Iterate queries in original order so the most relevant query
    // dominates `symbols[0]` for a given file.
    for query in &symbol_matches.query_order {
        let Some(hits) = symbol_matches.by_query.get(query) else {
            continue;
        };
        for hit in hits {
            if hit.file.trim().is_empty() {
                continue;
            }
            let entry = by_file.entry(hit.file.clone()).or_default();
            entry.queries.insert(query.clone());
            entry.symbols.push(serde_json::json!({
                "name": hit.name,
                "kind": hit.kind,
                "line": hit.line,
                "score": hit.score,
            }));
            entry.score += hit.score;
        }
    }

    let mut ranked: Vec<(String, PerFile)> = by_file.into_iter().collect();
    ranked.sort_by(|(la, a), (lb, b)| {
        // Primary: more distinct queries. Secondary: total score. Final:
        // stable path order.
        b.queries
            .len()
            .cmp(&a.queries.len())
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| la.cmp(lb))
    });

    let mut items: Vec<AnswerItem> = Vec::new();
    for (file_path, summary) in ranked.into_iter().take(cap) {
        let matched_queries: Vec<String> = summary.queries.iter().cloned().collect();
        let multi = matched_queries.len() > 1;
        let confidence: f64 = if multi { 0.88 } else { 0.76 };
        let reason = if multi {
            "Multiple request terms matched symbols in this file."
        } else {
            "A request term matched a symbol in this file."
        };
        let symbols_preview: Vec<serde_json::Value> = summary.symbols.into_iter().take(5).collect();
        let evidence = serde_json::json!({
            "source": "query-symbol",
            "matched_queries": matched_queries,
            "symbols": symbols_preview,
            "combined_score": summary.score,
        });
        items.push(AnswerItem {
            kind: "symbol_search_file".into(),
            target: file_path.clone(),
            path: Some(file_path),
            status: "candidate".into(),
            confidence,
            reason: reason.into(),
            role: "candidate".into(),
            evidence,
        });
    }
    items
}

pub(super) fn bucket_confidence(items: &[AnswerItem]) -> ConfidenceSummary {
    let mut summary = ConfidenceSummary::default();
    for item in items {
        if item.confidence >= 0.8 {
            summary.high += 1;
        } else if item.confidence >= 0.6 {
            summary.medium += 1;
        } else {
            summary.low += 1;
        }
    }
    summary
}
