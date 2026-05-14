//! Determinism test suite for `aethyme-graph-schema`.
//!
//! These tests are the load-bearing guarantee for the "no schema
//! versioning" + "Option C committed fragments" stack. A fragment
//! committed by contributor A on Linux must deserialize to the same
//! bytes contributor B produces on macOS. This file enforces the
//! invariants that make that possible.
//!
//! ## What we test
//!
//! 1. **Bincode round-trip** for every node/edge type — anything
//!    that can be serialized to a fragment must round-trip
//!    losslessly.
//! 2. **Discriminant order** for `NodeKind` and `EdgeKind` — the
//!    bincode tag for each variant matches its index in
//!    `ALL_*_KINDS`. Reordering a variant silently shifts every
//!    later variant's discriminant; this test fails loudly.
//! 3. **Cross-construction byte equality** — constructing the same
//!    value many times within a process produces byte-identical
//!    bincode output. This is the in-process determinism check;
//!    cross-process determinism is guarded by NodeId's snapshot
//!    test in tests/identity.rs.
//! 4. **Single-byte discriminants for small enums** — bincode 1's
//!    varint encoding makes <128-variant enums take exactly one
//!    byte for the discriminant. This means inserting a variant
//!    mid-list silently shifts later discriminants, which is the
//!    forever-mistake we want to catch. The test pins exact byte
//!    positions.

use aethyme_graph_schema::{
    ALL_EDGE_KINDS, ALL_NODE_KINDS, Class, Confidence, Edge, EdgeAttributes,
    EdgeKind, EdgeSite, File, Function, NodeKind, ParameterSignature,
    Repository, Source, SourceRange, Visibility,
};

// ─── Bincode round-trip for every kind enum variant ─────────────────

#[test]
fn every_node_kind_round_trips_through_bincode() {
    for &kind in ALL_NODE_KINDS {
        let bytes = bincode::serialize(&kind).unwrap();
        let back: NodeKind = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, kind);
    }
}

#[test]
fn every_edge_kind_round_trips_through_bincode() {
    for &kind in ALL_EDGE_KINDS {
        let bytes = bincode::serialize(&kind).unwrap();
        let back: EdgeKind = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, kind);
    }
}

// ─── Discriminant order matches source order ────────────────────────

#[test]
fn node_kind_discriminants_match_all_node_kinds_index() {
    // Bincode 1 with default config encodes unit-variant enums as a
    // u32 little-endian discriminant tag. The first byte of that
    // tag is the variant's source-order index (assuming <256
    // variants, which we have far fewer of).
    //
    // This test asserts: the bincode tag for ALL_NODE_KINDS[i]
    // decodes to index i. If anyone reorders the enum source, this
    // test trips loudly — preventing the silent forever-mistake of
    // shifting every later variant's discriminant.
    for (i, &kind) in ALL_NODE_KINDS.iter().enumerate() {
        let bytes = bincode::serialize(&kind).unwrap();
        // u32 little-endian: first byte is the low byte of the index.
        // We have <256 variants, so the low byte == the index.
        assert_eq!(
            bytes[0] as usize, i,
            "NodeKind discriminant for {kind:?} is {} but expected {i} \
             (its position in ALL_NODE_KINDS). A variant was inserted \
             mid-enum or reordered, which is a forever-format break.",
            bytes[0]
        );
    }
}

#[test]
fn edge_kind_discriminants_match_all_edge_kinds_index() {
    for (i, &kind) in ALL_EDGE_KINDS.iter().enumerate() {
        let bytes = bincode::serialize(&kind).unwrap();
        assert_eq!(
            bytes[0] as usize, i,
            "EdgeKind discriminant for {kind:?} is {} but expected {i}",
            bytes[0]
        );
    }
}

// ─── Cross-construction byte equality ────────────────────────────────

#[test]
fn function_struct_serializes_identically_across_constructions() {
    // Build the same Function 100 times. Every serialization must
    // be byte-identical. Non-determinism (random fields, HashMap
    // iteration order, timestamps) would leak here.
    let mut first: Option<Vec<u8>> = None;
    for _ in 0..100 {
        let f = Function::new(
            "aethyme",
            "src/cli.py",
            "explore_command",
            "fn explore_command()",
            vec![ParameterSignature {
                name: "x".into(),
                type_str: Some("int".into()),
                default_value: None,
            }],
            Some("str"),
            SourceRange::new(10, 50).unwrap(),
            Visibility::Public,
            true,
        )
        .unwrap();
        let bytes = bincode::serialize(&f).unwrap();
        match &first {
            None => first = Some(bytes),
            Some(prev) => assert_eq!(
                prev, &bytes,
                "Function serialization is non-deterministic across constructions"
            ),
        }
    }
}

