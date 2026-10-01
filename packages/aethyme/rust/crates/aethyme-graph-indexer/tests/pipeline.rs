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

/// Deleting a source file must remove its fragment and its index shard.
///
/// Writing artifacts is not enough to keep the fragment store accurate:
/// a removed file used to leave `*.bin` and `_index/*.ndjson` behind
/// forever, so the linker kept indexing symbols belonging to a file
/// that no longer existed and resolved them as if they were live.
///
/// `aethyme graph refresh` hid this by deleting the whole graph
/// directory before rebuilding; only a direct `index_repo_to_disk`
/// accumulated them.
#[test]
fn deleting_a_source_file_removes_its_fragment_and_shard() {
    let tmp = tempfile::tempdir().unwrap();
    // A source revision is required for `units.ndjson` to be written,
    // and that file is the record pruning reads to learn what the
    // previous pass owned.
    let ctx = IndexerContext::new("prune-test", tmp.path().to_path_buf(), "0.1.0")
        .unwrap()
        .with_source_revision("0123456789abcdef0123456789abcdef01234567")
        .unwrap()
        .with_source_tree_digest("tree-digest")
        .unwrap();

    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/keep.py"), "def keep():\n    return 1\n").unwrap();
    std::fs::write(tmp.path().join("src/gone.py"), "def gone():\n    return 2\n").unwrap();

    let first = index_repo_to_disk(&ctx, &WalkOptions::default()).unwrap();
    let gone_fragment = tmp.path().join(".aethyme/graph/src/gone.py.bin");
    assert!(gone_fragment.is_file(), "fixture must produce a fragment");
    assert_eq!(first.stale_artifacts_removed, 0);

    std::fs::remove_file(tmp.path().join("src/gone.py")).unwrap();

    let second = index_repo_to_disk(&ctx, &WalkOptions::default()).unwrap();

    assert!(
        !gone_fragment.exists(),
        "a deleted source must not leave its fragment behind"
    );
    assert!(
        second.stale_artifacts_removed > 0,
        "the summary must report the pruned artifacts"
    );

    // The surviving file is untouched.
    assert!(tmp.path().join(".aethyme/graph/src/keep.py.bin").is_file());
}

/// Pruning must not remove files it did not write, such as a
/// hand-placed artifact living alongside the generated ones.
#[test]
fn pruning_leaves_unrelated_files_in_the_graph_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = IndexerContext::new("prune-scope-test", tmp.path().to_path_buf(), "0.1.0").unwrap();

    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/only.py"), "def only():\n    return 1\n").unwrap();
    index_repo_to_disk(&ctx, &WalkOptions::default()).unwrap();

    let marker = tmp.path().join(".aethyme/graph/NOTES.txt");
    std::fs::write(&marker, "hand-written").unwrap();
    let stray_bin = tmp.path().join(".aethyme/graph/stray.bin");
    std::fs::write(&stray_bin, "not ours").unwrap();

    index_repo_to_disk(&ctx, &WalkOptions::default()).unwrap();

    assert!(marker.is_file(), "non-artifact files must survive pruning");
    assert!(
        stray_bin.is_file(),
        "a .bin the indexer did not write is indistinguishable by extension alone; \
         pruning is scoped to paths this pass wrote, so it must be left alone"
    );
}
