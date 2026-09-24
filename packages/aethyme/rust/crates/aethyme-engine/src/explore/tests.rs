use super::*;

fn sample_view() -> serde_json::Value {
    serde_json::json!({
        "task": "find watchlist handlers",
        "anchors": {
            "task": "find watchlist handlers",
            "anchors": [
                {"kind": "file", "id": "includes/Watchlist/WatchedItemStore.php",
                 "file": "includes/Watchlist/WatchedItemStore.php",
                 "reason": "filename match"},
                {"kind": "folder", "id": "includes/Watchlist",
                 "file": null,
                 "reason": "area match"}
            ]
        },
        "scope": {
            "task": "find watchlist handlers",
            "navigation_order": ["includes/Watchlist", "includes/Specials"],
            "in_scope_files": [
                "includes/Specials/SpecialEditWatchlist.php",
                "includes/Watchlist/WatchlistManager.php"
            ],
            "in_scope_symbols": [],
            "in_scope_areas": ["includes/Watchlist"],
            "out_of_scope": [],
            "risks": []
        },
        "next": {
            "target": "find watchlist handlers",
            "relation": "next",
            "items": []
        }
    })
}

fn empty_symbols() -> SymbolBatchResults {
    SymbolBatchResults::default()
}

fn symbols_for(file: &str, queries: &[(&str, i64)]) -> SymbolBatchResults {
    let mut by_query = std::collections::BTreeMap::new();
    let mut order = Vec::new();
    for (q, score) in queries {
        order.push((*q).to_string());
        by_query.insert(
            (*q).to_string(),
            vec![SymbolHit {
                name: format!("hit_for_{q}"),
                kind: "function".into(),
                file: file.to_string(),
                line: 42,
                score: *score,
            }],
        );
    }
    SymbolBatchResults {
        query_order: order,
        by_query,
    }
}

#[test]
fn build_response_synthesizes_answers_and_nav_hints() {
    let response = build_response(
        "find watchlist handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &empty_symbols(),
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );
    assert!(!response.answer.is_empty(), "expected at least one answer");
    assert!(
        response
            .answer
            .iter()
            .any(|a| a.path.as_deref() == Some("includes/Watchlist/WatchedItemStore.php")),
        "anchor file should land in answer[]"
    );
    assert!(
        response
            .answer
            .iter()
            .any(|a| a.path.as_deref() == Some("includes/Specials/SpecialEditWatchlist.php")),
        "in-scope file should land in answer[]"
    );
    assert!(
        response
            .navigation_hints
            .iter()
            .any(|h| h.target == "includes/Watchlist"),
        "folder anchor should land in navigation_hints[]"
    );
}

#[test]
fn build_response_promotes_symbol_anchors_into_answer() {
    // Regression test for the 2026-05-12 symbol-anchor promotion.
    //
    // Pre-fix: anchors with `kind: "symbol"` (produced by the
    // Unknown arm of `resolve_anchors` after 7a01c32) landed in
    // `navigation_hints[]` via the generic `other` arm. Agents
    // reading only `answer[]` never saw them, even though
    // symbol-name match is at least as specific as filename match.
    //
    // Post-fix: symbol anchors push into `answer[]` as
    // `kind: "anchor"` items with `evidence.anchor_kind: "symbol"`
    // and `confidence: 0.80`. Path-based dedup against text
    // matches still applies — see the merge step at the
    // "anchor_items" loop.
    let view = serde_json::json!({
        "task": "find watchlist handlers",
        "anchors": {
            "task": "find watchlist handlers",
            "anchors": [
                {
                    "kind": "symbol",
                    // Qualified id mirrors the real shape:
                    // `fn:<repo>:<file>:<symbol>`.
                    "id": "fn:Mediawiki - Aethyme:includes/Page/WikiPage.php:doViewUpdates",
                    "file": "includes/Page/WikiPage.php",
                    "reason": "function-name-match via viewupdates"
                }
            ]
        },
        "scope": {
            "in_scope_files": [],
            "in_scope_symbols": [],
            "in_scope_areas": [],
            "out_of_scope": [],
            "risks": []
        },
        "next": {"items": []}
    });
    let response = build_response(
        "find watchlist handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &view,
        &empty_symbols(),
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );

    let promoted = response
        .answer
        .iter()
        .find(|a| a.path.as_deref() == Some("includes/Page/WikiPage.php"))
        .expect(
            "symbol anchor for `doViewUpdates` should be promoted into \
                 answer[] (was previously routed to navigation_hints)",
        );

    // Pinned downstream contract: kind="anchor" with sub-kind in
    // evidence. This keeps the answer-kind list in the
    // `debug_assert!` at line ~1454 unchanged.
    assert_eq!(promoted.kind, "anchor");
    assert_eq!(
        promoted
            .evidence
            .get("anchor_kind")
            .and_then(|v| v.as_str()),
        Some("symbol")
    );

    // Confidence: 0.80 (chosen 2026-05-12). Slightly below file
    // anchors (0.85) to acknowledge symbol_search's current
    // token-substring naivete. If a future ranking improvement
    // makes symbol matches as reliable as filename matches, raise
    // this to 0.85 — single-line change.
    let confidence_diff = (promoted.confidence - 0.80_f64).abs();
    assert!(
        confidence_diff < 1e-9,
        "expected confidence 0.80; got {}",
        promoted.confidence
    );

    // The qualified symbol id flows into `target` so agents can
    // navigate to the specific symbol, not just the file.
    assert!(
        promoted.target.contains("doViewUpdates"),
        "target should preserve the qualified symbol id; got {}",
        promoted.target
    );

    // Sanity: the same anchor should NOT also appear in
    // navigation_hints (we promoted it, didn't duplicate it).
    assert!(
        response
            .navigation_hints
            .iter()
            .all(|h| h.path.as_deref() != Some("includes/Page/WikiPage.php")
                || h.kind != "anchor_symbol"),
        "symbol anchor should be in answer[], not duplicated to \
             navigation_hints[]"
    );
}

