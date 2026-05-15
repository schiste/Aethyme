//! Integration tests for Fragment construction + binary encoding.

use aethyme_graph_schema::{
    Class, Confidence, Edge, EdgeAttributes, EdgeSite, Function, Node,
    NodeId, ParameterSignature, Source, SourceRange, Visibility,
};
use aethyme_graph_storage::{
    read_fragment_bytes, write_fragment_bytes, Fragment,
    FragmentBuildError, FragmentDecodeError,
};

fn sample_function() -> Function {
    Function::new(
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
    .unwrap()
}

fn sample_class() -> Class {
    Class::new(
        "aethyme",
        "src/cli.py",
        "Helper",
        SourceRange::new(60, 100).unwrap(),
        Visibility::Public,
    )
    .unwrap()
}

fn sample_call_edge(src: NodeId, dst: NodeId) -> Edge {
    Edge::new(
        src,
        dst,
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    )
    .with_site(EdgeSite {
        line: 25,
        is_in_branch: false,
        is_in_loop: false,
        kind_tag: "direct".into(),
    })
}

// ─── Construction validation ────────────────────────────────────────

#[test]
fn fragment_rejects_empty_file_path() {
    let err = Fragment::new("", vec![], vec![]).unwrap_err();
    assert_eq!(err, FragmentBuildError::EmptyFilePath);
}

#[test]
fn fragment_dedupes_duplicate_node_ids_silently() {
    // The pre-fix behavior was an error on duplicates. The real-
    // world Python @overload pattern produces multiple AST nodes
    // that hash to the same NodeId; the indexer doesn't always
    // know in advance, so Fragment silently keeps the first.
    let f = sample_function();
    let nodes = vec![Node::Function(f.clone()), Node::Function(f.clone())];
    let frag = Fragment::new("src/cli.py", nodes, vec![]).unwrap();
    assert_eq!(frag.node_count(), 1);
}

#[test]
fn fragment_dedupes_duplicate_edges_silently() {
    let f = sample_function();
    let c = sample_class();
    use aethyme_graph_schema::Callable;
    let edges = vec![
        sample_call_edge(f.id().clone(), c.id().clone()),
        sample_call_edge(f.id().clone(), c.id().clone()),
    ];
    let nodes = vec![Node::Function(f), Node::Class(c)];
    let frag = Fragment::new("src/cli.py", nodes, edges).unwrap();
    assert_eq!(frag.edge_count(), 1);
}

// ─── Canonical ordering ─────────────────────────────────────────────

#[test]
fn fragment_canonicalizes_node_order() {
    // Build with two nodes; assert that whichever order we pass
    // them in, the resulting Fragment has them sorted by id.
    let f = sample_function();
    let c = sample_class();
    let n_f = Node::Function(f);
    let n_c = Node::Class(c);

    let frag_a = Fragment::new(
        "src/cli.py",
        vec![n_f.clone(), n_c.clone()],
        vec![],
    )
    .unwrap();
    let frag_b = Fragment::new(
        "src/cli.py",
        vec![n_c.clone(), n_f.clone()],
        vec![],
    )
    .unwrap();
    assert_eq!(frag_a, frag_b);
    // Verify the sorted order matches both.
    let ids_a: Vec<&str> =
        frag_a.nodes().iter().map(|n| n.id().as_str()).collect();
    let ids_b: Vec<&str> =
        frag_b.nodes().iter().map(|n| n.id().as_str()).collect();
    assert_eq!(ids_a, ids_b);
    // And that the sort actually happened: ids are in lex order.
    let mut sorted = ids_a.clone();
    sorted.sort();
    assert_eq!(ids_a, sorted);
}

#[test]
fn fragment_canonicalizes_edge_order() {
    use aethyme_graph_schema::Callable;
    let f = sample_function();
    let c = sample_class();
    let fn_to_cls = sample_call_edge(f.id().clone(), c.id().clone());
    let cls_to_fn = Edge::new(
        c.id().clone(),
        f.id().clone(),
        EdgeAttributes::Uses,
        Source::Code,
        Confidence::FULL,
    );
    let nodes = vec![Node::Function(f), Node::Class(c)];

    let frag_a = Fragment::new(
        "src/cli.py",
        nodes.clone(),
        vec![fn_to_cls.clone(), cls_to_fn.clone()],
    )
    .unwrap();
    let frag_b = Fragment::new(
        "src/cli.py",
        nodes.clone(),
        vec![cls_to_fn.clone(), fn_to_cls.clone()],
    )
    .unwrap();
    assert_eq!(frag_a, frag_b);
}

// ─── Bincode round-trip ─────────────────────────────────────────────

#[test]
fn fragment_round_trips_through_bincode() {
    let f = sample_function();
    let c = sample_class();
    use aethyme_graph_schema::Callable;
    let edge = sample_call_edge(f.id().clone(), c.id().clone());
    let frag = Fragment::new(
        "src/cli.py",
        vec![Node::Function(f), Node::Class(c)],
        vec![edge],
    )
    .unwrap();

    let bytes = write_fragment_bytes(&frag).unwrap();
    let back = read_fragment_bytes(&bytes).unwrap();
    assert_eq!(back, frag);
    assert_eq!(back.file_path(), "src/cli.py");
    assert_eq!(back.node_count(), 2);
    assert_eq!(back.edge_count(), 1);
}

#[test]
fn empty_fragment_round_trips() {
    let frag = Fragment::new("src/empty.py", vec![], vec![]).unwrap();
    let bytes = write_fragment_bytes(&frag).unwrap();
    let back = read_fragment_bytes(&bytes).unwrap();
    assert_eq!(back, frag);
    assert_eq!(back.node_count(), 0);
    assert_eq!(back.edge_count(), 0);
}

// ─── Determinism ────────────────────────────────────────────────────

#[test]
fn same_inputs_produce_byte_identical_fragments() {
    // Build the same Fragment 100 times; every encoding must be
    // byte-identical. This is THE load-bearing property for
    // Option C's merge story.
    use aethyme_graph_schema::Callable;

    fn build() -> Vec<u8> {
        let f = sample_function();
        let c = sample_class();
        let edge = sample_call_edge(f.id().clone(), c.id().clone());
        let frag = Fragment::new(
            "src/cli.py",
            vec![Node::Function(f), Node::Class(c)],
            vec![edge],
        )
        .unwrap();
        write_fragment_bytes(&frag).unwrap()
    }

    let first = build();
    for _ in 0..100 {
        assert_eq!(build(), first);
    }
}

#[test]
fn different_input_order_produces_same_bytes() {
    // The canonical-sort step in Fragment::new must make the byte
    // output order-independent.
    let f = sample_function();
    let c = sample_class();
    let n_f = Node::Function(f);
    let n_c = Node::Class(c);

    let frag_a =
        Fragment::new("src/cli.py", vec![n_f.clone(), n_c.clone()], vec![])
            .unwrap();
    let frag_b =
        Fragment::new("src/cli.py", vec![n_c.clone(), n_f.clone()], vec![])
            .unwrap();
    let bytes_a = write_fragment_bytes(&frag_a).unwrap();
    let bytes_b = write_fragment_bytes(&frag_b).unwrap();
    assert_eq!(bytes_a, bytes_b);
}

// ─── Schema version handling ────────────────────────────────────────

#[test]
fn fragment_carries_schema_version() {
    let frag = Fragment::new("src/x.py", vec![], vec![]).unwrap();
    assert_eq!(frag.schema_version(), Fragment::SCHEMA_VERSION);
}

#[test]
fn read_rejects_unknown_schema_version() {
    // Construct a Fragment with a bogus schema_version by going
    // through serde with a hand-crafted JSON, then re-encoding to
    // bincode. This simulates a fragment written with a future
    // wire format.
    let frag = Fragment::new("src/x.py", vec![], vec![]).unwrap();
    let mut bytes = write_fragment_bytes(&frag).unwrap();
    // Mutate the schema_version: the second field in the bincode
    // serialization (after file_path Box<str>). Easier: just
    // construct a Fragment with the wrong version via a manual
    // serde Value detour.
    //
    // bincode 1 encodes a Box<str> as a u64 length prefix + bytes.
    // The Fragment shape is (file_path: Box<str>, schema_version:
    // u32, nodes: Vec<Node>, edges: Vec<Edge>). For "src/x.py"
    // (8 bytes), the layout is:
    //   bytes[0..8]  = length 8 (u64 LE)
    //   bytes[8..16] = "src/x.py"
    //   bytes[16..20] = schema_version (u32 LE)
    //   ...
    let version_offset = 8 + "src/x.py".len();
    bytes[version_offset] = 99; // change low byte
    let err = read_fragment_bytes(&bytes).unwrap_err();
    assert!(matches!(
        err,
        FragmentDecodeError::SchemaVersionMismatch { .. }
    ));
}
