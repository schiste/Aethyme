//! Integration tests for sub-symbol node kinds.

use aethyme_graph_schema::{
    Expression, Field, FieldConstructionError, GlobalVariable, NodeId, NodeKind, Parameter,
    SourceRange, Statement, Visibility,
};

fn class_id() -> NodeId {
    NodeId::new(NodeKind::Class, "aethyme", "src/x.py", "Foo").unwrap()
}

fn function_id() -> NodeId {
    NodeId::new(NodeKind::Function, "aethyme", "src/x.py", "do_thing").unwrap()
}

fn range() -> SourceRange {
    SourceRange::new(1, 5).unwrap()
}

#[test]
fn field_construction_carries_enclosing_type() {
    let f = Field::new(
        "aethyme",
        "src/x.py",
        "counter",
        "int",
        class_id(),
        Visibility::Private,
    )
    .unwrap();
    assert_eq!(f.id().kind(), NodeKind::Field);
    assert_eq!(f.name(), "counter");
    assert_eq!(f.type_str(), "int");
    assert_eq!(f.enclosing_type_id(), &class_id());

    assert!(matches!(
        Field::new(
            "aethyme",
            "src/x.py",
            "",
            "int",
            class_id(),
            Visibility::Private
        ),
        Err(FieldConstructionError::EmptyName)
    ));
    assert!(matches!(
        Field::new(
            "aethyme",
            "src/x.py",
            "n",
            "",
            class_id(),
            Visibility::Private
        ),
        Err(FieldConstructionError::EmptyType)
    ));
}

#[test]
fn global_variable_accepts_optional_type() {
    let with_type =
        GlobalVariable::new("aethyme", "src/x.py", "VERSION", Some("str"), range()).unwrap();
    assert_eq!(with_type.type_str(), Some("str"));

    let without = GlobalVariable::new("aethyme", "src/x.py", "INSTANCE", None, range()).unwrap();
    assert_eq!(without.type_str(), None);
}

#[test]
fn parameter_id_includes_enclosing_callable_disambiguation() {
    // Two parameters with the same name in different callables of
    // the same file must produce different NodeIds.
    let fn_a = NodeId::new(NodeKind::Function, "r", "f.py", "alpha").unwrap();
    let fn_b = NodeId::new(NodeKind::Function, "r", "f.py", "beta").unwrap();
    let p_a = Parameter::new("r", "f.py", "x", None, 0, fn_a).unwrap();
    let p_b = Parameter::new("r", "f.py", "x", None, 0, fn_b).unwrap();
    assert_ne!(p_a.id(), p_b.id());
    assert_eq!(p_a.position(), 0);
}

#[test]
fn statement_id_includes_position_and_kind_tag() {
    // Two statements in the same callable with the same kind_tag
    // but different positions must produce different NodeIds.
    let f = function_id();
    let s1 = Statement::new("aethyme", "src/x.py", "assign", range(), f.clone(), 0).unwrap();
    let s2 = Statement::new("aethyme", "src/x.py", "assign", range(), f.clone(), 1).unwrap();
    assert_ne!(s1.id(), s2.id());
    assert_eq!(s1.kind_tag(), "assign");
    assert_eq!(s1.position_in_body(), 0);
}

#[test]
fn expression_id_includes_position_and_kind_tag() {
    let f = function_id();
    let s = Statement::new("aethyme", "src/x.py", "if", range(), f, 0).unwrap();
    let e1 = Expression::new("aethyme", "src/x.py", "call", range(), s.id().clone(), 0).unwrap();
    let e2 = Expression::new("aethyme", "src/x.py", "call", range(), s.id().clone(), 1).unwrap();
    assert_ne!(e1.id(), e2.id());
    assert_eq!(e1.kind_tag(), "call");
}

#[test]
fn sub_symbol_kinds_round_trip_through_serde() {
    let f = Field::new(
        "aethyme",
        "src/x.py",
        "x",
        "int",
        class_id(),
        Visibility::Private,
    )
    .unwrap();
    let json = serde_json::to_string(&f).unwrap();
    let back: Field = serde_json::from_str(&json).unwrap();
    assert_eq!(back, f);

    let g = GlobalVariable::new("aethyme", "src/x.py", "GLOBAL", None, range()).unwrap();
    let json = serde_json::to_string(&g).unwrap();
    let back: GlobalVariable = serde_json::from_str(&json).unwrap();
    assert_eq!(back, g);
}