#[test]
fn build_response_symbol_anchor_dedup_against_text_match() {
    // When a file is ALREADY in answer[] via text-match, a symbol
    // anchor for the same file should be dropped (path-based
    // dedup at the merge step). The text-match item carries
    // line-level evidence which is stronger than "this file
    // contains a matching symbol name."
    let view = serde_json::json!({
        "task": "find watchlist handlers",
        "anchors": {
            "task": "find watchlist handlers",
            "anchors": [
                {
                    "kind": "symbol",
                    "id": "fn:repo:WatchedItemStore.php:resetNotificationTimestamp",
                    "file": "includes/Watchlist/WatchedItemStore.php",
                    "reason": "function-name-match via notification"
                }
            ]
        },
        "scope": {
            "in_scope_files": [],
            "in_scope_symbols": [],
            "in_scope_areas": [],
            "out_of_scope": [],
            "risks": []
        },
        "next": {"items": []}
    });
    let text_match = AnswerItem {
        kind: "source_text_file".into(),
        target: "includes/Watchlist/WatchedItemStore.php".into(),
        path: Some("includes/Watchlist/WatchedItemStore.php".into()),
        status: "candidate".into(),
        confidence: 0.75,
        reason: "text-match line evidence".into(),
        role: "candidate".into(),
        evidence: serde_json::json!({"source": "text-search"}),
    };
    let response = build_response(
        "find watchlist handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &view,
        &empty_symbols(),
        &[text_match], // text-match for same file as symbol anchor
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );

    // Exactly ONE answer for this file — the text-match one.
    let matches: Vec<_> = response
        .answer
        .iter()
        .filter(|a| a.path.as_deref() == Some("includes/Watchlist/WatchedItemStore.php"))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly 1 answer for the file; got {}: {:?}",
        matches.len(),
        matches.iter().map(|m| &m.kind).collect::<Vec<_>>(),
    );
    // The survivor is the text-match (added first, stronger
    // evidence), not the symbol anchor.
    assert_eq!(matches[0].kind, "source_text_file");
    let verification = serde_json::to_string(&response.verification_steps).unwrap();
    assert!(verification.contains("--detail standard"));
    assert!(verification.contains("--detail full"));
    assert!(!verification.contains("Python"));
}

