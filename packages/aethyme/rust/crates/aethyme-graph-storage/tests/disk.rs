//! Integration tests for disk I/O. Uses tempfile for filesystem
//! isolation.

use aethyme_graph_schema::{Function, Node, NodeKind, ParameterSignature, SourceRange, Visibility};
use aethyme_graph_storage::{
    Fragment, FragmentReadError, FragmentStore, FragmentWriteError, OverlayDecodeError,
    OverlayFragment, OverlayReadError, SymbolRecord, read_fragment, read_index_shard, read_overlay,
    write_fragment, write_index_shard, write_overlay,
};
use std::collections::BTreeMap;

fn sample_fragment() -> Fragment {
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
    Fragment::new("src/cli.py", vec![Node::Function(f)], vec![]).unwrap()
}

fn sample_record() -> SymbolRecord {
    use aethyme_graph_schema::NodeId;
    SymbolRecord {
        module: "src.cli".into(),
        symbol: "explore_command".into(),
        kind: NodeKind::Function,
        node_id: NodeId::new(
            NodeKind::Function,
            "aethyme",
            "src/cli.py",
            "explore_command",
        )
        .unwrap(),
        file: "src/cli.py".into(),
    }
}

// ─── Fragment disk I/O ──────────────────────────────────────────────

#[test]
fn write_then_read_fragment_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let frag = sample_fragment();
    let written = write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    assert!(written.exists());
    let back = read_fragment(tmp.path(), "src/cli.py").unwrap();
    assert_eq!(back, frag);
}

#[test]
fn write_fragment_creates_parent_directories() {
    let tmp = tempfile::tempdir().unwrap();
    // Deeply nested source path; parent dirs don't exist yet.
    let frag = Fragment::new("packages/auth/src/jwt.rs", vec![], vec![]).unwrap();
    write_fragment(tmp.path(), "packages/auth/src/jwt.rs", &frag).unwrap();
    let expected = tmp
        .path()
        .join(".aethyme/graph/packages/auth/src/jwt.rs.bin");
    assert!(expected.exists());
}

#[test]
fn write_fragment_is_atomic_no_partial_file_after_success() {
    // After a successful write, the temp file should be gone and
    // only the final file should exist.
    let tmp = tempfile::tempdir().unwrap();
    let frag = sample_fragment();
    write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();

    let target = tmp.path().join(".aethyme/graph/src/cli.py.bin");
    assert!(target.exists());

    // No leftover tempfile in the same directory.
    let parent = target.parent().unwrap();
    for entry in std::fs::read_dir(parent).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with("cli.py.bin.tmp."),
            "leftover tempfile found: {name}"
        );
    }
}

#[test]
fn write_fragment_rejects_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let frag = Fragment::new("evil.py", vec![], vec![]).unwrap();
    let err = write_fragment(tmp.path(), "../../etc/passwd", &frag).unwrap_err();
    assert!(matches!(err, FragmentWriteError::Path(_)));
    // No file created inside the fragment root. (Joining the traversal
    // path would resolve to the real /etc/passwd on Linux, where tempdirs
    // sit directly under /tmp, so inspect the tempdir contents instead.)
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "traversal attempt must not create any file in the fragment root"
    );
}

#[test]
fn read_fragment_reports_missing_file_as_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_fragment(tmp.path(), "nonexistent.py").unwrap_err();
    assert!(matches!(err, FragmentReadError::Io(_)));
}

#[test]
fn read_fragment_reports_corrupt_bytes_as_decode_error() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join(".aethyme/graph/src/cli.py.bin");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"not a valid bincode payload").unwrap();
    let err = read_fragment(tmp.path(), "src/cli.py").unwrap_err();
    assert!(matches!(err, FragmentReadError::Decode(_)));
}

// ─── Index shard disk I/O ───────────────────────────────────────────

#[test]
fn write_then_read_index_shard_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let records = vec![sample_record()];
    write_index_shard(tmp.path(), "src.cli", &records).unwrap();
    let back = read_index_shard(tmp.path(), "src.cli").unwrap();
    assert_eq!(back, records);
}

#[test]
fn write_index_shard_creates_index_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    write_index_shard(tmp.path(), "src.cli", &[]).unwrap();
    let shard = tmp.path().join(".aethyme/graph/_index/src.cli.ndjson");
    assert!(shard.exists());
}

#[test]
fn write_index_shard_rejects_path_chars_in_module_name() {
    let tmp = tempfile::tempdir().unwrap();
    let result = write_index_shard(tmp.path(), "evil/module", &[]);
    assert!(result.is_err());
}

// ─── Overlay disk I/O ───────────────────────────────────────────────

/// Sample payload used by overlay tests. `BTreeMap` gives us
/// deterministic serialization for free, so the tests stay focused
/// on the wrapper rather than payload-author discipline.
fn sample_overlay_payload() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("layer".into(), "domain".into());
    m.insert("owner".into(), "platform".into());
    m
}

