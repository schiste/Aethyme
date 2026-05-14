//! Integration tests for the IndexedFile → Fragment → disk pipeline.

use aethyme_graph_indexer::{
    build_fragment, build_index_records, index_repo_to_disk,
    IndexerContext, WalkOptions,
};
use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::{read_fragment, read_index_shard};

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0")
        .unwrap()
}

// ─── build_fragment ─────────────────────────────────────────────────

#[test]
fn build_fragment_wraps_a_single_indexed_file() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");
    let walked = aethyme_graph_indexer::walk_source_tree(
        &ctx(tmp.path()),
        &WalkOptions::default(),
    )
    .unwrap();
    assert_eq!(walked.files.len(), 1);
    let built = build_fragment(&walked.files[0], None).unwrap();
    assert_eq!(&*built.source_path, "src/cli.py");
    assert_eq!(built.fragment.node_count(), 1);
    assert_eq!(built.fragment.edge_count(), 0);
    assert_eq!(built.fragment.nodes()[0].kind(), NodeKind::File);
}

// ─── build_index_records ────────────────────────────────────────────

#[test]
fn index_records_group_by_synthesized_module() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('a')\n");
    write(tmp.path(), "src/util.py", b"print('b')\n");
    write(tmp.path(), "other/lib.rs", b"fn main() {}\n");
    let walked = aethyme_graph_indexer::walk_source_tree(
        &ctx(tmp.path()),
        &WalkOptions::default(),
    )
    .unwrap();

    let groups = build_index_records(&walked.files);
    let modules: Vec<&str> = groups.keys().map(String::as_str).collect();
    assert!(modules.contains(&"src.cli"));
    assert!(modules.contains(&"src.util"));
    assert!(modules.contains(&"other.lib"));

    let cli_records = &groups["src.cli"];
    assert_eq!(cli_records.len(), 1);
    assert_eq!(&*cli_records[0].symbol, "cli");
    assert_eq!(cli_records[0].kind, NodeKind::File);
}

// ─── index_repo_to_disk ─────────────────────────────────────────────

#[test]
fn index_repo_writes_fragments_to_canonical_paths() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");
    write(tmp.path(), "README.md", b"# heading\n");

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 2);
    assert_eq!(summary.fragments_written.len(), 2);

    let cli_frag = tmp.path().join(".aethyme/graph/src/cli.py.bin");
    assert!(cli_frag.exists());
    let readme_frag = tmp.path().join(".aethyme/graph/README.md.bin");
    assert!(readme_frag.exists());

    // Each shard exists.
    assert!(!summary.shards_written.is_empty());
}

#[test]
fn index_repo_round_trip_fragment_decode_works() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let frag = read_fragment(tmp.path(), "src/cli.py").unwrap();
    assert_eq!(frag.file_path(), "src/cli.py");
    assert_eq!(frag.node_count(), 1);
    assert_eq!(frag.nodes()[0].kind(), NodeKind::File);
}

#[test]
fn index_repo_round_trip_index_shard_decode_works() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let records = read_index_shard(tmp.path(), "src.cli").unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(&*records[0].symbol, "cli");
    assert_eq!(records[0].kind, NodeKind::File);
    assert_eq!(&*records[0].file, "src/cli.py");
}

#[test]
fn index_repo_is_idempotent() {
    // Two runs over the same repo state must produce identical
    // on-disk results.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");
    write(tmp.path(), "src/util.py", b"print('u')\n");
    write(tmp.path(), "README.md", b"# md\n");

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_first =
        std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    let shard_first = std::fs::read(
        tmp.path().join(".aethyme/graph/_index/src.cli.ndjson"),
    )
    .unwrap();

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_second =
        std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    let shard_second = std::fs::read(
        tmp.path().join(".aethyme/graph/_index/src.cli.ndjson"),
    )
    .unwrap();

    assert_eq!(bytes_first, bytes_second);
    assert_eq!(shard_first, shard_second);
}

#[test]
fn index_repo_counts_by_kind() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/a.py", b"# py\n");
    write(tmp.path(), "src/b.py", b"# py\n");
    write(tmp.path(), "src/c.rs", b"// rs\n");
    write(tmp.path(), "README.md", b"# md\n");

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 4);
    // 3 File nodes (py + py + rs) and 1 NonCodeFile node (md)
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&3));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::NonCodeFile), Some(&1));
}

#[test]
fn index_repo_handles_empty_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 0);
    assert!(summary.fragments_written.is_empty());
    assert!(summary.shards_written.is_empty());
}