#[test]
fn build_response_caps_answer_count() {
    let mut view = sample_view();
    let scope = view.get_mut("scope").unwrap();
    scope["in_scope_files"] = serde_json::json!(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
    let response = build_response(
        "test",
        Intent::TaskLocalization,
        IntentSource::Default,
        &view,
        &empty_symbols(),
        &[],
        &[],
        &[],
        &ExploreParams {
            max_answer_items: 3,
            ..ExploreParams::default()
        },
        test_observability(),
    );
    assert_eq!(response.answer.len(), 3);
}

#[test]
fn build_response_degraded_when_empty() {
    let view = serde_json::json!({
        "anchors": {"anchors": []},
        "scope": {
            "in_scope_files": [],
            "in_scope_symbols": [],
            "in_scope_areas": [],
            "out_of_scope": [],
            "risks": []
        },
        "next": {"items": []}
    });
    let response = build_response(
        "nothing matches",
        Intent::TaskLocalization,
        IntentSource::Default,
        &view,
        &empty_symbols(),
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );
    assert_eq!(response.status, "degraded");
    assert!(response.answer.is_empty());
    assert!(response.navigation_hints.is_empty());
    assert_eq!(response.trust_policy.trust_policy, "failed");
}

#[test]
fn trust_policy_without_symbol_evidence_is_needs_verification() {
    let response = build_response(
        "find handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &empty_symbols(),
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );
    // Without symbol evidence, anchors+scope alone don't earn
    // `answer_candidate` — the cross-corroboration is missing.
    assert!(!response.safe_to_use_as_answer);
    assert_eq!(response.trust_policy.trust_policy, "needs_verification");
    assert_eq!(response.trust_policy.evidence_level, "graph");
}

#[test]
fn multi_query_symbol_match_elevates_to_answer_candidate() {
    let symbols = symbols_for(
        "src/auth/SessionStore.php",
        &[("session", 200), ("authenticate", 300)],
    );
    let response = build_response(
        "find session authenticate handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &symbols,
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );
    assert!(response.safe_to_use_as_answer);
    assert_eq!(response.trust_policy.trust_policy, "answer_candidate");
    assert_eq!(response.trust_policy.evidence_level, "graph+symbol");
    // The matched file should appear in answer[] as a symbol_search_file
    // ahead of in_scope_file items because symbol evidence is stronger.
    let symbol_match_position = response
        .answer
        .iter()
        .position(|a| a.kind == "symbol_search_file");
    assert!(
        symbol_match_position.is_some(),
        "symbol_search_file should be present in answer[]"
    );
}

#[test]
fn build_response_verification_prefers_symbol_evidence_merged_into_top_text_answer() {
    let text_match = AnswerItem {
        kind: "source_text_file".into(),
        target: "billing/vendor_sync.py".into(),
        path: Some("billing/vendor_sync.py".into()),
        status: "candidate".into(),
        confidence: 0.87,
        reason: "text evidence".into(),
        role: "candidate".into(),
        evidence: serde_json::json!({
            "source": "source-text-search",
            "matched_terms": ["vendor", "sync", "ledger"],
            "line_refs": [{"line": 114, "text": "vendor sync ledger", "matched_terms": ["vendor", "sync", "ledger"]}],
        }),
    };
    let mut by_query = std::collections::BTreeMap::new();
    by_query.insert(
        "Vendor".to_string(),
        vec![
            SymbolHit {
                name: "get_vendor_ledger".into(),
                kind: "function".into(),
                file: "billing/vendor_sync.py".into(),
                line: 83,
                score: 160,
            },
            SymbolHit {
                name: "record_vendor_assets".into(),
                kind: "function".into(),
                file: "billing/vendor_assets.py".into(),
                line: 56,
                score: 100,
            },
        ],
    );
    let symbols = SymbolBatchResults {
        query_order: vec!["Vendor".to_string()],
        by_query,
    };

    let response = build_response(
        "Trace vendor sync ledger behavior.",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &symbols,
        &[text_match],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );

    let top = response
        .answer
        .first()
        .expect("expected text answer to survive");
    assert_eq!(top.path.as_deref(), Some("billing/vendor_sync.py"));
    assert!(
        top.evidence.get("also_symbol_search").is_some(),
        "same-path symbol evidence should merge into the top text answer"
    );
    let symbol_step = response
        .verification_steps
        .iter()
        .filter_map(|value| value.get("step").and_then(|step| step.as_str()))
        .find(|step| step.contains("callers of the symbol"))
        .unwrap_or("");
    assert!(
        symbol_step.contains("billing/vendor_sync.py"),
        "symbol verification should follow the merged top answer, got {symbol_step}"
    );
}

#[test]
fn single_query_symbol_match_stays_at_needs_verification() {
    let symbols = symbols_for("src/util/helpers.php", &[("helper", 100)]);
    let response = build_response(
        "find helper code",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &symbols,
        &[],
        &[],
        &[],
        &ExploreParams::default(),
        test_observability(),
    );
    // One query matched is weak corroboration — bar to claim
    // `answer_candidate` is multi-term match in the SAME file.
    assert!(!response.safe_to_use_as_answer);
    assert_eq!(response.trust_policy.trust_policy, "needs_verification");
    assert_eq!(response.trust_policy.evidence_level, "graph+symbol-weak");
}

#[test]
fn extract_symbol_queries_drops_stop_words_and_short_terms() {
    let queries = extract_symbol_queries("Find the file that handles WatchedItem revisions");
    // "find", "the", "that" are stop words. "Watcheditem" stays.
    assert!(
        queries
            .iter()
            .any(|q| q.eq_ignore_ascii_case("WatchedItem"))
    );
    assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("revisions")));
    assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("the")));
    assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("find")));
}

