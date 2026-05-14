//! Integration tests for the `NodeKind` and `NodeKindCategory` enums.
//!
//! These exercise only the crate's public API. The intent is to lock in
//! several invariants the schema doc treats as load-bearing:
//!
//! 1. Every variant has a stable canonical name (snake_case), and the
//!    `name` ↔ `from_name` mapping is a perfect bijection for both
//!    `NodeKind` and `NodeKindCategory`.
//! 2. `ALL_NODE_KINDS` and `ALL_NODE_KIND_CATEGORIES` are exhaustive —
//!    adding a variant without updating the slice is caught here.
//! 3. JSON round-trip is identity for every variant. The NDJSON index
//!    shards (commit 5.3 / 1.12) rely on this; if it ever breaks, the
//!    on-disk format silently drifts from the enum.
//! 4. Variants are alphabetical within each category (initial set only;
//!    future tail-appended additions intentionally break this).
//! 5. Categories appear contiguously in `ALL_NODE_KINDS`.
//!
//! The bincode discriminant order is checked in `tests/determinism.rs`
//! (commit 1.13). Skipping it here keeps this commit's dep tree minimal.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_graph_schema::{
    ALL_NODE_KIND_CATEGORIES, ALL_NODE_KINDS, NodeKind, NodeKindCategory,
};

// ─── NodeKind: bijection and exhaustiveness ──────────────────────────

#[test]
fn name_and_from_name_round_trip_for_every_variant() {
    for &kind in ALL_NODE_KINDS {
        let name = kind.name();
        let back = NodeKind::from_name(name).unwrap_or_else(|err| {
            panic!(
                "from_name({name:?}) returned Err({err}) for variant \
                 {kind:?}; the mapping in NodeKind::name and \
                 NodeKind::from_name must stay in sync"
            )
        });
        assert_eq!(
            back, kind,
            "round-trip for {kind:?}: name() = {name:?}, but \
             from_name({name:?}) = {back:?}"
        );
    }
}

#[test]
fn from_name_returns_err_for_unknown_strings() {
    // Spot-check obvious wrong inputs. The point isn't exhaustive
    // coverage of bad inputs, it's that the function isn't silently
    // mapping mis-cased or typo'd names to some variant — and that the
    // error preserves the rejected string so callers can produce
    // informative diagnostics without re-passing the input.
    let bad_inputs = [
        "",
        "Function",   // TitleCase
        "FUNCTION",   // ALLCAPS
        "func",       // truncated
        "functions",  // plural
        "doc-section", // kebab
        "non.code.file", // dotted
    ];
    for bad in bad_inputs {
        let err = NodeKind::from_name(bad).expect_err(&format!(
            "from_name({bad:?}) should have returned Err"
        ));
        assert_eq!(
            err.given(),
            bad,
            "error for {bad:?} did not preserve the rejected string; \
             got given() = {:?}",
            err.given(),
        );
        // Display impl should embed the rejected string for log-friendly
        // diagnostics at cross-process boundaries.
        let displayed = format!("{err}");
        assert!(
            displayed.contains(&format!("{bad:?}")),
            "Display for unknown {bad:?} should embed the rejected \
             string; got {displayed:?}"
        );
    }
}

#[test]
fn canonical_names_are_snake_case() {
    // The schema doc commits to snake_case. Any uppercase letter, hyphen,
    // space, or dot in a canonical name is a contract violation and would
    // produce non-portable node IDs.
    //
    // Also reject double underscores and leading/trailing underscores:
    // the canonical-name surface should be exactly one underscore per
    // word boundary, no decorative variations.
    for &kind in ALL_NODE_KINDS {
        let name = kind.name();
        assert!(!name.is_empty(), "canonical name for {kind:?} is empty");
        for c in name.chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_',
                "canonical name for {kind:?} has non-snake_case char {c:?} \
                 in name = {name:?}"
            );
        }
        assert!(
            !name.starts_with('_') && !name.ends_with('_'),
            "canonical name for {kind:?} = {name:?} starts/ends with _"
        );
        assert!(
            !name.contains("__"),
            "canonical name for {kind:?} = {name:?} contains double \
             underscore; the snake_case convention is one underscore per \
             word boundary",
        );
    }
}