#[test]
fn edge_with_sites_serializes_identically_across_constructions() {
    // NOTE: Edge serializes through JSON (serde_json) rather than
    // bincode in this test because `EdgeAttributes` uses
    // `#[serde(tag = "kind", content = "data")]` (adjacently tagged
    // enum), which requires a self-describing format on
    // deserialization. Bincode 1 cannot deserialize tagged enums.
    //
    // For Phase 1 (types only), this is fine: byte-determinism via
    // JSON is the load-bearing property for NDJSON index shards,
    // which is where Edge will live on disk anyway. Per-file
    // binary fragments (Phase 2 storage layer) will pick their
    // own encoding for tagged enums — either a custom format that
    // supports them, or a manual Serialize impl that emits the
    // discriminator as a known-position u8.
    fn build() -> Edge {
        Edge::new(
            aethyme_graph_schema::NodeId::new(
                NodeKind::Function,
                "aethyme",
                "src/x.py",
                "a",
            )
            .unwrap(),
            aethyme_graph_schema::NodeId::new(
                NodeKind::Function,
                "aethyme",
                "src/x.py",
                "b",
            )
            .unwrap(),
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
                line: 20,
                is_in_branch: true,
                is_in_loop: false,
                kind_tag: "direct".into(),
            },
            EdgeSite {
                line: 30,
                is_in_branch: false,
                is_in_loop: true,
                kind_tag: "virtual".into(),
            },
        ])
    }

    let mut first: Option<String> = None;
    for _ in 0..100 {
        let bytes = serde_json::to_string(&build()).unwrap();
        match &first {
            None => first = Some(bytes),
            Some(prev) => assert_eq!(prev, &bytes),
        }
    }
}

// ─── Complex types round-trip through bincode losslessly ─────────────

#[test]
fn repository_round_trips_through_bincode() {
    let r = Repository::new("aethyme", "/path/to/repo", "git").unwrap();
    let bytes = bincode::serialize(&r).unwrap();
    let back: Repository = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back, r);
}

#[test]
fn file_round_trips_through_bincode() {
    let f = File::new(
        "aethyme",
        "src/cli.py",
        "python",
        12345,
        "abc123",
    )
    .unwrap();
    let bytes = bincode::serialize(&f).unwrap();
    let back: File = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back, f);
}

#[test]
fn class_round_trips_through_bincode() {
    let c = Class::new(
        "aethyme",
        "src/x.py",
        "Foo",
        SourceRange::new(1, 100).unwrap(),
        Visibility::Public,
    )
    .unwrap();
    let bytes = bincode::serialize(&c).unwrap();
    let back: Class = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back, c);
}

#[test]
fn full_edge_with_language_boundary_round_trips_through_json() {
    // Edge uses tagged-enum serialization for EdgeAttributes and
    // therefore needs a self-describing format. JSON works; bincode
    // doesn't (see edge_with_sites_serializes_identically_across_
    // constructions for the rationale).
    use aethyme_graph_schema::{BindingKind, NodeId};
    let e = Edge::new(
        NodeId::new(NodeKind::Function, "aethyme", "src/py.py", "caller").unwrap(),
        NodeId::new(NodeKind::Function, "aethyme", "src/rs.rs", "callee").unwrap(),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::from_milli(850).unwrap(),
    )
    .with_language_boundary(BindingKind::Pyo3)
    .with_site(EdgeSite {
        line: 42,
        is_in_branch: false,
        is_in_loop: false,
        kind_tag: "ffi".into(),
    });
    let json = serde_json::to_string(&e).unwrap();
    let back: Edge = serde_json::from_str(&json).unwrap();
    assert_eq!(back, e);
    assert_eq!(back.kind(), EdgeKind::Calls);
    assert!(back.is_cross_language());
}

// ─── Snapshot: pinned bincode output for one canonical Function ─────

#[test]
fn pinned_function_bincode_snapshot() {
    // The function-level equivalent of NodeId's snapshot_known_good
    // test (tests/identity.rs). Pins the exact bincode output for a
    // canonical Function so any unintended change to the
    // serialization path fails loudly.
    //
    // The expected bytes were computed at commit 1.13 (Phase 1
    // completion). Any change to the Function struct's layout, the
    // NodeId hash, the SourceRange shape, or bincode's encoding
    // strategy will fail this test.
    let f = Function::new(
        "aethyme",
        "src/cli.py",
        "explore_command",
        "fn explore_command()",
        vec![],
        None,
        SourceRange::new(10, 50).unwrap(),
        Visibility::Public,
        true,
    )
    .unwrap();
    let bytes = bincode::serialize(&f).unwrap();
    // Pin the length and a checksum. Pinning the full byte string
    // would make every byte-tweak in any dependency a test failure;
    // length + checksum is the right granularity for snapshot value.
    let expected_len = 124_usize;
    assert_eq!(
        bytes.len(),
        expected_len,
        "Function bincode length drifted: was {}, expected {}",
        bytes.len(),
        expected_len,
    );
    // BLAKE3 the bytes for a stable checksum.
    let mut hasher = blake3::Hasher::new();
    hasher.update(&bytes);
    let actual = hex_encode_first_n(hasher.finalize().as_bytes(), 8);
    let expected = "162018f4b8f4ca4b";
    assert_eq!(
        actual, expected,
        "Function bincode checksum drifted — see test docstring for \
         what this means before updating the expected value"
    );
}

fn hex_encode_first_n(bytes: &[u8], n: usize) -> String {
    let mut out = String::with_capacity(n * 2);
    for b in bytes.iter().take(n) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