#[test]
fn extract_symbol_queries_adds_underscore_collapsed_variant() {
    let queries = extract_symbol_queries("trace add_watch behavior");
    // Both `add_watch` and `addwatch` should be present.
    let lower: Vec<String> = queries.iter().map(|q| q.to_ascii_lowercase()).collect();
    assert!(lower.contains(&"add_watch".to_string()));
    assert!(lower.contains(&"addwatch".to_string()));
}

#[test]
fn extract_text_search_terms_extends_for_behavioural_words() {
    // For text search we keep behavioural keywords like "viewed" and
    // "seen" that the symbol-query helper drops. The trigger is
    // matching them in the request itself; if the request mentions
    // "watchlist" we add domain synonyms ("watched", "notification").
    let terms = extract_text_search_terms("Bug: viewing a diff revision marks watchlist as seen");
    let lower: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    // The request word "viewing" survives (symbol-search would drop it
    // as too noisy, text-search keeps it).
    assert!(lower.contains(&"viewing".to_string()));
    // Domain expansions added by the watchlist trigger:
    assert!(lower.contains(&"watched".to_string()));
    assert!(lower.contains(&"notification".to_string()));
    // Domain expansions added by the diff/revision trigger:
    assert!(lower.contains(&"diff".to_string()));
    assert!(lower.contains(&"revisions".to_string()));
    // No duplicates (case-insensitive):
    let mut sorted = lower.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), lower.len(), "duplicate term in {lower:?}");
}

// ── Intent::auto_select heuristic ──────────────────────────────────

