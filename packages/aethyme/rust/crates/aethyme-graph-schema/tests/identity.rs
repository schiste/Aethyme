//! Integration tests for [`NodeId`].
//!
//! Covers four concern groups:
//!
//! 1. **Determinism**: same inputs → same NodeId, every time, on every
//!    machine. This is the foundation for Option C's per-file fragment
//!    merge story (different contributors must produce byte-identical
//!    fragments).
//! 2. **Sensitivity**: each input dimension genuinely participates in
//!    the hash. Changing any one of (kind, repo, file_path,
//!    symbol_name) produces a different NodeId.
//! 3. **Format**: the canonical string form is well-formed, the
//!    component accessors return the right slices, and parse↔new
//!    round-trip.
//! 4. **Validation**: construction and parse both reject malformed
//!    inputs with informative errors.

use aethyme_graph_schema::{NodeId, NodeIdConstructionError, NodeIdParseError, NodeKind};

// ─── Determinism ─────────────────────────────────────────────────────

#[test]
fn same_inputs_produce_same_node_id() {
    let a = NodeId::new(
        NodeKind::Function,
        "aethyme",
        "src/cli.py",
        "explore_command",
    )
    .unwrap();
    let b = NodeId::new(
        NodeKind::Function,
        "aethyme",
        "src/cli.py",
        "explore_command",
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.as_str(), b.as_str());
}

#[test]
fn id_is_stable_across_repeated_construction() {
    // Hammer the constructor with the same inputs many times. A
    // non-deterministic hashing path (e.g., a hash function with
    // process-randomized seed, hello SipHash) would surface here.
    let first = NodeId::new(
        NodeKind::Class,
        "aethyme",
        "packages/foo/Bar.php",
        "BarClass",
    )
    .unwrap();
    for _ in 0..32 {
        let again = NodeId::new(
            NodeKind::Class,
            "aethyme",
            "packages/foo/Bar.php",
            "BarClass",
        )
        .unwrap();
        assert_eq!(first, again);
    }
}

// ─── Sensitivity: each input participates in the hash ────────────────

fn ref_id() -> NodeId {
    NodeId::new(NodeKind::Function, "aethyme", "src/cli.py", "foo").unwrap()
}

#[test]
fn different_kind_produces_different_id() {
    let other = NodeId::new(NodeKind::Method, "aethyme", "src/cli.py", "foo").unwrap();
    assert_ne!(ref_id(), other);
}

#[test]
fn different_repo_produces_different_id() {
    let other = NodeId::new(NodeKind::Function, "playground", "src/cli.py", "foo").unwrap();
    assert_ne!(ref_id(), other);
}

#[test]
fn different_file_path_produces_different_id() {
    let other = NodeId::new(NodeKind::Function, "aethyme", "src/other.py", "foo").unwrap();
    assert_ne!(ref_id(), other);
}

#[test]
fn different_symbol_name_produces_different_id() {
    let other = NodeId::new(NodeKind::Function, "aethyme", "src/cli.py", "bar").unwrap();
    assert_ne!(ref_id(), other);
}

#[test]
fn concatenation_attacks_are_distinguished_by_length_prefix() {
    // Without length-prefixing, ("foo", "bar") and ("foob", "ar") would
    // hash identically (both produce the byte stream "foobar"). The
    // length-prefix step in compute_hash defends against this. Confirm
    // it works by constructing two NodeIds whose unprefixed
    // concatenation would collide.
    let a = NodeId::new(NodeKind::Function, "foo", "bar", "name").unwrap();
    let b = NodeId::new(NodeKind::Function, "foob", "ar", "name").unwrap();
    assert_ne!(a, b);
}

// ─── Format: canonical shape and component accessors ─────────────────

#[test]
fn canonical_form_has_three_colon_separated_components() {
    let id = ref_id();
    let parts: Vec<&str> = id.as_str().splitn(3, ':').collect();
    assert_eq!(
        parts.len(),
        3,
        "NodeId canonical form must have exactly 3 colon-separated \
         components; got {parts:?} for id {id:?}"
    );
}

#[test]
fn hash_suffix_is_26_chars_of_lowercase_base32() {
    let id = ref_id();
    let suffix = id.hash_suffix();
    assert_eq!(suffix.len(), 26);
    for c in suffix.chars() {
        assert!(
            matches!(c, 'a'..='z' | '2'..='7'),
            "hash suffix has non-base32-lowercase char {c:?} in {suffix:?}"
        );
    }
}

#[test]
fn kind_accessor_returns_constructed_kind() {
    for &kind in aethyme_graph_schema::ALL_NODE_KINDS {
        let id = NodeId::new(kind, "aethyme", "src/x.py", "y").unwrap();
        assert_eq!(
            id.kind(),
            kind,
            "kind accessor drifted from constructed kind for {kind:?}"
        );
    }
}

#[test]
fn repo_accessor_returns_constructed_repo() {
    let id = NodeId::new(NodeKind::Function, "playground-mediawiki", "src/x.py", "y").unwrap();
    assert_eq!(id.repo(), "playground-mediawiki");
}

