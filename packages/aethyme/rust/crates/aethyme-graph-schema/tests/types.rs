//! Integration tests for type-defining node kinds.

use aethyme_graph_schema::{
    Class, Enum, EnumConstructionError, EnumVariant, Interface, NodeKind, SourceRange, Struct,
    Trait, TypeAlias, TypeAliasConstructionError, Visibility,
};

fn range() -> SourceRange {
    SourceRange::new(1, 10).unwrap()
}

#[test]
fn class_struct_interface_trait_share_construction_shape() {
    // Spot-check each of the four "basic" type-defining kinds.
    let c = Class::new("aethyme", "src/x.py", "Foo", range(), Visibility::Public).unwrap();
    assert_eq!(c.id().kind(), NodeKind::Class);

    let s = Struct::new("aethyme", "src/x.rs", "Bar", range(), Visibility::Public).unwrap();
    assert_eq!(s.id().kind(), NodeKind::Struct);

    let i = Interface::new("aethyme", "src/x.ts", "Baz", range(), Visibility::Public).unwrap();
    assert_eq!(i.id().kind(), NodeKind::Interface);

    let t = Trait::new("aethyme", "src/x.rs", "Qux", range(), Visibility::Public).unwrap();
    assert_eq!(t.id().kind(), NodeKind::Trait);
}

#[test]
fn class_rejects_empty_inputs() {
    let bad = Class::new("aethyme", "x.py", "", range(), Visibility::Public);
    assert!(bad.is_err());
    let bad = Class::new("aethyme", "", "Foo", range(), Visibility::Public);
    assert!(bad.is_err());
}

#[test]
fn basic_kinds_round_trip_through_serde() {
    let c = Class::new("aethyme", "src/x.py", "Foo", range(), Visibility::Public).unwrap();
    let json = serde_json::to_string(&c).unwrap();
    let back: Class = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
}

#[test]
fn enum_carries_inline_variants() {
    let variants = vec![
        EnumVariant {
            name: "A".into(),
            payload: None,
        },
        EnumVariant {
            name: "B".into(),
            payload: Some("u32".into()),
        },
    ];
    let e = Enum::new(
        "aethyme",
        "src/x.rs",
        "Color",
        variants.clone(),
        range(),
        Visibility::Public,
    )
    .unwrap();

    assert_eq!(e.variants(), &variants[..]);
    assert_eq!(e.id().kind(), NodeKind::Enum);

    let bad = Enum::new(
        "aethyme",
        "src/x.rs",
        "",
        vec![],
        range(),
        Visibility::Public,
    );
    assert!(matches!(bad, Err(EnumConstructionError::EmptyName)));
}

#[test]
fn enum_serde_round_trip() {
    let e = Enum::new(
        "aethyme",
        "src/x.rs",
        "Color",
        vec![EnumVariant {
            name: "Red".into(),
            payload: None,
        }],
        range(),
        Visibility::Public,
    )
    .unwrap();
    let json = serde_json::to_string(&e).unwrap();
    let back: Enum = serde_json::from_str(&json).unwrap();
    assert_eq!(back, e);
}

#[test]
fn type_alias_requires_target_type() {
    let bad = TypeAlias::new(
        "aethyme",
        "src/x.rs",
        "Foo",
        "",
        range(),
        Visibility::Public,
    );
    assert!(matches!(
        bad,
        Err(TypeAliasConstructionError::EmptyTargetType)
    ));

    let t = TypeAlias::new(
        "aethyme",
        "src/x.rs",
        "MyInt",
        "i64",
        range(),
        Visibility::Public,
    )
    .unwrap();
    assert_eq!(t.target_type(), "i64");
    assert_eq!(t.id().kind(), NodeKind::TypeAlias);
}

#[test]
fn type_alias_serde_round_trip() {
    let t = TypeAlias::new(
        "aethyme",
        "src/x.rs",
        "MyInt",
        "i64",
        range(),
        Visibility::Public,
    )
    .unwrap();
    let json = serde_json::to_string(&t).unwrap();
    let back: TypeAlias = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t);
}