#[test]
fn all_node_kinds_is_exhaustive() {
    // The crate exposes ALL_NODE_KINDS as the canonical iteration handle
    // for consumers. If a variant is added to the enum but not appended
    // to ALL_NODE_KINDS, this test traps the omission.
    //
    // We can't reflect the enum's variant count, so we compare against
    // an explicit count: 24 variants across 6 categories
    // (5 containers + 3 callables + 6 type-defining + 5 sub-symbol +
    // 4 non-code + 1 partial-knowledge). The schema doc's prose says
    // "23" in places due to whether `non_code_file` is double-counted
    // (it's listed in §3.1 as a container AND in §3.5 as the parent of
    // non-code symbol kinds); the enum is the authoritative count and
    // places `non_code_file` once, in the container category.
    //
    // Bumping the count here is the one-line change required when a
    // new kind lands.
    const EXPECTED_TOTAL: usize = 24;
    assert_eq!(
        ALL_NODE_KINDS.len(),
        EXPECTED_TOTAL,
        "ALL_NODE_KINDS has {} entries but the schema currently has {} \
         kinds; if you added a NodeKind variant, append it to \
         ALL_NODE_KINDS and bump EXPECTED_TOTAL here",
        ALL_NODE_KINDS.len(),
        EXPECTED_TOTAL,
    );
}

#[test]
fn all_node_kinds_is_unique() {
    // Defends against duplicate entries in ALL_NODE_KINDS (a copy-paste
    // hazard given the slice has 24 entries). A duplicate would silently
    // make iteration miscount in any consumer that relies on the slice
    // length matching the variant count.
    //
    // BTreeSet rather than HashSet to keep test code aligned with the
    // schema doc's blanket "no HashMap / HashSet" determinism rule. This
    // test does not write a fragment, so HashSet would not produce
    // non-deterministic output, but the consistency-of-discipline
    // argument is worth the trivial cost.
    let mut seen = BTreeSet::new();
    for &kind in ALL_NODE_KINDS {
        assert!(
            seen.insert(kind),
            "duplicate entry in ALL_NODE_KINDS: {kind:?} appears twice"
        );
    }
}

// ─── NodeKind: serde round-trip ──────────────────────────────────────

#[test]
fn json_round_trip_is_identity_for_every_variant() {
    // The NDJSON index shards serialize NodeKind via serde_json. If the
    // serde rename_all attribute ever drifts from the canonical names in
    // NodeKind::name, this test catches the divergence — and if it
    // catches it, the on-disk format is the contract, so this test's
    // failure means the canonical names in name()/from_name() are the
    // ones that need fixing, NOT the serde derive.
    for &kind in ALL_NODE_KINDS {
        let json = serde_json::to_string(&kind).unwrap();
        let back: NodeKind = serde_json::from_str(&json).unwrap_or_else(|err| {
            panic!(
                "round-trip failed for {kind:?}: serialized to {json:?}, \
                 deserialization error = {err}"
            )
        });
        assert_eq!(back, kind, "round-trip mismatch for {kind:?}: {json:?}");
    }
}

#[test]
fn json_form_matches_canonical_name() {
    // The serde rename_all = "snake_case" plus the enum variant idents
    // should produce the same canonical name as NodeKind::name. A drift
    // here means humans reading NDJSON would see one name and code calling
    // NodeKind::name would see another — a cross-process contract hazard.
    for &kind in ALL_NODE_KINDS {
        let json = serde_json::to_string(&kind).unwrap();
        // serde_json::to_string on a unit-like enum variant emits "name",
        // including the quotes. Strip them for the comparison.
        let trimmed = json.trim_matches('"');
        assert_eq!(
            trimmed,
            kind.name(),
            "serde rename_all output {json:?} does not match \
             NodeKind::name() = {:?} for {kind:?}",
            kind.name(),
        );
    }
}