#[test]
fn display_impl_matches_as_str() {
    let id = ref_id();
    assert_eq!(format!("{id}"), id.as_str());
}

#[test]
fn parse_round_trips_with_new() {
    let id = ref_id();
    let parsed = NodeId::parse(id.as_str()).unwrap();
    assert_eq!(parsed, id);
    assert_eq!(parsed.as_str(), id.as_str());
}

#[test]
fn parse_extracts_correct_kind_and_repo() {
    let id = NodeId::new(NodeKind::Class, "aethyme", "src/x.rs", "Foo").unwrap();
    let parsed = NodeId::parse(id.as_str()).unwrap();
    assert_eq!(parsed.kind(), NodeKind::Class);
    assert_eq!(parsed.repo(), "aethyme");
}

// ─── Validation: construction errors ─────────────────────────────────

#[test]
fn new_rejects_empty_repo() {
    let err = NodeId::new(NodeKind::Function, "", "src/x.py", "y").unwrap_err();
    assert!(
        matches!(err, NodeIdConstructionError::EmptyRepo),
        "expected EmptyRepo, got {err:?}"
    );
}

#[test]
fn new_rejects_repo_containing_colon() {
    let err = NodeId::new(NodeKind::Function, "bad:repo", "src/x.py", "y").unwrap_err();
    match err {
        NodeIdConstructionError::RepoContainsColon { given } => {
            assert_eq!(&*given, "bad:repo");
        }
        other => panic!("expected RepoContainsColon, got {other:?}"),
    }
}

#[test]
fn new_accepts_empty_file_path_and_symbol_name() {
    // Per the API design, only `repo` is validated at this layer.
    // Per-kind semantic validation (e.g., "File must have non-empty
    // file_path") lives in the per-kind struct constructors landing
    // in commits 1.6-1.11. This test pins the current behavior so
    // those struct-level constraints don't accidentally leak into
    // the NodeId layer.
    let id = NodeId::new(NodeKind::Repository, "aethyme", "", "").unwrap();
    assert!(id.as_str().starts_with("repository:aethyme:"));
}

// ─── Validation: parse errors ────────────────────────────────────────

#[test]
fn parse_rejects_wrong_component_count() {
    let bads = [
        "",
        "function",
        "function:aethyme",
        "function:aethyme:abc:extra",
    ];
    for bad in bads {
        let err = NodeId::parse(bad).unwrap_err();
        match err {
            NodeIdParseError::WrongComponentCount { .. }
            | NodeIdParseError::EmptyKindComponent { .. }
            | NodeIdParseError::EmptyRepoComponent { .. }
            | NodeIdParseError::WrongHashLength { .. } => {}
            other => panic!(
                "expected component-count or component-empty error for \
                 {bad:?}, got {other:?}"
            ),
        }
    }
}

#[test]
fn parse_rejects_unknown_kind() {
    // Build a syntactically valid string but with a non-NodeKind first
    // component.
    let valid = NodeId::new(NodeKind::Function, "aethyme", "x.py", "y").unwrap();
    let suffix = valid.hash_suffix();
    let bad = format!("zorblat:aethyme:{suffix}");
    let err = NodeId::parse(&bad).unwrap_err();
    match err {
        NodeIdParseError::UnknownKind { kind_str, .. } => {
            assert_eq!(&*kind_str, "zorblat");
        }
        other => panic!("expected UnknownKind, got {other:?}"),
    }
}

#[test]
fn parse_rejects_wrong_hash_length() {
    let bad_short = "function:aethyme:abc";
    let err = NodeId::parse(bad_short).unwrap_err();
    match err {
        NodeIdParseError::WrongHashLength { hash_len, .. } => {
            assert_eq!(hash_len, 3);
        }
        other => panic!("expected WrongHashLength, got {other:?}"),
    }

    let bad_long = "function:aethyme:abcdefghijklmnopqrstuvwxyz23456789";
    let err = NodeId::parse(bad_long).unwrap_err();
    assert!(matches!(err, NodeIdParseError::WrongHashLength { .. }));
}

#[test]
fn parse_rejects_empty_kind_component() {
    // The `WrongComponentCount` and `EmptyKindComponent` errors are
    // close cousins; this test pins the exact input shape that
    // triggers `EmptyKindComponent` (3 components with the first
    // one empty: leading colon).
    let valid = NodeId::new(NodeKind::Function, "aethyme", "x.py", "y").unwrap();
    let suffix = valid.hash_suffix();
    let bad = format!(":aethyme:{suffix}");
    let err = NodeId::parse(&bad).unwrap_err();
    assert!(
        matches!(err, NodeIdParseError::EmptyKindComponent { .. }),
        "expected EmptyKindComponent for {bad:?}, got {err:?}"
    );
}

