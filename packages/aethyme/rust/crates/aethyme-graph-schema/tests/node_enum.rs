//! Integration tests for the unifying `Node` enum.

use std::collections::BTreeMap;

use aethyme_graph_schema::{
    Class, Function, Module, Node, NodeKind, Package, ParameterSignature, Repository, SourceRange,
    SurfaceFlowNode, Visibility,
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

#[test]
fn kind_method_returns_correct_node_kind_for_every_variant() {
    let f = sample_function();
    let n: Node = f.into();
    assert_eq!(n.kind(), NodeKind::Function);

    let m = Module::new("aethyme", "src/cli", "cli", "python").unwrap();
    let n: Node = m.into();
    assert_eq!(n.kind(), NodeKind::Module);

    let c = Class::new(
        "aethyme",
        "src/x.py",
        "Foo",
        SourceRange::new(1, 10).unwrap(),
        Visibility::Public,
    )
    .unwrap();
    let n: Node = c.into();
    assert_eq!(n.kind(), NodeKind::Class);

    let r = Repository::new("aethyme", "/p", "git").unwrap();
    let n: Node = r.into();
    assert_eq!(n.kind(), NodeKind::Repository);

    let p = Package::new(
        "aethyme",
        "rust",
        "aethyme-engine",
        "rust/Cargo.toml",
        "cargo",
    )
    .unwrap();
    let n: Node = p.into();
    assert_eq!(n.kind(), NodeKind::Package);

    let route = SurfaceFlowNode::new(
        NodeKind::RouteSurface,
        "aethyme",
        "src/routes.py",
        "GET /api/token",
        "Django URL route",
        SourceRange::new(12, 12).unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let n = Node::RouteSurface(route);
    assert_eq!(n.kind(), NodeKind::RouteSurface);
}

#[test]
fn id_method_returns_the_inner_node_id() {
    let f = sample_function();
    let inner_id = {
        use aethyme_graph_schema::Callable;
        f.id().clone()
    };
    let n: Node = f.into();
    assert_eq!(n.id(), &inner_id);
}

#[test]
fn surface_flow_node_variants_expose_the_inner_node_id_and_name() {
    let surface = SurfaceFlowNode::new(
        NodeKind::WorkerSurface,
        "aethyme",
        "worker.ts",
        "fetch",
        "Cloudflare-style fetch handler",
        SourceRange::new(3, 8).unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let inner_id = surface.id().clone();
    let n = Node::WorkerSurface(surface);
    assert_eq!(n.id(), &inner_id);
    assert_eq!(n.name(), Some("fetch"));
}

#[test]
fn from_impl_works_for_each_variant() {
    // Sweep check via the From impls — each kind's struct should
    // convert into Node::Variant via the macro-generated From.
    let _: Node = Repository::new("a", "p", "git").unwrap().into();
    let _: Node = Module::new("a", "p", "n", "py").unwrap().into();
    let _: Node = sample_function().into();
    let _: Node = Class::new(
        "a",
        "p",
        "C",
        SourceRange::new(1, 1).unwrap(),
        Visibility::Public,
    )
    .unwrap()
    .into();
    let _: Node = Package::new("a", "rust", "a-engine", "rust/Cargo.toml", "cargo")
        .unwrap()
        .into();
}

#[test]
fn node_serde_round_trip_through_json() {
    let f = sample_function();
    let n: Node = f.clone().into();
    let json = serde_json::to_string(&n).unwrap();
    // Externally tagged shape: variant name is the field key.
    assert!(
        json.starts_with("{\"function\":{"),
        "expected externally-tagged shape, got {json}"
    );
    let back: Node = serde_json::from_str(&json).unwrap();
    assert_eq!(back, n);
}

#[test]
fn node_serde_round_trip_through_bincode() {
    // The whole point of switching to externally-tagged + the
    // Node enum: bincode round-trip works for the full Node
    // payload. This is what makes per-file binary fragments
    // possible.
    let n: Node = sample_function().into();
    let bytes = bincode::serialize(&n).unwrap();
    let back: Node = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back, n);
}