#[test]
fn auto_select_picks_behavior_for_change_verbs() {
    // Standard change-task openings.
    assert_eq!(
        Intent::auto_select("Add a new authentication provider"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("Implement caching for the snapshot builder"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("Fix the bug where viewing a diff marks all revisions as seen"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("Refactor the suppliers grader scoring logic"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("Remove the deprecated v1 API surface"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("Migrate from SurrealDB to redb"),
        Intent::BehaviorLocalization,
    );
}

#[test]
fn auto_select_picks_task_for_read_only_verbs() {
    // "where/find/show/explain" — the user wants to LOCATE, not change.
    assert_eq!(
        Intent::auto_select("Where does suppliers grader live?"),
        Intent::TaskLocalization,
    );
    assert_eq!(
        Intent::auto_select("Find files that handle watchlist notifications"),
        Intent::TaskLocalization,
    );
    assert_eq!(
        Intent::auto_select("Show me the auth flow"),
        Intent::TaskLocalization,
    );
    assert_eq!(
        Intent::auto_select("Explain how the snapshot builder works"),
        Intent::TaskLocalization,
    );
    // No verb at all — default to task.
    assert_eq!(
        Intent::auto_select("authentication provider"),
        Intent::TaskLocalization,
    );
}

#[test]
fn auto_select_only_scans_first_10_tokens() {
    // Long preamble whose CHANGE verb sits past the 10-token window
    // should NOT trigger BehaviorLocalization. Verbs front-load in
    // requests; mid-sentence verbs are usually descriptive.
    let request = "Where does the file that the recent CI failure last \
                       Tuesday refers to add a new feature live?";
    // "add" appears at token ~13. Should still be TaskLocalization.
    assert_eq!(Intent::auto_select(request), Intent::TaskLocalization,);
}

#[test]
fn auto_select_ignores_verb_inside_word() {
    // "padding" contains "add" as substring — must NOT trigger.
    // Tokenization is the safety net: we match whole tokens, not
    // substrings.
    assert_eq!(
        Intent::auto_select("Where is the padding logic for the form?"),
        Intent::TaskLocalization,
    );
}

#[test]
fn auto_select_handles_punctuation_around_verbs() {
    // Common patterns where the verb has punctuation neighbors.
    assert_eq!(
        Intent::auto_select("Bug: \"add\" feature broken"),
        Intent::BehaviorLocalization,
    );
    assert_eq!(
        Intent::auto_select("TODO -- implement the missing handler"),
        Intent::BehaviorLocalization,
    );
}

// ── response-shape snapshot tests ───────────────────────────────
//
// Goal: catch silent drift in the JSON response schema. Today's
// hard-delete of Python explore removed the side-by-side comparison
// that would have caught divergences naturally; these tests are
// the standing replacement.
//
// What we assert:
//   (a) Required top-level keys are always present (any agent can
//       rely on `answer[]`, `trust_policy`, etc. existing).
//   (b) Optional keys (`output_adapters`, `resolved_parameters`)
//       gate correctly on `Detail::Full`.
//   (c) Nested types — e.g. `evidence.answer_count` is a number,
//       `trust_policy.trust_policy` is one of the documented
//       enum values.
//
// What we DON'T assert:
//   - Exact bytes (too brittle — float precision, key order,
//     etc. would break tests on benign refactors).
//   - Exact answer-list contents (those are integration-test
//     concerns; this is a schema test).

fn empty_view() -> serde_json::Value {
    serde_json::json!({
        "task": "stub",
        "anchors": {"task": "stub", "anchors": []},
        "scope": {
            "task": "stub",
            "navigation_order": [],
            "in_scope_files": [],
            "in_scope_symbols": [],
            "in_scope_areas": [],
            "out_of_scope": [],
            "risks": []
        },
        "next": {"target": "stub", "relation": "next", "items": []}
    })
}

fn empty_symbol_matches() -> SymbolBatchResults {
    SymbolBatchResults::default()
}

fn test_observability() -> serde_json::Value {
    serde_json::json!({
        "graph_store": {
            "backend": "redb",
            "status": "fresh",
            "exists": true,
            "stale": false,
        }
    })
}

fn build_minimal_response(detail: Detail, show_observability: bool) -> ExploreResponse {
    let view = empty_view();
    let symbols = empty_symbol_matches();
    let params = ExploreParams {
        detail,
        show_observability,
        ..ExploreParams::default()
    };
    build_response(
        "stub request",
        Intent::TaskLocalization,
        IntentSource::Default,
        &view,
        &symbols,
        &[],
        &[],
        &[],
        &params,
        test_observability(),
    )
}

/// Required top-level keys that EVERY response must carry. Adding
/// or removing a key here is a schema-breaking change for downstream
/// consumers; do it deliberately, not as a side-effect.
const REQUIRED_TOP_LEVEL_KEYS: &[&str] = &[
    "schema_version",
    "mode",
    "intent",
    "intent_source",
    "status",
    "request",
    "answer",
    "navigation_hints",
    "excluded",
    "ambiguous",
    "subsystems",
    "evidence",
    "confidence",
    "safe_to_use_as_answer",
    "safe_to_use_as_navigation",
    "trust_policy",
    "degraded_reasons",
    "verification_steps",
    "next_actions",
    "available_specialized_intents",
    "output_chars_estimate",
    "truncated",
];

#[test]
fn response_compact_has_all_required_keys() {
    let response = build_minimal_response(Detail::Compact, false);
    let json = serde_json::to_value(&response).unwrap();
    let obj = json.as_object().expect("response is a JSON object");
    for key in REQUIRED_TOP_LEVEL_KEYS {
        assert!(
            obj.contains_key(*key),
            "compact response missing required key: {key}"
        );
    }
}

#[test]
fn response_compact_omits_verbose_fields() {
    // At compact + no show_observability, the Python predecessor
    // trimmed `output_adapters` and `resolved_parameters` (cli.py
    // `_trim_explore_response`). Native preserves that contract.
    let response = build_minimal_response(Detail::Compact, false);
    let json = serde_json::to_value(&response).unwrap();
    let obj = json.as_object().unwrap();
    assert!(
        !obj.contains_key("output_adapters"),
        "compact must omit output_adapters; got {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    assert!(
        !obj.contains_key("resolved_parameters"),
        "compact must omit resolved_parameters"
    );
    assert!(
        !obj.contains_key("observability"),
        "compact must omit observability"
    );
    assert!(
        response.output_chars_estimate > 0,
        "compact response should report an output char estimate"
    );
}

#[test]
fn response_full_emits_verbose_fields() {
    // At Detail::Full, the verbose envelope (output_adapters +
    // resolved_parameters) lights up.
    let response = build_minimal_response(Detail::Full, false);
    let json = serde_json::to_value(&response).unwrap();
    let obj = json.as_object().unwrap();
    assert!(
        obj.contains_key("output_adapters"),
        "Detail::Full must emit output_adapters"
    );
    assert!(
        obj.contains_key("resolved_parameters"),
        "Detail::Full must emit resolved_parameters"
    );
    assert!(
        obj.contains_key("observability"),
        "Detail::Full must emit observability"
    );
}

#[test]
fn response_show_observability_uses_agent_compact_profile() {
    // `--show-observability` at compact emits the agent-facing trust
    // summary, not the full debug envelope.
    let response = build_minimal_response(Detail::Compact, true);
    let json = serde_json::to_value(&response).unwrap();
    let obj = json.as_object().unwrap();
    assert!(
        !obj.contains_key("output_adapters"),
        "agent compact observability must not emit output_adapters"
    );
    assert!(
        !obj.contains_key("resolved_parameters"),
        "agent compact observability must not emit resolved_parameters"
    );
    assert!(
        obj.contains_key("observability"),
        "show_observability=true must emit compact observability"
    );
    assert_eq!(
        obj["observability"]["output_profile"], "agent_compact",
        "compact observability should identify its profile"
    );
}

#[test]
fn observability_reports_surface_flow_coverage_gaps() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_file(
        tmp.path(),
        "backend/keys/middleware.py",
        "class KeyCheckMiddleware: pass\n",
    );
    write_test_file(
        tmp.path(),
        "edge-proxy/src/worker.mjs",
        "export default { fetch(request) { return fetch(request) } }\n",
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/_index/backend.keys.middleware.ndjson",
        r#"{"module":"backend.keys.middleware","symbol":"KeyCheckMiddleware","kind":"class","node_id":"class:demo:abc","file":"backend/keys/middleware.py"}"#,
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/backend/keys/middleware.py.bin",
        "middleware_installation validates_credential\n",
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/backend/keys/urls.py.bin",
        "route_surface exposes\n",
    );
    write_test_file(tmp.path(), ".aethyme/graph_store.redb", "placeholder");

    let observability = graph_store_observability(tmp.path());
    assert_eq!(
        observability
            .get("graph_freshness")
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str()),
        Some("fresh")
    );
    assert_eq!(
        observability
            .get("graph_freshness")
            .and_then(|value| value.get("fresh"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    let surface_flow = observability
        .get("surface_flow_graph")
        .and_then(|value| value.as_object())
        .expect("surface_flow_graph observability object");
    assert_eq!(
        surface_flow.get("status").and_then(|value| value.as_str()),
        Some("partial")
    );

    let coverage = surface_flow
        .get("coverage")
        .and_then(|value| value.as_object())
        .expect("coverage object");
    let backend = coverage
        .get("backend")
        .and_then(|value| value.as_object())
        .expect("backend coverage");
    assert_eq!(
        backend.get("source_present").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(backend.get("indexed").and_then(|v| v.as_bool()), Some(true));

    let edge_proxy = coverage
        .get("edge_proxy")
        .and_then(|value| value.as_object())
        .expect("edge proxy coverage");
    assert_eq!(
        edge_proxy.get("source_present").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        edge_proxy.get("indexed").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        edge_proxy.get("status").and_then(|v| v.as_str()),
        Some("source_present_not_indexed")
    );

    let missing = surface_flow
        .get("missing_expected_surfaces")
        .and_then(|value| value.as_array())
        .expect("missing surface list");
    assert!(missing.iter().any(|item| {
        item.get("surface_type").and_then(|value| value.as_str()) == Some("edge_proxy")
    }));
    let top_level_missing = observability
        .get("missing_expected_surfaces")
        .and_then(|value| value.as_array())
        .expect("top-level missing surface list");
    assert_eq!(top_level_missing.len(), missing.len());
    assert_eq!(
        observability
            .get("graph_completeness_by_surface_type")
            .and_then(|value| value.get("edge_proxy"))
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str()),
        Some("source_present_not_indexed")
    );
    let languages = observability
        .get("indexed_languages")
        .and_then(|value| value.as_array())
        .expect("indexed languages");
    assert!(
        languages
            .iter()
            .any(|value| value.as_str() == Some("python")),
        "expected python indexed language in {languages:?}"
    );
    let frameworks = observability
        .get("indexed_frameworks")
        .and_then(|value| value.as_array())
        .expect("indexed frameworks");
    assert!(
        frameworks
            .iter()
            .any(|value| value.as_str() == Some("django")),
        "expected django indexed framework in {frameworks:?}"
    );
}

#[test]
fn observability_reports_partially_indexed_surface_families() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_file(
        tmp.path(),
        "functions/_middleware.ts",
        "export function onRequest() {}\n",
    );
    write_test_file(
        tmp.path(),
        "edge-proxy/src/worker.mjs",
        "export default { fetch(request) { return fetch(request) } }\n",
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/_index/functions._middleware.ndjson",
        r#"{"module":"functions._middleware","symbol":"onRequest","kind":"function","node_id":"function:demo:def","file":"functions/_middleware.ts"}"#,
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/functions/_middleware.ts.bin",
        r#"{"kind":"worker_surface","path":"functions/_middleware.ts","trigger":"request"}"#,
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/edge-proxy/FOLDER.edge-proxy.md.bin",
        "folder summary",
    );
    write_test_file(
        tmp.path(),
        ".aethyme/graph/edge-proxy/package.json.bin",
        "{\"scripts\":{\"deploy\":\"wrangler deploy\"}}\n",
    );

    let observability = graph_store_observability(tmp.path());
    let edge_proxy = observability
        .get("surface_flow_graph")
        .and_then(|value| value.get("coverage"))
        .and_then(|value| value.get("edge_proxy"))
        .and_then(|value| value.as_object())
        .expect("edge proxy coverage");

    assert_eq!(
        edge_proxy.get("status").and_then(|v| v.as_str()),
        Some("partially_indexed")
    );
    assert_eq!(
        edge_proxy.get("path_indexed").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        edge_proxy.get("semantic_indexed").and_then(|v| v.as_bool()),
        Some(true)
    );
    let unindexed = edge_proxy
        .get("unindexed_source_path_hints")
        .and_then(|value| value.as_array())
        .expect("unindexed source hints");
    assert!(unindexed.iter().any(|value| {
        value
            .as_str()
            .is_some_and(|path| path.starts_with("edge-proxy/"))
    }));
    let languages = observability
        .get("indexed_languages")
        .and_then(|value| value.as_array())
        .expect("indexed languages");
    assert!(
        languages
            .iter()
            .any(|value| value.as_str() == Some("typescript")),
        "expected typescript indexed language in {languages:?}"
    );
    let frameworks = observability
        .get("indexed_frameworks")
        .and_then(|value| value.as_array())
        .expect("indexed frameworks");
    assert!(
        frameworks
            .iter()
            .any(|value| value.as_str() == Some("edge-middleware")),
        "expected edge-middleware indexed framework in {frameworks:?}"
    );
}

#[test]
fn full_response_observability_reports_safety_and_ranking_signals() {
    let text_match = AnswerItem {
        kind: "source_text_file".into(),
        target: "includes/Watchlist/WatchedItemStore.php".into(),
        path: Some("includes/Watchlist/WatchedItemStore.php".into()),
        status: "candidate".into(),
        confidence: 0.87,
        reason: "source text evidence".into(),
        role: "candidate".into(),
        evidence: serde_json::json!({
            "source": "source-text-search",
            "matched_terms": ["watchlist", "revision"],
            "line_refs": [{"line": 12, "text": "watchlist revision", "matched_terms": ["watchlist", "revision"]}],
        }),
    };
    let symbols = symbols_for(
        "includes/Watchlist/WatchedItemStore.php",
        &[("watchlist", 200), ("revision", 300)],
    );
    let params = ExploreParams {
        detail: Detail::Full,
        ..ExploreParams::default()
    };

    let response = build_response(
        "find watchlist revision handlers",
        Intent::TaskLocalization,
        IntentSource::Default,
        &sample_view(),
        &symbols,
        &[text_match],
        &[],
        &[],
        &params,
        test_observability(),
    );

    let observability = response
        .observability
        .as_ref()
        .expect("full detail emits observability");
    assert_eq!(
        observability
            .get("answer_safety")
            .and_then(|value| value.get("mode"))
            .and_then(|value| value.as_str()),
        Some("answer_safe")
    );
    assert_eq!(
        observability
            .get("readiness")
            .and_then(|value| value.get("fresh_enough"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        observability
            .get("readiness")
            .and_then(|value| value.get("complete_enough"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    let signals = observability
        .get("ranking_explainability")
        .and_then(|value| value.get("top_signals_used"))
        .and_then(|value| value.as_array())
        .expect("used signals");
    assert!(
        signals.iter().any(|value| {
            value.get("signal").and_then(|signal| signal.as_str())
                == Some("multi_query_symbol_match")
        }),
        "expected multi-query symbol signal in {signals:?}"
    );
    assert!(
        signals.iter().any(|value| {
            value.get("signal").and_then(|signal| signal.as_str()) == Some("source_line_refs")
        }),
        "expected source line-ref signal in {signals:?}"
    );
    let absent = observability
        .get("ranking_explainability")
        .and_then(|value| value.get("top_signals_absent"))
        .and_then(|value| value.as_array())
        .expect("absent signals");
    assert!(
        absent.iter().any(|value| {
            value.get("signal").and_then(|signal| signal.as_str()) == Some("callsite_adjacency")
        }),
        "expected missing callsite signal in {absent:?}"
    );
}

#[test]
fn response_top_level_types_are_stable() {
    // Pin the type of each top-level field. A future refactor that
    // accidentally changed `answer` from array to object (for
    // example) would fail loudly here, before any consumer broke.
    let response = build_minimal_response(Detail::Compact, false);
    let json = serde_json::to_value(&response).unwrap();
    let obj = json.as_object().unwrap();

    let expectations: &[(&str, &str)] = &[
        ("schema_version", "string"),
        ("mode", "string"),
        ("intent", "string"),
        ("intent_source", "string"),
        ("status", "string"),
        ("request", "object"),
        ("answer", "array"),
        ("navigation_hints", "array"),
        ("excluded", "array"),
        ("ambiguous", "array"),
        ("evidence", "object"),
        ("confidence", "object"),
        ("safe_to_use_as_answer", "boolean"),
        ("safe_to_use_as_navigation", "boolean"),
        ("trust_policy", "object"),
        ("degraded_reasons", "array"),
        ("verification_steps", "array"),
        ("next_actions", "array"),
        ("available_specialized_intents", "array"),
    ];

    for (key, expected_type) in expectations {
        let value = obj
            .get(*key)
            .unwrap_or_else(|| panic!("missing required key {key} in response"));
        let actual_type = match value {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        };
        assert_eq!(
            actual_type, *expected_type,
            "field {key:?} expected type {expected_type:?}, got {actual_type:?}"
        );
    }
}

#[test]
fn trust_policy_inner_shape_is_stable() {
    let response = build_minimal_response(Detail::Compact, false);
    let json = serde_json::to_value(&response).unwrap();
    let trust = json
        .get("trust_policy")
        .and_then(|v| v.as_object())
        .unwrap();

    // The trust_policy object's keys are read by every downstream
    // consumer that branches on whether to act on `answer[]`.
    for key in &[
        "safe_to_use_as_answer",
        "safe_to_use_as_navigation",
        "evidence_level",
        "authoritative_answer_count",
        "navigation_hint_count",
        "degraded",
        "trust_policy",
        "reason",
    ] {
        assert!(trust.contains_key(*key), "trust_policy missing key: {key}");
    }

    // The `trust_policy` enum value is one of the documented values
    // (mirrors Python's `_intent_catalog` declaration in cli.py:1571).
    let policy = trust.get("trust_policy").and_then(|v| v.as_str()).unwrap();
    let allowed = [
        "answer_candidate",
        "needs_verification",
        "navigation_only",
        "failed",
    ];
    assert!(
        allowed.contains(&policy),
        "trust_policy enum value {policy:?} not in documented set {allowed:?}"
    );
}

#[test]
fn evidence_inner_shape_is_stable() {
    let response = build_minimal_response(Detail::Compact, false);
    let json = serde_json::to_value(&response).unwrap();
    let evidence = json.get("evidence").and_then(|v| v.as_object()).unwrap();

    for key in &["answer_count", "navigation_hint_count", "excluded_count"] {
        let v = evidence
            .get(*key)
            .and_then(|x| x.as_u64())
            .unwrap_or_else(|| panic!("evidence.{key} should be a non-negative integer"));
        // Sanity: counts are bounded and non-negative.
        assert!(v < 10_000, "evidence.{key} unrealistic: {v}");
    }
}

fn write_test_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}