#[test]
fn parse_rejects_empty_repo_component() {
    // Parallel to the empty-kind case: 3 components, middle empty
    // (adjacent colons).
    let valid = NodeId::new(NodeKind::Function, "aethyme", "x.py", "y").unwrap();
    let suffix = valid.hash_suffix();
    let bad = format!("function::{suffix}");
    let err = NodeId::parse(&bad).unwrap_err();
    assert!(
        matches!(err, NodeIdParseError::EmptyRepoComponent { .. }),
        "expected EmptyRepoComponent for {bad:?}, got {err:?}"
    );
}

#[test]
fn parse_rejects_invalid_hash_chars() {
    // Use uppercase (rejected because we lowercase base32), and the
    // digit '1' (rejected because base32 alphabet uses 2-7 only).
    let with_uppercase = "function:aethyme:Abcdefghijklmnopqrstuvwxyz";
    let err = NodeId::parse(with_uppercase).unwrap_err();
    assert!(matches!(err, NodeIdParseError::InvalidHashChar { .. }));

    let with_one = "function:aethyme:1bcdefghijklmnopqrstuvwxyz";
    let err = NodeId::parse(with_one).unwrap_err();
    assert!(matches!(err, NodeIdParseError::InvalidHashChar { .. }));
}

// ─── Cross-process determinism: pinned snapshot ──────────────────────

#[test]
fn snapshot_known_good_node_id() {
    // This pinned-output snapshot is THE regression guard for the
    // entire hashing path: length-prefix format, input order, BLAKE3
    // truncation, base32 encoding, lowercase step. Any unintended
    // change anywhere in the pipeline produces a different string
    // here and FAILS LOUDLY — which is the only way silent
    // hash-format drift gets caught before it ships to user fragments.
    //
    // If you intentionally changed the hashing path (e.g. for a
    // performance optimization or to fix a bug), update the expected
    // value AND treat the change as a forever-format break: every
    // existing NodeId on disk becomes invalid, every fragment needs
    // regeneration, every consumer that cached an ID is now wrong.
    // This is rarely the right call.
    //
    // Inputs: NodeKind::Function, "aethyme", "src/cli.py",
    // "explore_command". Computed at commit 1.4, 2026-05-14.
    let id = NodeId::new(
        NodeKind::Function,
        "aethyme",
        "src/cli.py",
        "explore_command",
    )
    .unwrap();
    assert_eq!(
        id.as_str(),
        "function:aethyme:ezra4adyfpyyfpec2v35xuwieu",
        "NodeId snapshot drifted — see test comment for what this \
         means before updating the expected value"
    );
}

// ─── Adversarial inputs ──────────────────────────────────────────────

#[test]
fn nul_byte_in_path_or_name_produces_valid_id() {
    // NUL bytes are legal UTF-8 codepoints and may appear in
    // pathological filename or symbol inputs. The hashing path
    // takes raw bytes via length-prefixing, so NUL is just another
    // byte. Confirm it doesn't crash, produces a valid ID, and that
    // NUL placement matters (i.e., length-prefixing prevents
    // NUL-as-separator-collision).
    let a = NodeId::new(NodeKind::Function, "aethyme", "src/cli\0.py", "foo").unwrap();
    let b = NodeId::new(NodeKind::Function, "aethyme", "src/cli", "\0.py:foo").unwrap();
    // Both should parse and be different IDs.
    assert!(NodeId::parse(a.as_str()).is_ok());
    assert!(NodeId::parse(b.as_str()).is_ok());
    assert_ne!(a, b);
}

#[test]
fn deserialize_rejects_malformed_strings() {
    // The manual Deserialize impl routes through parse — confirm a
    // structurally invalid JSON string does NOT produce a NodeId that
    // would panic on later kind() calls.
    let bad = r#""this-is-not-a-valid-node-id""#;
    let result: Result<NodeId, _> = serde_json::from_str(bad);
    assert!(
        result.is_err(),
        "deserialize must reject malformed strings, but accepted {bad:?}"
    );
}

#[test]
fn deserialize_rejects_unknown_kind_string() {
    // A string with the right shape but an unknown kind component
    // must be rejected at deserialization, not silently constructed.
    let valid = NodeId::new(NodeKind::Function, "aethyme", "x.py", "y").unwrap();
    let suffix = valid.hash_suffix();
    let bad_json = format!(r#""zorblat:aethyme:{suffix}""#);
    let result: Result<NodeId, _> = serde_json::from_str(&bad_json);
    assert!(result.is_err());
}

// ─── Serde round-trip ────────────────────────────────────────────────

#[test]
fn serde_serializes_as_canonical_string() {
    // #[serde(transparent)] means a NodeId serializes as the inner
    // Box<str> — i.e., as a JSON string of the canonical form. This
    // is the wire shape that NDJSON index shards rely on.
    let id = ref_id();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, format!("\"{}\"", id.as_str()));
}

#[test]
fn serde_round_trip_is_identity() {
    let id = ref_id();
    let json = serde_json::to_string(&id).unwrap();
    let back: NodeId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
}
