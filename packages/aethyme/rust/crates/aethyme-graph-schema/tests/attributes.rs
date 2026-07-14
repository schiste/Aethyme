//! Integration tests for the common attribute types: `Confidence`,
//! `Source`, `BindingKind`.

use aethyme_graph_schema::{ALL_BINDING_KINDS, ALL_SOURCES, BindingKind, Confidence, Source};

// ─── Confidence ──────────────────────────────────────────────────────

#[test]
fn confidence_zero_and_full_constants_are_at_endpoints() {
    assert_eq!(Confidence::ZERO.as_milli(), 0);
    assert_eq!(Confidence::FULL.as_milli(), 1000);
}

#[test]
fn confidence_from_milli_accepts_valid_range() {
    for v in [0_u16, 1, 500, 999, 1000] {
        let c = Confidence::from_milli(v).unwrap();
        assert_eq!(c.as_milli(), v);
    }
}

#[test]
fn confidence_from_milli_rejects_out_of_range() {
    for v in [1001_u16, 5000, 65535] {
        let err = Confidence::from_milli(v).unwrap_err();
        assert_eq!(err.given(), v);
    }
}

#[test]
fn confidence_display_shows_fraction() {
    let c = Confidence::from_milli(750).unwrap();
    assert_eq!(format!("{c}"), "750/1000");
}

#[test]
fn confidence_serializes_as_bare_integer() {
    // #[serde(transparent)] over u16 — important: NDJSON shards
    // expect a JSON number here, not a wrapping object.
    let c = Confidence::from_milli(750).unwrap();
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(json, "750");
}

#[test]
fn confidence_deserialize_validates_range() {
    // Manual Deserialize impl routes through from_milli — confirm a
    // numerically valid JSON that's out-of-range gets rejected at
    // deserialization, not silently constructed.
    let bad = "5000";
    let result: Result<Confidence, _> = serde_json::from_str(bad);
    assert!(
        result.is_err(),
        "deserialize must reject out-of-range values, but accepted {bad:?}"
    );
}

#[test]
fn confidence_deserialize_accepts_valid_range() {
    let json = "750";
    let c: Confidence = serde_json::from_str(json).unwrap();
    assert_eq!(c.as_milli(), 750);
}

#[test]
fn confidence_ord_matches_milli_ord() {
    let a = Confidence::from_milli(100).unwrap();
    let b = Confidence::from_milli(200).unwrap();
    let c = Confidence::from_milli(200).unwrap();
    assert!(a < b);
    assert!(b == c);
}

// ─── Source ──────────────────────────────────────────────────────────

#[test]
fn source_name_and_from_name_round_trip() {
    for &s in ALL_SOURCES {
        let name = s.name();
        let back = Source::from_name(name).unwrap();
        assert_eq!(back, s);
    }
}

#[test]
fn source_from_name_rejects_unknown() {
    let err = Source::from_name("magic").unwrap_err();
    assert_eq!(err.given(), "magic");
    assert!(Source::from_name("").is_err());
    assert!(Source::from_name("STRUCTURE").is_err());
}

#[test]
fn source_canonical_names_are_snake_case() {
    for &s in ALL_SOURCES {
        let name = s.name();
        assert!(!name.is_empty());
        for c in name.chars() {
            assert!(c.is_ascii_lowercase() || c == '_');
        }
    }
}

#[test]
fn source_serde_round_trip() {
    for &s in ALL_SOURCES {
        let json = serde_json::to_string(&s).unwrap();
        let back: Source = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}

#[test]
fn source_json_matches_canonical_name() {
    for &s in ALL_SOURCES {
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json.trim_matches('"'), s.name());
    }
}

#[test]
fn all_sources_is_exhaustive() {
    const EXPECTED: usize = 3;
    assert_eq!(ALL_SOURCES.len(), EXPECTED);
}

// ─── BindingKind ─────────────────────────────────────────────────────

#[test]
fn binding_kind_name_and_from_name_round_trip() {
    for &b in ALL_BINDING_KINDS {
        let name = b.name();
        let back = BindingKind::from_name(name).unwrap();
        assert_eq!(back, b);
    }
}

#[test]
fn binding_kind_from_name_rejects_unknown() {
    let err = BindingKind::from_name("magic").unwrap_err();
    assert_eq!(err.given(), "magic");
}

#[test]
fn binding_kind_canonical_names_are_snake_case() {
    for &b in ALL_BINDING_KINDS {
        let name = b.name();
        assert!(!name.is_empty());
        for c in name.chars() {
            assert!(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        }
    }
}

#[test]
fn binding_kind_serde_round_trip() {
    for &b in ALL_BINDING_KINDS {
        let json = serde_json::to_string(&b).unwrap();
        let back: BindingKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }
}

#[test]
fn binding_kind_json_matches_canonical_name() {
    for &b in ALL_BINDING_KINDS {
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json.trim_matches('"'), b.name());
    }
}

#[test]
fn all_binding_kinds_is_exhaustive() {
    const EXPECTED: usize = 5;
    assert_eq!(ALL_BINDING_KINDS.len(), EXPECTED);
}

#[test]
fn binding_kind_wasm_bindgen_uses_underscore() {
    // Canonical name "wasm_bindgen" rather than "wasm-bindgen" so the
    // snake_case discipline holds across the schema. The Rust crate
    // is called wasm-bindgen with a hyphen; we deliberately diverge.
    assert_eq!(BindingKind::WasmBindgen.name(), "wasm_bindgen");
}

// ─── Cross-attribute invariants ──────────────────────────────────────

#[test]
fn source_and_binding_kind_canonical_names_are_disjoint() {
    use std::collections::BTreeSet;
    let source_names: BTreeSet<&'static str> = ALL_SOURCES.iter().map(|s| s.name()).collect();
    for &b in ALL_BINDING_KINDS {
        assert!(
            !source_names.contains(b.name()),
            "BindingKind {b:?} collides with a Source name"
        );
    }
}
