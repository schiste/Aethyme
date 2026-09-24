//! Tests for the progressive-disclosure ladder (`DISCLOSURE_LEVELS`
//! + `ExploreParams::apply_disclosure_level`).
//!
//! The table encodes a budget contract: depth=0 is cheap discovery,
//! depth=3 is expensive deep-dive. These tests pin invariants so a
//! contributor adjusting the table doesn't accidentally violate
//! the "each rung must be meaningfully different" rule.
use super::*;

#[test]
fn disclosure_table_has_four_rungs() {
    // Pin the count — the CLI parser bounds-checks against this
    // length, and SKILL.md teaches a 4-level ladder. A future
    // change adding a 5th rung needs to update this test, the
    // CLI bound, and the skill teaching together.
    assert_eq!(DISCLOSURE_LEVELS.len(), 4);
}

#[test]
fn disclosure_levels_are_meaningfully_different() {
    // Constraint #1 from the table comment: each rung must
    // differ from the one below in at least one observable
    // way. Implementation: walk pairs and assert at least one
    // field changes.
    for i in 0..(DISCLOSURE_LEVELS.len() - 1) {
        let a = DISCLOSURE_LEVELS[i];
        let b = DISCLOSURE_LEVELS[i + 1];
        let differs = a.max_items != b.max_items
            || a.include_signatures != b.include_signatures
            || a.include_snippets != b.include_snippets
            || a.snippet_lines != b.snippet_lines
            || a.include_call_graph != b.include_call_graph
            || a.max_response_tokens != b.max_response_tokens;
        assert!(
            differs,
            "DISCLOSURE_LEVELS[{i}] and [{}] are observably \
                 identical — agents will skip the cheaper rung",
            i + 1,
        );
    }
}

#[test]
fn token_budgets_are_monotonically_increasing() {
    // Constraint: deeper rungs cost more. If depth=2 cost less
    // than depth=1, the ladder shape is broken — agents have no
    // reason to stop at lower rungs.
    let budgets: Vec<usize> = DISCLOSURE_LEVELS
        .iter()
        .map(|l| l.max_response_tokens)
        .collect();
    for i in 0..(budgets.len() - 1) {
        assert!(
            budgets[i] < budgets[i + 1],
            "budgets {} -> {} not strictly increasing: {budgets:?}",
            i,
            i + 1,
        );
    }
}

#[test]
fn call_graph_only_at_max_depth() {
    // Constraint #4: call-graph closure is O(graph) per call;
    // gating it behind the deepest rung prevents accidental
    // fan-out. If a future change opens this up at lower
    // depths, that should be deliberate — and tested.
    for (i, level) in DISCLOSURE_LEVELS.iter().enumerate() {
        let expect = i == DISCLOSURE_LEVELS.len() - 1;
        assert_eq!(
            level.include_call_graph, expect,
            "DISCLOSURE_LEVELS[{i}].include_call_graph must be \
                 {expect} (only the deepest rung enables call-graph)",
        );
    }
}

#[test]
fn depth_zero_strips_evidence() {
    // depth=0 is the discovery rung — cheap. Apply must zero
    // out line_refs and downstream knobs that crowd the budget.
    let mut p = ExploreParams {
        depth: Some(0),
        ..ExploreParams::default()
    };
    p.apply_disclosure_level();
    assert_eq!(p.max_answer_items, 15);
    assert_eq!(p.max_text_line_refs, 0);
    assert_eq!(p.max_filename_hints, 0);
    assert_eq!(p.max_callsite_symbols, 0);
    assert_eq!(p.max_callsite_results, 0);
}

#[test]
fn depth_one_keeps_signature_line_only() {
    let mut p = ExploreParams {
        depth: Some(1),
        ..ExploreParams::default()
    };
    p.apply_disclosure_level();
    assert_eq!(p.max_answer_items, 8);
    // Exactly 1 line_ref to surface the signature line — the
    // "+ signatures" promise without the snippet cost.
    assert_eq!(p.max_text_line_refs, 1);
    // Callsite expansion still gated below depth=2.
    assert_eq!(p.max_callsite_symbols, 0);
}

#[test]
fn depth_two_enables_snippets_and_callsites() {
    let mut p = ExploreParams {
        depth: Some(2),
        ..ExploreParams::default()
    };
    p.apply_disclosure_level();
    assert_eq!(p.max_answer_items, 3);
    assert!(p.max_text_line_refs > 0);
    assert!(p.max_callsite_symbols > 0);
}

#[test]
fn depth_three_pulls_full_content() {
    let mut p = ExploreParams {
        depth: Some(3),
        ..ExploreParams::default()
    };
    p.apply_disclosure_level();
    assert_eq!(p.max_answer_items, 1);
    // depth=3 has the call-graph flag — verify the table value.
    assert!(DISCLOSURE_LEVELS[3].include_call_graph);
    assert_eq!(DISCLOSURE_LEVELS[3].snippet_lines, usize::MAX);
}

#[test]
fn out_of_range_depth_clamps_to_max() {
    // The CLI binary validates 0..=3 explicitly, but the
    // method itself is robust against bad inputs from future
    // embedded callers (PyO3, MCP). Clamps silently to the
    // top rung rather than panicking — defensive only.
    let mut p = ExploreParams {
        depth: Some(99),
        ..ExploreParams::default()
    };
    p.apply_disclosure_level();
    // Should land on the deepest rung's caps.
    assert_eq!(p.max_answer_items, 1);
}

#[test]
fn no_depth_leaves_params_untouched() {
    // depth=None means use legacy detail-based defaults.
    // apply_disclosure_level must be a no-op in this case.
    let mut p = ExploreParams::default();
    let before_max_items = p.max_answer_items;
    let before_callsite = p.max_callsite_symbols;
    p.apply_disclosure_level();
    assert_eq!(p.max_answer_items, before_max_items);
    assert_eq!(p.max_callsite_symbols, before_callsite);
}
