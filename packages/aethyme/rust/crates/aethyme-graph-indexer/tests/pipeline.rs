//! Integration tests for the IndexedFile → Fragment → disk pipeline.

use aethyme_graph_indexer::{
    IndexerContext, WalkOptions, build_fragment, build_index_records, index_repo_to_disk,
};
use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::{read_fragment, read_index_shard};

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0").unwrap()
}

// ─── build_fragment ─────────────────────────────────────────────────

#[test]
fn build_fragment_wraps_a_single_indexed_file() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
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
    write(tmp.path(), "src/cli.py", b"def alpha():\n    pass\n");
    write(tmp.path(), "src/util.py", b"def beta():\n    pass\n");
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    // build_index_records now takes BuiltFragments (post-4.3) so
    // we have to walk the pipeline a step further to construct
    // them. Each built fragment holds the file's top node plus
    // the Python indexer's extracted Function nodes.
    let py_indexer = aethyme_graph_indexer::PythonIndexer::new();
    use aethyme_graph_indexer::LanguageIndexer;
    let mut built = Vec::new();
    for indexed in &walked.files {
        let content = std::fs::read_to_string(tmp.path().join(&*indexed.source_path)).unwrap();
        let lang = py_indexer
            .index_file(&ctx(tmp.path()), indexed, &content)
            .unwrap();
        built.push(build_fragment(indexed, Some(lang)).unwrap());
    }

    let groups = build_index_records(&built);
    let modules: Vec<&str> = groups.keys().map(String::as_str).collect();
    assert!(modules.contains(&"src.cli"));
    assert!(modules.contains(&"src.util"));

    let cli_records = &groups["src.cli"];
    // alpha (extracted Function); File and NonCodeFile are
    // skipped by the name-only emission rule.
    let symbol_names: Vec<&str> = cli_records.iter().map(|r| r.symbol.as_ref()).collect();
    assert!(symbol_names.contains(&"alpha"));
}

// ─── index_repo_to_disk ─────────────────────────────────────────────

#[test]
fn index_repo_writes_fragments_to_canonical_paths() {
    let tmp = tempfile::tempdir().unwrap();
    // Real Python content so the language indexer produces a
    // Function node — the post-4.3 index shards only carry named
    // symbols, so a trivial `print('hi')` would write zero shard
    // records.
    write(tmp.path(), "src/cli.py", b"def hello():\n    return 'hi'\n");
    write(tmp.path(), "README.md", b"# heading\n");

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 2);
    assert_eq!(summary.fragments_written.len(), 2);

    let cli_frag = tmp.path().join(".aethyme/graph/src/cli.py.bin");
    assert!(cli_frag.exists());
    let readme_frag = tmp.path().join(".aethyme/graph/README.md.bin");
    assert!(readme_frag.exists());

    // src/cli.py contains a Function so src.cli gets a shard.
    // README.md contains only a NonCodeFile node (no extracted
    // named symbols) so it produces no shard. shards_written is
    // therefore exactly 1.
    assert_eq!(summary.shards_written.len(), 1);
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
    write(tmp.path(), "src/cli.py", b"def hello():\n    return 'hi'\n");

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let records = read_index_shard(tmp.path(), "src.cli").unwrap();
    // One record per extracted named symbol; the File node itself
    // is unnamed and skipped.
    assert!(!records.is_empty());
    let names: Vec<&str> = records.iter().map(|r| r.symbol.as_ref()).collect();
    assert!(names.contains(&"hello"));
    assert!(records.iter().any(|r| r.kind == NodeKind::Function));
}

#[test]
fn index_repo_is_idempotent() {
    // Two runs over the same repo state must produce identical
    // on-disk results.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def hello():\n    pass\n");
    write(tmp.path(), "src/util.py", b"def util_fn():\n    pass\n");
    write(tmp.path(), "README.md", b"# md\n");

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_first = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    let shard_first =
        std::fs::read(tmp.path().join(".aethyme/graph/_index/src.cli.ndjson")).unwrap();

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_second = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    let shard_second =
        std::fs::read(tmp.path().join(".aethyme/graph/_index/src.cli.ndjson")).unwrap();

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

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 4);
    // 3 File nodes (py + py + rs) and 1 NonCodeFile node (md)
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&3));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::NonCodeFile), Some(&1));
}

#[test]
fn index_repo_reports_content_free_phase_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let source = b"def hello():\n    return 'hi'\n";
    write(tmp.path(), "src/cli.py", source);

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 1);
    assert_eq!(summary.observability.source_bytes_read, source.len() as u64);
    assert!(summary.observability.fragment_bytes_written > 0);
    assert_eq!(
        summary.total_nodes,
        summary.counts_by_kind.values().sum::<usize>()
    );
    assert!(summary.total_nodes >= 2);
    assert!(summary.total_edges >= 1);
    // Timings are deliberately not threshold assertions: their presence in
    // the typed report is the contract, not host scheduling behavior.
    let _ = summary.observability.source_discovery_elapsed_us;
    let _ = summary.observability.source_indexing_elapsed_us;
    let _ = summary.observability.fragment_serialization_elapsed_us;
}

#[test]
fn index_repo_handles_empty_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 0);
    assert!(summary.fragments_written.is_empty());
    assert!(summary.shards_written.is_empty());
}