// ─── NodeKind: ordering and category contiguity ──────────────────────

#[test]
fn variants_are_alphabetical_within_each_category() {
    // The module docs claim "within a group, order by canonical
    // snake_case name's alphabetical order". This test makes that
    // claim load-bearing for the *initial* set of variants.
    //
    // Future additions break this invariant intentionally (the docs
    // say new variants append to the tail of their group, never
    // insert mid-list). When the next variant lands, this test will
    // start failing; the dev should:
    //
    //   - update the test to acknowledge the tail-appended variant
    //     (e.g., by partitioning the assertion to "initial set
    //     alphabetical, tail can deviate"), and
    //   - confirm they appended to the tail rather than inserted.
    //
    // For now (commit 1.2, initial 24 kinds), the entire slice must
    // be alphabetical-within-category. This is the strongest form of
    // the rule and it's reachable today.
    let mut group_start = 0;
    while group_start < ALL_NODE_KINDS.len() {
        let head_category = ALL_NODE_KINDS[group_start].category();
        let mut group_end = group_start + 1;
        while group_end < ALL_NODE_KINDS.len()
            && ALL_NODE_KINDS[group_end].category() == head_category
        {
            group_end += 1;
        }

        // Assert names within [group_start, group_end) are alphabetical.
        for i in group_start..group_end.saturating_sub(1) {
            let a = ALL_NODE_KINDS[i].name();
            let b = ALL_NODE_KINDS[i + 1].name();
            assert!(
                a < b,
                "ALL_NODE_KINDS not alphabetical within {head_category:?}: \
                 {a:?} appears before {b:?} (positions {i} and {})",
                i + 1,
            );
        }

        group_start = group_end;
    }
}

#[test]
fn categories_appear_contiguously_in_all_node_kinds() {
    // Each category should appear as a single contiguous block.
    // Interleaving would break the "append to tail of group" rule
    // by removing the notion of "tail of group" entirely.
    let mut seen_categories = Vec::new();
    let mut current_category: Option<NodeKindCategory> = None;
    for &kind in ALL_NODE_KINDS {
        let cat = kind.category();
        if current_category != Some(cat) {
            assert!(
                !seen_categories.contains(&cat),
                "category {cat:?} reappears non-contiguously in \
                 ALL_NODE_KINDS; once a category's block ends, the \
                 category may not appear again",
            );
            seen_categories.push(cat);
            current_category = Some(cat);
        }
    }
}

#[test]
fn categories_are_exhaustive_and_well_defined() {
    // Every kind reports a category, and the category counts match the
    // schema doc's grouping (5 containers, 3 callables, 6 type-defining,
    // 5 sub-symbol, 4 non-code, 1 partial-knowledge = 24 total).
    //
    // BTreeMap (rather than HashMap) keeps test code aligned with the
    // crate-wide determinism discipline.
    let mut counts: BTreeMap<NodeKindCategory, usize> = BTreeMap::new();
    for &kind in ALL_NODE_KINDS {
        *counts.entry(kind.category()).or_insert(0) += 1;
    }

    assert_eq!(counts.get(&NodeKindCategory::Container), Some(&5));
    assert_eq!(counts.get(&NodeKindCategory::Callable), Some(&3));
    assert_eq!(counts.get(&NodeKindCategory::TypeDefining), Some(&6));
    assert_eq!(counts.get(&NodeKindCategory::SubSymbol), Some(&5));
    assert_eq!(counts.get(&NodeKindCategory::NonCode), Some(&4));
    assert_eq!(counts.get(&NodeKindCategory::PartialKnowledge), Some(&1));

    let total: usize = counts.values().sum();
    assert_eq!(total, ALL_NODE_KINDS.len());
}

// ─── NodeKindCategory: parallel bijection / exhaustiveness ───────────