#[test]
fn write_then_read_overlay_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let overlay = OverlayFragment::new(
        "structure",
        "structure-producer/0.1.0",
        sample_overlay_payload(),
    )
    .unwrap();
    let written = write_overlay(tmp.path(), "structure", &overlay).unwrap();
    assert!(written.exists());
    let back: OverlayFragment<BTreeMap<String, String>> =
        read_overlay(tmp.path(), "structure").unwrap();
    assert_eq!(back, overlay);
}

#[test]
fn write_overlay_lands_under_overlays_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let overlay =
        OverlayFragment::new("configs", "configs/0.1.0", sample_overlay_payload()).unwrap();
    write_overlay(tmp.path(), "configs", &overlay).unwrap();
    let expected = tmp.path().join(".aethyme/graph/_overlays/configs.bin");
    assert!(expected.exists());
}

#[test]
fn read_overlay_kind_mismatch_fails_loudly() {
    // Write the bytes under one kind, then hand-place them under
    // another and try to decode. Simulates either tampering or a
    // caller asking for the wrong kind.
    let tmp = tempfile::tempdir().unwrap();
    let overlay = OverlayFragment::new("risks", "risks/0.1.0", sample_overlay_payload()).unwrap();
    write_overlay(tmp.path(), "risks", &overlay).unwrap();

    // Copy risks.bin → structure.bin to simulate a kind/filename
    // mismatch on disk.
    let overlays_dir = tmp.path().join(".aethyme/graph/_overlays");
    std::fs::copy(
        overlays_dir.join("risks.bin"),
        overlays_dir.join("structure.bin"),
    )
    .unwrap();

    let err = read_overlay::<BTreeMap<String, String>>(tmp.path(), "structure").unwrap_err();
    assert!(
        matches!(
            err,
            OverlayReadError::Decode(OverlayDecodeError::KindMismatch { .. })
        ),
        "expected KindMismatch, got {err:?}",
    );
}

#[test]
fn fragment_store_list_overlays_returns_sorted_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    // Write in non-alphabetical order to prove the sort.
    for kind in ["risks", "configs", "structure"] {
        let overlay =
            OverlayFragment::new(kind, &format!("{kind}/0.1.0"), sample_overlay_payload()).unwrap();
        write_overlay(tmp.path(), kind, &overlay).unwrap();
    }

    let store = FragmentStore::open(tmp.path()).unwrap();
    let kinds = store.list_overlays().unwrap();
    assert_eq!(kinds, vec!["configs", "risks", "structure"]);
}

#[test]
fn fragment_store_list_overlays_empty_when_no_overlays() {
    // Need a per-file fragment to make the graph dir exist (so
    // FragmentStore::open succeeds) without creating _overlays/.
    let tmp = tempfile::tempdir().unwrap();
    write_fragment(tmp.path(), "src/cli.py", &sample_fragment()).unwrap();
    let store = FragmentStore::open(tmp.path()).unwrap();
    assert!(store.list_overlays().unwrap().is_empty());
}

#[test]
fn fragment_store_does_not_enumerate_overlay_bins_as_source_paths() {
    let tmp = tempfile::tempdir().unwrap();
    // One real fragment, one overlay sitting next to it.
    write_fragment(tmp.path(), "src/cli.py", &sample_fragment()).unwrap();
    let overlay =
        OverlayFragment::new("structure", "structure/0.1.0", sample_overlay_payload()).unwrap();
    write_overlay(tmp.path(), "structure", &overlay).unwrap();

    let store = FragmentStore::open(tmp.path()).unwrap();
    let paths = store.list_indexed_source_paths().unwrap();
    assert_eq!(paths, vec!["src/cli.py"]);
}

#[test]
fn overlay_bytes_on_disk_byte_identical_across_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let overlay =
        OverlayFragment::new("structure", "structure/0.1.0", sample_overlay_payload()).unwrap();

    write_overlay(tmp.path(), "structure", &overlay).unwrap();
    let bytes_a = std::fs::read(tmp.path().join(".aethyme/graph/_overlays/structure.bin")).unwrap();

    write_overlay(tmp.path(), "structure", &overlay).unwrap();
    let bytes_b = std::fs::read(tmp.path().join(".aethyme/graph/_overlays/structure.bin")).unwrap();

    assert_eq!(bytes_a, bytes_b);
}

// ─── Determinism across disk round-trip ─────────────────────────────

#[test]
fn fragment_bytes_on_disk_byte_identical_across_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let frag = sample_fragment();

    write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    let bytes_a = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();

    // Rewrite from scratch
    write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    let bytes_b = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();

    assert_eq!(bytes_a, bytes_b);
}
