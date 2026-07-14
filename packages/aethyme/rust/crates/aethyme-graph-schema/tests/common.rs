//! Integration tests for the shared building blocks:
//! `SourceRange`, `Visibility`, `Modification`, `ModificationHistory`.

use aethyme_graph_schema::{
    ALL_VISIBILITIES, InvalidSourceRange, Modification, ModificationHistory, SourceRange,
    Visibility,
};

// ─── SourceRange ─────────────────────────────────────────────────────

#[test]
fn source_range_construction_accepts_valid_inputs() {
    let r = SourceRange::new(1, 1).unwrap();
    assert_eq!(r.start_line(), 1);
    assert_eq!(r.end_line(), 1);
    assert_eq!(r.line_count(), 1);

    let r = SourceRange::new(10, 20).unwrap();
    assert_eq!(r.line_count(), 11);
}

#[test]
fn source_range_rejects_zero_start_line() {
    let err = SourceRange::new(0, 5).unwrap_err();
    assert_eq!(err, InvalidSourceRange::StartLineZero);
}

#[test]
fn source_range_rejects_end_before_start() {
    let err = SourceRange::new(10, 5).unwrap_err();
    assert!(matches!(
        err,
        InvalidSourceRange::EndBeforeStart {
            start_line: 10,
            end_line: 5
        }
    ));
}

#[test]
fn source_range_serde_round_trip() {
    let r = SourceRange::new(42, 99).unwrap();
    let json = serde_json::to_string(&r).unwrap();
    let back: SourceRange = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn source_range_deserialize_validates() {
    // Invalid range in JSON form should fail at deserialization,
    // mirroring the NodeId / Confidence pattern.
    let bad = r#"{"start_line": 0, "end_line": 5}"#;
    let result: Result<SourceRange, _> = serde_json::from_str(bad);
    assert!(result.is_err());

    let bad2 = r#"{"start_line": 10, "end_line": 5}"#;
    let result: Result<SourceRange, _> = serde_json::from_str(bad2);
    assert!(result.is_err());
}

#[test]
fn source_range_ord_lexicographic_by_start_then_end() {
    let a = SourceRange::new(1, 10).unwrap();
    let b = SourceRange::new(1, 20).unwrap();
    let c = SourceRange::new(5, 6).unwrap();
    assert!(a < b);
    assert!(b < c);
    assert!(a < c);
}

// ─── Visibility ──────────────────────────────────────────────────────

#[test]
fn visibility_name_and_from_name_round_trip() {
    for &v in ALL_VISIBILITIES {
        let name = v.name();
        let back = Visibility::from_name(name).unwrap();
        assert_eq!(back, v);
    }
}

#[test]
fn visibility_from_name_rejects_unknown() {
    assert!(Visibility::from_name("").is_err());
    assert!(Visibility::from_name("Public").is_err()); // case-sensitive
    assert!(Visibility::from_name("internal").is_err()); // not in canonical set
}

#[test]
fn visibility_serde_round_trip() {
    for &v in ALL_VISIBILITIES {
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json.trim_matches('"'), v.name());
        let back: Visibility = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }
}

#[test]
fn all_visibilities_is_exhaustive() {
    assert_eq!(ALL_VISIBILITIES.len(), 4);
}

// ─── Modification + ModificationHistory ──────────────────────────────

fn modif(commit: &str, ts: u64) -> Modification {
    Modification {
        commit: commit.into(),
        unix_timestamp: ts,
        author: "alice@example.com".into(),
    }
}

#[test]
fn modification_history_starts_empty() {
    let h = ModificationHistory::new();
    assert_eq!(h.len(), 0);
    assert!(h.is_empty());
    assert!(h.most_recent().is_none());
    assert_eq!(h.entries(), &[] as &[Modification]);
}

#[test]
fn modification_history_push_most_recent_orders_correctly() {
    let mut h = ModificationHistory::new();
    h.push_most_recent(modif("aaa", 100));
    h.push_most_recent(modif("bbb", 200));
    h.push_most_recent(modif("ccc", 300));
    assert_eq!(h.len(), 3);
    // Most recent first
    assert_eq!(&*h.entries()[0].commit, "ccc");
    assert_eq!(&*h.entries()[1].commit, "bbb");
    assert_eq!(&*h.entries()[2].commit, "aaa");
}

#[test]
fn modification_history_evicts_oldest_at_capacity() {
    let mut h = ModificationHistory::new();
    for (sha, ts) in [("aaa", 100), ("bbb", 200), ("ccc", 300), ("ddd", 400)] {
        h.push_most_recent(modif(sha, ts));
    }
    assert_eq!(h.len(), 3); // Capped at 3
    // ddd is most recent; aaa was evicted
    let commits: Vec<&str> = h.entries().iter().map(|m| &*m.commit).collect();
    assert_eq!(commits, vec!["ddd", "ccc", "bbb"]);
}

#[test]
fn modification_history_from_most_recent_first_truncates() {
    let entries = vec![
        modif("aaa", 100),
        modif("bbb", 200),
        modif("ccc", 300),
        modif("ddd", 400),
    ];
    let h = ModificationHistory::from_most_recent_first(entries);
    assert_eq!(h.len(), 3);
    let commits: Vec<&str> = h.entries().iter().map(|m| &*m.commit).collect();
    assert_eq!(commits, vec!["aaa", "bbb", "ccc"]);
}

#[test]
fn modification_history_serde_round_trip() {
    let mut h = ModificationHistory::new();
    h.push_most_recent(modif("aaa", 100));
    h.push_most_recent(modif("bbb", 200));
    let json = serde_json::to_string(&h).unwrap();
    let back: ModificationHistory = serde_json::from_str(&json).unwrap();
    assert_eq!(back, h);
}

#[test]
fn modification_history_deserialize_rejects_oversize() {
    // A shard claiming 4 entries must be rejected — the buffer caps
    // at 3 and we don't silently truncate at deserialization (that
    // would mask a bug somewhere).
    let bad = serde_json::json!([
        { "commit": "a", "unix_timestamp": 1, "author": "x" },
        { "commit": "b", "unix_timestamp": 2, "author": "x" },
        { "commit": "c", "unix_timestamp": 3, "author": "x" },
        { "commit": "d", "unix_timestamp": 4, "author": "x" },
    ]);
    let result: Result<ModificationHistory, _> = serde_json::from_value(bad);
    assert!(result.is_err());
}

#[test]
fn modification_history_serializes_transparently() {
    // #[serde(transparent)] over the Vec — confirm the wire shape is
    // a JSON array, not a wrapping object.
    let mut h = ModificationHistory::new();
    h.push_most_recent(modif("aaa", 100));
    let json = serde_json::to_string(&h).unwrap();
    assert!(json.starts_with('['), "expected array, got {json}");
}