#[test]
fn category_name_and_from_name_round_trip_for_every_variant() {
    for &cat in ALL_NODE_KIND_CATEGORIES {
        let name = cat.name();
        let back = NodeKindCategory::from_name(name).unwrap_or_else(|err| {
            panic!(
                "from_name({name:?}) returned Err({err}) for category \
                 {cat:?}; the mapping in NodeKindCategory::name and \
                 NodeKindCategory::from_name must stay in sync"
            )
        });
        assert_eq!(
            back, cat,
            "round-trip for {cat:?}: name() = {name:?}, but \
             from_name({name:?}) = {back:?}"
        );
    }
}

#[test]
fn category_from_name_returns_err_for_unknown_strings() {
    let bad_inputs = ["", "Container", "containers", "type-defining", "unknown"];
    for bad in bad_inputs {
        let err = NodeKindCategory::from_name(bad).expect_err(&format!(
            "from_name({bad:?}) should have returned Err"
        ));
        assert_eq!(err.given(), bad);
    }
}

#[test]
fn all_node_kind_categories_is_exhaustive() {
    // Mirrors all_node_kinds_is_exhaustive. The expected count of 6 is
    // the contract; adding a category is itself a contract change per
    // NodeKindCategory's doc comment.
    const EXPECTED_TOTAL: usize = 6;
    assert_eq!(
        ALL_NODE_KIND_CATEGORIES.len(),
        EXPECTED_TOTAL,
        "ALL_NODE_KIND_CATEGORIES has {} entries but the schema \
         currently has {} categories; adding a category is a contract \
         change — update the rest of the schema and bump EXPECTED_TOTAL",
        ALL_NODE_KIND_CATEGORIES.len(),
        EXPECTED_TOTAL,
    );
}

#[test]
fn all_node_kind_categories_is_unique() {
    let mut seen = BTreeSet::new();
    for &cat in ALL_NODE_KIND_CATEGORIES {
        assert!(
            seen.insert(cat),
            "duplicate entry in ALL_NODE_KIND_CATEGORIES: {cat:?}"
        );
    }
}

#[test]
fn category_canonical_names_are_snake_case() {
    for &cat in ALL_NODE_KIND_CATEGORIES {
        let name = cat.name();
        assert!(!name.is_empty(), "name for {cat:?} is empty");
        for c in name.chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_',
                "name for {cat:?} has non-snake_case char {c:?} in {name:?}"
            );
        }
        assert!(!name.starts_with('_') && !name.ends_with('_'));
        assert!(
            !name.contains("__"),
            "category name for {cat:?} = {name:?} has double underscore"
        );
    }
}

#[test]
fn category_round_trips_through_json() {
    // The category appears in derived facts (`is_callable`, etc.) and may
    // be serialized in observability output. Same contract as NodeKind:
    // its serde form must match its canonical snake_case name.
    //
    // The expectations array is asserted exhaustive — if a new
    // category lands and isn't added here, the count check below trips
    // before any silent under-coverage occurs.
    let expectations = [
        (NodeKindCategory::Container, "container"),
        (NodeKindCategory::Callable, "callable"),
        (NodeKindCategory::TypeDefining, "type_defining"),
        (NodeKindCategory::SubSymbol, "sub_symbol"),
        (NodeKindCategory::NonCode, "non_code"),
        (NodeKindCategory::PartialKnowledge, "partial_knowledge"),
    ];
    assert_eq!(
        expectations.len(),
        ALL_NODE_KIND_CATEGORIES.len(),
        "expectations array is not exhaustive against \
         ALL_NODE_KIND_CATEGORIES; if you added a category, also add it \
         to this expectations list",
    );

    for (category, expected_name) in expectations {
        // The category's name() helper and its serde form must agree
        // with each other and with the expectations array.
        assert_eq!(
            category.name(),
            expected_name,
            "category.name() drift for {category:?}"
        );

        let json = serde_json::to_string(&category).unwrap();
        assert_eq!(
            json.trim_matches('"'),
            expected_name,
            "serde rename_all drift for {category:?}: serde said \
             {json:?}, expected {expected_name:?}",
        );
        let back: NodeKindCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, category);
    }
}
