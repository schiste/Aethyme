//! Cross-enum invariants for the schema's canonical-name spaces.
//!
//! Each of the four kind-like enums (`NodeKind`, `NodeKindCategory`,
//! `EdgeKind`, `EdgeKindCategory`) emits canonical snake_case names
//! via its own `name()` method. Those names appear together on the
//! wire — in fragment bodies, in NDJSON shards, in log lines, in
//! query filters. A collision between two enums' name spaces would
//! produce ambiguous wire-format diagnostics ("got 'contains' — is
//! that a node kind, an edge kind, or a category?") and ambiguous
//! parser routing.
//!
//! This file holds the cross-enum collision tests as a single
//! comprehensive sweep across all six pairs:
//!
//!   NodeKind         × EdgeKind
//!   NodeKind         × NodeKindCategory
//!   NodeKind         × EdgeKindCategory
//!   EdgeKind         × NodeKindCategory
//!   EdgeKind         × EdgeKindCategory
//!   NodeKindCategory × EdgeKindCategory
//!
//! Today no collisions exist. The point is to trap any future
//! addition that would silently introduce one — the schema's "no
//! versioning" rule means a collision shipped once is forever.

use std::collections::BTreeSet;

use aethyme_graph_schema::{
    ALL_EDGE_KIND_CATEGORIES, ALL_EDGE_KINDS, ALL_NODE_KIND_CATEGORIES,
    ALL_NODE_KINDS,
};

/// Collect the canonical names of an iterable of items with a
/// `name() -> &'static str` method.
fn names<T>(items: &[T], name_of: impl Fn(&T) -> &'static str) -> BTreeSet<&'static str> {
    items.iter().map(name_of).collect()
}

fn assert_disjoint(
    a_label: &str,
    a: &BTreeSet<&'static str>,
    b_label: &str,
    b: &BTreeSet<&'static str>,
) {
    let intersection: BTreeSet<_> = a.intersection(b).copied().collect();
    assert!(
        intersection.is_empty(),
        "canonical-name collision between {a_label} and {b_label}: \
         shared names = {intersection:?}; canonical-name spaces must \
         stay disjoint so wire-format diagnostics remain unambiguous",
    );
}

#[test]
fn node_kind_and_edge_kind_names_are_disjoint() {
    let nks = names(ALL_NODE_KINDS, |k| k.name());
    let eks = names(ALL_EDGE_KINDS, |k| k.name());
    assert_disjoint("NodeKind", &nks, "EdgeKind", &eks);
}

#[test]
fn node_kind_and_node_kind_category_names_are_disjoint() {
    let nks = names(ALL_NODE_KINDS, |k| k.name());
    let nkcs = names(ALL_NODE_KIND_CATEGORIES, |c| c.name());
    assert_disjoint("NodeKind", &nks, "NodeKindCategory", &nkcs);
}

#[test]
fn node_kind_and_edge_kind_category_names_are_disjoint() {
    let nks = names(ALL_NODE_KINDS, |k| k.name());
    let ekcs = names(ALL_EDGE_KIND_CATEGORIES, |c| c.name());
    assert_disjoint("NodeKind", &nks, "EdgeKindCategory", &ekcs);
}

#[test]
fn edge_kind_and_node_kind_category_names_are_disjoint() {
    let eks = names(ALL_EDGE_KINDS, |k| k.name());
    let nkcs = names(ALL_NODE_KIND_CATEGORIES, |c| c.name());
    assert_disjoint("EdgeKind", &eks, "NodeKindCategory", &nkcs);
}

#[test]
fn edge_kind_and_edge_kind_category_names_are_disjoint() {
    let eks = names(ALL_EDGE_KINDS, |k| k.name());
    let ekcs = names(ALL_EDGE_KIND_CATEGORIES, |c| c.name());
    assert_disjoint("EdgeKind", &eks, "EdgeKindCategory", &ekcs);
}

#[test]
fn node_kind_category_and_edge_kind_category_names_are_disjoint() {
    let nkcs = names(ALL_NODE_KIND_CATEGORIES, |c| c.name());
    let ekcs = names(ALL_EDGE_KIND_CATEGORIES, |c| c.name());
    assert_disjoint("NodeKindCategory", &nkcs, "EdgeKindCategory", &ekcs);
}

#[test]
fn all_canonical_names_in_a_single_space_are_unique() {
    // Combined sweep — every canonical name across all four enums
    // must be unique. If any pair-wise test above fails, this one
    // will too; it serves as a defense-in-depth single-shot check
    // that catches the same hazard.
    let mut combined: BTreeSet<(&'static str, &'static str)> = BTreeSet::new();
    let push = |s: &mut BTreeSet<(&'static str, &'static str)>,
                origin: &'static str,
                name: &'static str| {
        assert!(
            s.insert((name, origin)),
            "duplicate canonical name {name:?} (from {origin}); \
             check the pair-wise disjoint tests for which enums \
             collided",
        );
    };
    // Insert (name, origin) so that a duplicate name from a different
    // origin still trips the assertion. We need to dedupe by name only,
    // not by (name, origin), so track separately:
    let mut seen_names: BTreeSet<&'static str> = BTreeSet::new();
    for &k in ALL_NODE_KINDS {
        push(&mut combined, "NodeKind", k.name());
        assert!(
            seen_names.insert(k.name()),
            "duplicate canonical name across all enums: {:?}",
            k.name()
        );
    }
    for &c in ALL_NODE_KIND_CATEGORIES {
        push(&mut combined, "NodeKindCategory", c.name());
        assert!(
            seen_names.insert(c.name()),
            "duplicate canonical name across all enums: {:?}",
            c.name()
        );
    }
    for &k in ALL_EDGE_KINDS {
        push(&mut combined, "EdgeKind", k.name());
        assert!(
            seen_names.insert(k.name()),
            "duplicate canonical name across all enums: {:?}",
            k.name()
        );
    }
    for &c in ALL_EDGE_KIND_CATEGORIES {
        push(&mut combined, "EdgeKindCategory", c.name());
        assert!(
            seen_names.insert(c.name()),
            "duplicate canonical name across all enums: {:?}",
            c.name()
        );
    }

    // The combined count should equal the sum of all four enum counts.
    let expected = ALL_NODE_KINDS.len()
        + ALL_NODE_KIND_CATEGORIES.len()
        + ALL_EDGE_KINDS.len()
        + ALL_EDGE_KIND_CATEGORIES.len();
    assert_eq!(seen_names.len(), expected);
}
