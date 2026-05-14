//! Integration tests for non-code + unresolved node kinds.

use aethyme_graph_schema::{
    ALL_COMMENT_TAGS, Comment, CommentConstructionError, CommentTag,
    ConfigValue, ConfigValueConstructionError, DocSection,
    DocSectionConstructionError, Docstring, DocstringConstructionError,
    NodeId, NodeKind, NonCodeFile, NonCodeFormat, SourceRange,
    UnresolvedSymbol,
};

fn non_code_file_id() -> NodeId {
    NonCodeFile::new("aethyme", "README.md", NonCodeFormat::Markdown)
        .unwrap()
        .id()
        .clone()
}

fn function_id() -> NodeId {
    NodeId::new(NodeKind::Function, "aethyme", "src/x.py", "foo").unwrap()
}

fn statement_id() -> NodeId {
    // Build a Statement and grab its id. Using NodeId::new directly
    // for brevity — the Statement struct's tests live in
    // tests/sub_symbols.rs.
    NodeId::new(NodeKind::Statement, "aethyme", "src/x.py", "fake_stmt")
        .unwrap()
}

fn range() -> SourceRange {
    SourceRange::new(1, 5).unwrap()
}

// ─── DocSection ──────────────────────────────────────────────────────

#[test]
fn doc_section_uses_enclosing_file_in_id() {
    let s = DocSection::new(
        "aethyme",
        "README.md",
        "Setup",
        range(),
        non_code_file_id(),
    )
    .unwrap();
    assert_eq!(s.id().kind(), NodeKind::DocSection);
    assert_eq!(s.heading(), "Setup");

    assert!(matches!(
        DocSection::new("aethyme", "README.md", "", range(), non_code_file_id()),
        Err(DocSectionConstructionError::EmptyHeading)
    ));
}

#[test]
fn doc_section_id_disambiguates_repeated_headings() {
    // Two sections with the same heading at different lines get
    // different IDs because the start_line is folded into the
    // symbol component.
    let s1 = DocSection::new(
        "aethyme",
        "README.md",
        "Setup",
        SourceRange::new(10, 20).unwrap(),
        non_code_file_id(),
    )
    .unwrap();
    let s2 = DocSection::new(
        "aethyme",
        "README.md",
        "Setup",
        SourceRange::new(100, 110).unwrap(),
        non_code_file_id(),
    )
    .unwrap();
    assert_ne!(s1.id(), s2.id());
}

// ─── Docstring ───────────────────────────────────────────────────────

#[test]
fn docstring_id_is_stable_for_target() {
    // Same target → same docstring ID, even if other fields differ.
    let target = function_id();
    let d1 = Docstring::new(
        "aethyme",
        "src/x.py",
        target.clone(),
        range(),
        "hashA",
    )
    .unwrap();
    let d2 = Docstring::new(
        "aethyme",
        "src/x.py",
        target.clone(),
        SourceRange::new(50, 60).unwrap(),
        "hashB",
    )
    .unwrap();
    assert_eq!(d1.id(), d2.id());
}

#[test]
fn docstring_rejects_empty_text_hash() {
    let err = Docstring::new(
        "aethyme",
        "src/x.py",
        function_id(),
        range(),
        "",
    )
    .unwrap_err();
    assert!(matches!(err, DocstringConstructionError::EmptyTextHash));
}

// ─── Comment ─────────────────────────────────────────────────────────

#[test]
fn all_comment_tags_match_canonical_names() {
    let expectations = [
        (CommentTag::Todo, "todo"),
        (CommentTag::Fixme, "fixme"),
        (CommentTag::Note, "note"),
        (CommentTag::Decision, "decision"),
        (CommentTag::SeeAlso, "see_also"),
    ];
    assert_eq!(expectations.len(), ALL_COMMENT_TAGS.len());
    for (tag, name) in expectations {
        assert_eq!(tag.name(), name);
    }
}

#[test]
fn comment_carries_optional_statement_target() {
    let c_with = Comment::new(
        "aethyme",
        "src/x.py",
        42,
        CommentTag::Todo,
        "abc",
        Some(statement_id()),
    )
    .unwrap();
    assert!(c_with.target_statement_id().is_some());

    let c_without = Comment::new(
        "aethyme",
        "src/x.py",
        100,
        CommentTag::Decision,
        "def",
        None,
    )
    .unwrap();
    assert!(c_without.target_statement_id().is_none());
}

#[test]
fn comment_rejects_zero_start_line() {
    let err = Comment::new(
        "aethyme",
        "src/x.py",
        0,
        CommentTag::Todo,
        "abc",
        None,
    )
    .unwrap_err();
    assert!(matches!(err, CommentConstructionError::StartLineZero));
}

// ─── ConfigValue ─────────────────────────────────────────────────────

#[test]
fn config_value_uses_dotted_path_in_id() {
    let v = ConfigValue::new(
        "aethyme",
        "config.toml",
        "database.url",
        "hash123",
        range(),
        non_code_file_id(),
    )
    .unwrap();
    assert_eq!(v.id().kind(), NodeKind::ConfigValue);
    assert_eq!(v.config_path(), "database.url");

    assert!(matches!(
        ConfigValue::new(
            "aethyme",
            "config.toml",
            "",
            "h",
            range(),
            non_code_file_id()
        ),
        Err(ConfigValueConstructionError::EmptyConfigPath)
    ));
}

#[test]
fn config_value_distinct_paths_distinct_ids() {
    let a = ConfigValue::new(
        "aethyme",
        "config.toml",
        "database.url",
        "h",
        range(),
        non_code_file_id(),
    )
    .unwrap();
    let b = ConfigValue::new(
        "aethyme",
        "config.toml",
        "cache.ttl",
        "h",
        range(),
        non_code_file_id(),
    )
    .unwrap();
    assert_ne!(a.id(), b.id());
}

// ─── UnresolvedSymbol ────────────────────────────────────────────────

#[test]
fn unresolved_symbol_disambiguates_by_callsite() {
    // Same unresolved name, different call sites → different IDs.
    let site_a = function_id();
    let site_b =
        NodeId::new(NodeKind::Function, "aethyme", "src/x.py", "bar").unwrap();
    let u_a = UnresolvedSymbol::new(
        "aethyme",
        "src/x.py",
        "print",
        Some(NodeKind::Function),
        site_a,
    )
    .unwrap();
    let u_b = UnresolvedSymbol::new(
        "aethyme",
        "src/x.py",
        "print",
        Some(NodeKind::Function),
        site_b,
    )
    .unwrap();
    assert_ne!(u_a.id(), u_b.id());
    assert_eq!(u_a.name(), "print");
    assert_eq!(u_a.expected_kind(), Some(NodeKind::Function));
}

#[test]
fn unresolved_symbol_accepts_none_expected_kind() {
    let u = UnresolvedSymbol::new(
        "aethyme",
        "src/x.py",
        "magic_thing",
        None,
        function_id(),
    )
    .unwrap();
    assert_eq!(u.expected_kind(), None);
}

// ─── Serde round-trips ───────────────────────────────────────────────

#[test]
fn non_code_kinds_round_trip() {
    let s = DocSection::new(
        "aethyme",
        "README.md",
        "Setup",
        range(),
        non_code_file_id(),
    )
    .unwrap();
    let json = serde_json::to_string(&s).unwrap();
    let back: DocSection = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);

    let c = Comment::new(
        "aethyme",
        "src/x.py",
        42,
        CommentTag::Todo,
        "abc",
        None,
    )
    .unwrap();
    let json = serde_json::to_string(&c).unwrap();
    let back: Comment = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
}
