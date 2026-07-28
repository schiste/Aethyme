//! Integration tests for the `EdgeKind` and `EdgeKindCategory` enums.
//!
//! Mirrors `tests/kinds.rs` exactly in shape — the two enums share the
//! same invariants (bijection, exhaustiveness, snake_case format,
//! alphabetical-within-category for initial set, serde round-trip,
//! contiguous categories). When `tests/kinds.rs` adds an invariant,
//! add the parallel test here too.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_graph_schema::{ALL_EDGE_KIND_CATEGORIES, ALL_EDGE_KINDS, EdgeKind, EdgeKindCategory};

// ─── EdgeKind: bijection and exhaustiveness ──────────────────────────

#[test]
fn name_and_from_name_round_trip_for_every_variant() {
    for &kind in ALL_EDGE_KINDS {
        let name = kind.name();
        let back = EdgeKind::from_name(name).unwrap_or_else(|err| {
            panic!(
                "from_name({name:?}) returned Err({err}) for variant \
                 {kind:?}; the mapping in EdgeKind::name and \
                 EdgeKind::from_name must stay in sync"
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
    let bad_inputs = [
        "",
        "Calls",            // TitleCase
        "CALLS",            // ALLCAPS
        "call",             // truncated
        "called",           // wrong form
        "in-herits",        // kebab
        "implements_trait", // suffixed
    ];
    for bad in bad_inputs {
        let err = EdgeKind::from_name(bad)
            .expect_err(&format!("from_name({bad:?}) should have returned Err"));
        assert_eq!(err.given(), bad);
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
    for &kind in ALL_EDGE_KINDS {
        let name = kind.name();
        assert!(!name.is_empty(), "canonical name for {kind:?} is empty");
        for c in name.chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_',
                "canonical name for {kind:?} has non-snake_case char {c:?} \
                 in name = {name:?}"
            );
        }
        assert!(!name.starts_with('_') && !name.ends_with('_'));
        assert!(
            !name.contains("__"),
            "canonical name for {kind:?} = {name:?} has double underscore"
        );
    }
}

#[test]
fn all_edge_kinds_is_exhaustive() {
    // 24 edge kinds across 5 categories
    // (3 structural + 6 behavioral + 2 test + 5 documentation +
    // 8 surface-flow).
    // Bumping the count is the one-line change required when a new
    // edge kind lands.
    const EXPECTED_TOTAL: usize = 24;
    assert_eq!(
        ALL_EDGE_KINDS.len(),
        EXPECTED_TOTAL,
        "ALL_EDGE_KINDS has {} entries but the schema currently has {} \
         edge kinds; if you added an EdgeKind variant, append it to \
         ALL_EDGE_KINDS and bump EXPECTED_TOTAL here",
        ALL_EDGE_KINDS.len(),
        EXPECTED_TOTAL,
    );
}

#[test]
fn all_edge_kinds_is_unique() {
    let mut seen = BTreeSet::new();
    for &kind in ALL_EDGE_KINDS {
        assert!(
            seen.insert(kind),
            "duplicate entry in ALL_EDGE_KINDS: {kind:?}"
        );
    }
}

// ─── EdgeKind: serde round-trip ──────────────────────────────────────

#[test]
fn json_round_trip_is_identity_for_every_variant() {
    for &kind in ALL_EDGE_KINDS {
        let json = serde_json::to_string(&kind).unwrap();
        let back: EdgeKind = serde_json::from_str(&json).unwrap_or_else(|err| {
            panic!(
                "round-trip failed for {kind:?}: serialized to {json:?}, \
                 deserialization error = {err}"
            )
        });
        assert_eq!(back, kind);
    }
}

#[test]
fn json_form_matches_canonical_name() {
    for &kind in ALL_EDGE_KINDS {
        let json = serde_json::to_string(&kind).unwrap();
        let trimmed = json.trim_matches('"');
        assert_eq!(
            trimmed,
            kind.name(),
            "serde rename_all output {json:?} does not match \
             EdgeKind::name() = {:?} for {kind:?}",
            kind.name(),
        );
    }
}

// ─── EdgeKind: ordering and category contiguity ──────────────────────

#[test]
fn variants_are_alphabetical_within_each_category() {
    let mut group_start = 0;
    while group_start < ALL_EDGE_KINDS.len() {
        let head_category = ALL_EDGE_KINDS[group_start].category();
        let mut group_end = group_start + 1;
        while group_end < ALL_EDGE_KINDS.len()
            && ALL_EDGE_KINDS[group_end].category() == head_category
        {
            group_end += 1;
        }

        for i in group_start..group_end.saturating_sub(1) {
            let a = ALL_EDGE_KINDS[i].name();
            let b = ALL_EDGE_KINDS[i + 1].name();
            assert!(
                a < b,
                "ALL_EDGE_KINDS not alphabetical within {head_category:?}: \
                 {a:?} appears before {b:?} (positions {i} and {})",
                i + 1,
            );
        }

        group_start = group_end;
    }
}

#[test]
fn categories_appear_contiguously_in_all_edge_kinds() {
    let mut seen_categories = Vec::new();
    let mut current_category: Option<EdgeKindCategory> = None;
    for &kind in ALL_EDGE_KINDS {
        let cat = kind.category();
        if current_category != Some(cat) {
            assert!(
                !seen_categories.contains(&cat),
                "category {cat:?} reappears non-contiguously in \
                 ALL_EDGE_KINDS; once a category's block ends, the \
                 category may not appear again",
            );
            seen_categories.push(cat);
            current_category = Some(cat);
        }
    }
}

#[test]
fn categories_are_exhaustive_and_well_defined() {
    let mut counts: BTreeMap<EdgeKindCategory, usize> = BTreeMap::new();
    for &kind in ALL_EDGE_KINDS {
        *counts.entry(kind.category()).or_insert(0) += 1;
    }

    assert_eq!(counts.get(&EdgeKindCategory::Structural), Some(&3));
    assert_eq!(counts.get(&EdgeKindCategory::Behavioral), Some(&6));
    assert_eq!(counts.get(&EdgeKindCategory::Test), Some(&2));
    assert_eq!(counts.get(&EdgeKindCategory::Documentation), Some(&5));
    assert_eq!(counts.get(&EdgeKindCategory::SurfaceFlowEdge), Some(&8));

    let total: usize = counts.values().sum();
    assert_eq!(total, ALL_EDGE_KINDS.len());
}

// ─── EdgeKindCategory: parallel API ──────────────────────────────────

#[test]
fn category_name_and_from_name_round_trip_for_every_variant() {
    for &cat in ALL_EDGE_KIND_CATEGORIES {
        let name = cat.name();
        let back = EdgeKindCategory::from_name(name)
            .unwrap_or_else(|err| panic!("from_name({name:?}) returned Err({err}) for {cat:?}"));
        assert_eq!(back, cat);
    }
}

#[test]
fn category_from_name_returns_err_for_unknown_strings() {
    let bad_inputs = ["", "Structural", "structurals", "type-defining"];
    for bad in bad_inputs {
        let err = EdgeKindCategory::from_name(bad)
            .expect_err(&format!("from_name({bad:?}) should have returned Err"));
        assert_eq!(err.given(), bad);
    }
}

#[test]
fn all_edge_kind_categories_is_exhaustive() {
    const EXPECTED_TOTAL: usize = 5;
    assert_eq!(
        ALL_EDGE_KIND_CATEGORIES.len(),
        EXPECTED_TOTAL,
        "ALL_EDGE_KIND_CATEGORIES has {} entries but the schema \
         currently has {} categories",
        ALL_EDGE_KIND_CATEGORIES.len(),
        EXPECTED_TOTAL,
    );
}

#[test]
fn all_edge_kind_categories_is_unique() {
    let mut seen = BTreeSet::new();
    for &cat in ALL_EDGE_KIND_CATEGORIES {
        assert!(seen.insert(cat));
    }
}

#[test]
fn category_canonical_names_are_snake_case() {
    for &cat in ALL_EDGE_KIND_CATEGORIES {
        let name = cat.name();
        assert!(!name.is_empty());
        for c in name.chars() {
            assert!(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        }
        assert!(!name.starts_with('_') && !name.ends_with('_'));
        assert!(!name.contains("__"));
    }
}

#[test]
fn category_round_trips_through_json() {
    let expectations = [
        (EdgeKindCategory::Structural, "structural"),
        (EdgeKindCategory::Behavioral, "behavioral"),
        (EdgeKindCategory::Test, "test"),
        (EdgeKindCategory::Documentation, "documentation"),
        (EdgeKindCategory::SurfaceFlowEdge, "surface_flow_edge"),
    ];
    assert_eq!(
        expectations.len(),
        ALL_EDGE_KIND_CATEGORIES.len(),
        "expectations array is not exhaustive against \
         ALL_EDGE_KIND_CATEGORIES"
    );

    for (category, expected_name) in expectations {
        assert_eq!(category.name(), expected_name);
        let json = serde_json::to_string(&category).unwrap();
        assert_eq!(json.trim_matches('"'), expected_name);
        let back: EdgeKindCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, category);
    }
}
