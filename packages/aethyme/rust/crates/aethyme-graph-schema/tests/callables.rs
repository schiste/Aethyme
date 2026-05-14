//! Integration tests for callable node kinds and the Callable protocol.

use aethyme_graph_schema::{
    Callable, Function, FunctionConstructionError, Lambda, Method,
    MethodConstructionError, NodeId, NodeKind, ParameterSignature,
    SourceRange, Visibility,
};

fn range(s: u32, e: u32) -> SourceRange {
    SourceRange::new(s, e).unwrap()
}

fn class_id() -> NodeId {
    // A receiver-type ID for use in Method tests. Construction is
    // through the normal path; we don't import the Class struct
    // (commit 1.9) here, just the underlying NodeId.
    NodeId::new(NodeKind::Class, "aethyme", "src/foo.py", "FooClass").unwrap()
}

fn function_id() -> NodeId {
    NodeId::new(
        NodeKind::Function,
        "aethyme",
        "src/cli.py",
        "outer_function",
    )
    .unwrap()
}

// ─── Function ────────────────────────────────────────────────────────

#[test]
fn function_construction_and_callable_protocol() {
    let params = vec![ParameterSignature {
        name: "x".into(),
        type_str: Some("int".into()),
        default_value: None,
    }];
    let f = Function::new(
        "aethyme",
        "src/cli.py",
        "explore_command",
        "fn explore_command(x: int) -> str",
        params.clone(),
        Some("str"),
        range(10, 50),
        Visibility::Public,
        true,
    )
    .unwrap();

    // Callable protocol
    assert_eq!(f.name(), "explore_command");
    assert_eq!(f.signature(), "fn explore_command(x: int) -> str");
    assert_eq!(f.parameters(), &params[..]);
    assert_eq!(f.return_type(), Some("str"));
    assert_eq!(f.source_range(), range(10, 50));
    assert_eq!(f.visibility(), Visibility::Public);
    assert_eq!(f.id().kind(), NodeKind::Function);
    // Function-specific
    assert!(f.is_top_level());
}

#[test]
fn function_rejects_empty_name_and_path() {
    let bad = Function::new(
        "aethyme",
        "src/cli.py",
        "",
        "",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        true,
    );
    assert!(matches!(bad, Err(FunctionConstructionError::EmptyName)));

    let bad = Function::new(
        "aethyme",
        "",
        "foo",
        "",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        true,
    );
    assert!(matches!(bad, Err(FunctionConstructionError::EmptyFilePath)));
}

#[test]
fn function_serde_round_trip() {
    let f = Function::new(
        "aethyme",
        "src/cli.py",
        "foo",
        "fn foo()",
        vec![],
        None,
        range(1, 5),
        Visibility::Public,
        true,
    )
    .unwrap();
    let json = serde_json::to_string(&f).unwrap();
    let back: Function = serde_json::from_str(&json).unwrap();
    assert_eq!(back, f);
}

// ─── Method ──────────────────────────────────────────────────────────

#[test]
fn method_construction_carries_receiver_metadata() {
    let receiver = class_id();
    let m = Method::new(
        "aethyme",
        "src/foo.py",
        "do_thing",
        "fn do_thing(self)",
        vec![],
        None,
        range(20, 25),
        Visibility::Public,
        receiver.clone(),
        false,
        true,
    )
    .unwrap();

    assert_eq!(m.name(), "do_thing");
    assert_eq!(m.receiver_type(), &receiver);
    assert!(!m.is_static());
    assert!(m.is_virtual());
    assert_eq!(m.id().kind(), NodeKind::Method);
}

#[test]
fn method_rejects_empty_inputs() {
    let bad = Method::new(
        "aethyme",
        "src/foo.py",
        "",
        "",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        class_id(),
        false,
        false,
    );
    assert!(matches!(bad, Err(MethodConstructionError::EmptyName)));
}

#[test]
fn method_serde_round_trip() {
    let m = Method::new(
        "aethyme",
        "src/foo.py",
        "bar",
        "fn bar(self)",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        class_id(),
        false,
        false,
    )
    .unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: Method = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

// ─── Lambda ──────────────────────────────────────────────────────────

#[test]
fn lambda_with_assigned_name() {
    let l = Lambda::new(
        "aethyme",
        "src/cli.py",
        "handler",
        "() => void",
        vec![],
        None,
        range(30, 30),
        Visibility::Module,
        function_id(),
        Some("handler"),
    )
    .unwrap();
    assert_eq!(l.assigned_name(), Some("handler"));
    assert_eq!(l.id().kind(), NodeKind::Lambda);
}

#[test]
fn lambda_inline_passed_has_no_assigned_name() {
    let l = Lambda::new(
        "aethyme",
        "src/cli.py",
        "<anon@line:42>",
        "x => x.id",
        vec![],
        None,
        range(42, 42),
        Visibility::Module,
        function_id(),
        None,
    )
    .unwrap();
    assert_eq!(l.assigned_name(), None);
}

#[test]
fn lambda_serde_round_trip() {
    let l = Lambda::new(
        "aethyme",
        "src/cli.py",
        "h",
        "x => x",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        function_id(),
        Some("h"),
    )
    .unwrap();
    let json = serde_json::to_string(&l).unwrap();
    let back: Lambda = serde_json::from_str(&json).unwrap();
    assert_eq!(back, l);
}

// ─── ParameterSignature ──────────────────────────────────────────────

#[test]
fn parameter_signature_handles_optional_fields() {
    let p_untyped = ParameterSignature {
        name: "x".into(),
        type_str: None,
        default_value: None,
    };
    let p_typed = ParameterSignature {
        name: "y".into(),
        type_str: Some("int".into()),
        default_value: Some("0".into()),
    };
    let json_a = serde_json::to_string(&p_untyped).unwrap();
    let json_b = serde_json::to_string(&p_typed).unwrap();
    assert_ne!(json_a, json_b);

    let back: ParameterSignature = serde_json::from_str(&json_b).unwrap();
    assert_eq!(back, p_typed);
}

// ─── Cross-kind: Callable trait usable polymorphically ───────────────

#[test]
fn callable_trait_works_polymorphically() {
    let f = Function::new(
        "aethyme",
        "src/cli.py",
        "foo",
        "fn foo()",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        true,
    )
    .unwrap();
    let m = Method::new(
        "aethyme",
        "src/foo.py",
        "bar",
        "fn bar(self)",
        vec![],
        None,
        range(1, 1),
        Visibility::Public,
        class_id(),
        false,
        false,
    )
    .unwrap();
    let callables: Vec<&dyn Callable> = vec![&f, &m];
    assert_eq!(callables.len(), 2);
    let names: Vec<&str> = callables.iter().map(|c| c.name()).collect();
    assert_eq!(names, vec!["foo", "bar"]);
}
