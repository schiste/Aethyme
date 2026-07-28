//! Integration tests for the full Edge struct and per-kind attributes.

use aethyme_graph_schema::{
    BindingKind, Confidence, DecisionStatus, Edge, EdgeAttributes, EdgeKind, EdgeSite, NodeId,
    NodeKind, ReferenceKindHint, Source,
};

fn fn_id(name: &str) -> NodeId {
    NodeId::new(NodeKind::Function, "aethyme", "src/x.py", name).unwrap()
}

fn class_id(name: &str) -> NodeId {
    NodeId::new(NodeKind::Class, "aethyme", "src/x.py", name).unwrap()
}

// ─── Edge::kind() is derived from EdgeAttributes ────────────────────

#[test]
fn edge_kind_is_derived_from_attributes() {
    let e = Edge::new(
        fn_id("a"),
        fn_id("b"),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    );
    assert_eq!(e.kind(), EdgeKind::Calls);

    let e = Edge::new(
        fn_id("a"),
        class_id("Foo"),
        EdgeAttributes::Contains,
        Source::Structure,
        Confidence::FULL,
    );
    assert_eq!(e.kind(), EdgeKind::Contains);
}

#[test]
fn every_edge_attribute_variant_maps_to_a_distinct_kind() {
    use aethyme_graph_schema::ALL_EDGE_KINDS;
    // Build one EdgeAttributes per kind. Construct edges. Collect
    // resulting kinds. The set should equal ALL_EDGE_KINDS.
    let attrs = [
        EdgeAttributes::Contains,
        EdgeAttributes::Defines,
        EdgeAttributes::Imports {
            import_path: "foo".into(),
            is_namespace: false,
            is_default: false,
            is_named: true,
        },
        EdgeAttributes::Calls,
        EdgeAttributes::Implements,
        EdgeAttributes::Inherits,
        EdgeAttributes::Reads,
        EdgeAttributes::Uses,
        EdgeAttributes::Writes,
        EdgeAttributes::Mocks,
        EdgeAttributes::Tests {
            assertion_count: None,
        },
        EdgeAttributes::Configures {
            read_at_runtime: false,
        },
        EdgeAttributes::Decides {
            decision_status: DecisionStatus::Active,
        },
        EdgeAttributes::Deprecates {
            since_version: None,
        },
        EdgeAttributes::Documents,
        EdgeAttributes::References {
            kind_hint: ReferenceKindHint::SeeAlso,
        },
        EdgeAttributes::Authorizes,
        EdgeAttributes::Exposes,
        EdgeAttributes::ForwardsTo,
        EdgeAttributes::InstallsMiddleware,
        EdgeAttributes::IssuesCredential,
        EdgeAttributes::StoresCredential,
        EdgeAttributes::UsesCredential,
        EdgeAttributes::ValidatesCredential,
        EdgeAttributes::RewritesHeader,
        EdgeAttributes::TestedBy,
    ];
    let kinds: std::collections::BTreeSet<EdgeKind> = attrs.iter().map(|a| a.kind()).collect();
    let expected: std::collections::BTreeSet<EdgeKind> = ALL_EDGE_KINDS.iter().copied().collect();
    assert_eq!(kinds, expected);
}

// ─── Builder methods ─────────────────────────────────────────────────

#[test]
fn builder_with_language_boundary_marks_cross_language() {
    let e = Edge::new(
        fn_id("py_caller"),
        fn_id("rust_callee"),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    )
    .with_language_boundary(BindingKind::Pyo3);
    assert!(e.is_cross_language());
    assert_eq!(e.binding_kind(), Some(BindingKind::Pyo3));
}

#[test]
fn builder_with_site_appends_sites() {
    let e = Edge::new(
        fn_id("a"),
        fn_id("b"),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    )
    .with_site(EdgeSite {
        line: 10,
        is_in_branch: false,
        is_in_loop: false,
        kind_tag: "direct".into(),
    })
    .with_site(EdgeSite {
        line: 20,
        is_in_branch: true,
        is_in_loop: false,
        kind_tag: "direct".into(),
    });
    assert_eq!(e.sites().len(), 2);
    assert_eq!(e.sites()[0].line, 10);
    assert_eq!(e.sites()[1].line, 20);
    assert!(e.sites()[1].is_in_branch);
}

#[test]
fn multi_edge_collapses_via_sites_attribute() {
    // The schema's multi-edge policy: three call sites for the same
    // (src, dst, kind) → one Edge with three sites, not three Edges.
    let e = Edge::new(
        fn_id("a"),
        fn_id("b"),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    )
    .with_sites(vec![
        EdgeSite {
            line: 10,
            is_in_branch: false,
            is_in_loop: false,
            kind_tag: "direct".into(),
        },
        EdgeSite {
            line: 50,
            is_in_branch: false,
            is_in_loop: true,
            kind_tag: "direct".into(),
        },
        EdgeSite {
            line: 100,
            is_in_branch: true,
            is_in_loop: false,
            kind_tag: "virtual".into(),
        },
    ]);
    assert_eq!(e.sites().len(), 3);
    assert_eq!(e.kind(), EdgeKind::Calls);
}

