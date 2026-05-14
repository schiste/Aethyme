//! Integration tests for disk I/O. Uses tempfile for filesystem
//! isolation.

use aethyme_graph_schema::{
    Function, Node, NodeKind, ParameterSignature, SourceRange, Visibility,
};
use aethyme_graph_storage::{
    read_fragment, read_index_shard, write_fragment, write_index_shard,
    Fragment, FragmentReadError, FragmentWriteError, SymbolRecord,
};

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
    let written =
        write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    assert!(written.exists());
    let back = read_fragment(tmp.path(), "src/cli.py").unwrap();
    assert_eq!(back, frag);
}

#[test]
fn write_fragment_creates_parent_directories() {
    let tmp = tempfile::tempdir().unwrap();
    // Deeply nested source path; parent dirs don't exist yet.
    let frag = Fragment::new(
        "packages/auth/src/jwt.rs",
        vec![],
        vec![],
    )
    .unwrap();
    write_fragment(tmp.path(), "packages/auth/src/jwt.rs", &frag).unwrap();
    let expected = tmp.path().join(
        ".aethyme/graph/packages/auth/src/jwt.rs.bin",
    );
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
    let err = write_fragment(tmp.path(), "../../etc/passwd", &frag)
        .unwrap_err();
    assert!(matches!(err, FragmentWriteError::Path(_)));
    // No file created.
    assert!(!tmp.path().join("../../etc/passwd").exists());
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

// ─── Determinism across disk round-trip ─────────────────────────────

#[test]
fn fragment_bytes_on_disk_byte_identical_across_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let frag = sample_fragment();

    write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    let bytes_a = std::fs::read(
        tmp.path().join(".aethyme/graph/src/cli.py.bin"),
    )
    .unwrap();

    // Rewrite from scratch
    write_fragment(tmp.path(), "src/cli.py", &frag).unwrap();
    let bytes_b = std::fs::read(
        tmp.path().join(".aethyme/graph/src/cli.py.bin"),
    )
    .unwrap();

    assert_eq!(bytes_a, bytes_b);
}