// ─── Per-kind attribute carriage ─────────────────────────────────────

#[test]
fn imports_attributes_carry_import_metadata() {
    let e = Edge::new(
        fn_id("importer"),
        fn_id("imported"),
        EdgeAttributes::Imports {
            import_path: "package.module".into(),
            is_namespace: false,
            is_default: true,
            is_named: false,
        },
        Source::Structure,
        Confidence::FULL,
    );
    match e.attributes() {
        EdgeAttributes::Imports {
            import_path,
            is_default,
            ..
        } => {
            assert_eq!(&**import_path, "package.module");
            assert!(*is_default);
        }
        _ => panic!("expected Imports attributes"),
    }
}

#[test]
fn decides_carries_decision_status() {
    for status in [
        DecisionStatus::Active,
        DecisionStatus::Superseded,
        DecisionStatus::Rejected,
    ] {
        let e = Edge::new(
            fn_id("doc"),
            fn_id("target"),
            EdgeAttributes::Decides {
                decision_status: status,
            },
            Source::Derived,
            Confidence::FULL,
        );
        match e.attributes() {
            EdgeAttributes::Decides { decision_status } => {
                assert_eq!(*decision_status, status);
            }
            _ => panic!("expected Decides"),
        }
    }
}

#[test]
fn deprecates_with_and_without_version() {
    let with = EdgeAttributes::Deprecates {
        since_version: Some("2.0".into()),
    };
    let without = EdgeAttributes::Deprecates {
        since_version: None,
    };
    assert_ne!(with, without);
}

// ─── Serde round-trip ────────────────────────────────────────────────

#[test]
fn edge_serde_round_trip_preserves_all_fields() {
    let e = Edge::new(
        fn_id("a"),
        fn_id("b"),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::from_milli(750).unwrap(),
    )
    .with_language_boundary(BindingKind::Pyo3)
    .with_site(EdgeSite {
        line: 42,
        is_in_branch: true,
        is_in_loop: false,
        kind_tag: "direct".into(),
    });

    let json = serde_json::to_string(&e).unwrap();
    let back: Edge = serde_json::from_str(&json).unwrap();
    assert_eq!(back, e);
    assert_eq!(back.kind(), EdgeKind::Calls);
    assert!(back.is_cross_language());
    assert_eq!(back.binding_kind(), Some(BindingKind::Pyo3));
    assert_eq!(back.sites().len(), 1);
}

#[test]
fn edge_attributes_externally_tagged_serde_shape() {
    // EdgeAttributes uses default serde enum tagging (externally
    // tagged) — the variant name is the JSON object's outer field
    // key. This shape works with bincode, unlike internally /
    // adjacently tagged. See edge_struct.rs's EdgeAttributes
    // doc-comment for the rationale.
    let a = EdgeAttributes::Imports {
        import_path: "x".into(),
        is_namespace: false,
        is_default: false,
        is_named: true,
    };
    let json = serde_json::to_string(&a).unwrap();
    assert!(
        json.starts_with("{\"imports\":{"),
        "expected externally-tagged shape {{\"imports\":{{...}}}}, got {json}"
    );
    let back: EdgeAttributes = serde_json::from_str(&json).unwrap();
    assert_eq!(back, a);
}

#[test]
fn unit_variant_edge_attributes_serialize_as_bare_string() {
    // For unit-like variants, externally tagged emits just the
    // variant name as a string (not wrapped in an object). Confirm
    // and round-trip.
    let a = EdgeAttributes::Contains;
    let json = serde_json::to_string(&a).unwrap();
    assert_eq!(json, "\"contains\"");
    let back: EdgeAttributes = serde_json::from_str(&json).unwrap();
    assert_eq!(back, a);
}

// ─── Helper enums ────────────────────────────────────────────────────

#[test]
fn decision_status_canonical_names() {
    assert_eq!(DecisionStatus::Active.name(), "active");
    assert_eq!(DecisionStatus::Superseded.name(), "superseded");
    assert_eq!(DecisionStatus::Rejected.name(), "rejected");
}

#[test]
fn reference_kind_hint_canonical_names() {
    assert_eq!(ReferenceKindHint::SeeAlso.name(), "see_also");
    assert_eq!(ReferenceKindHint::Mentions.name(), "mentions");
    assert_eq!(ReferenceKindHint::Alternative.name(), "alternative");
}
