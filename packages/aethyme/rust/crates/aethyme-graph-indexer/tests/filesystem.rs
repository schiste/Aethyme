//! Integration tests for the filesystem-only indexer.

use aethyme_graph_indexer::{
    classify_file, infer_language_from_extension, walk_source_tree,
    FileClassification, IndexerContext, SkipReason, WalkOptions,
};
use aethyme_graph_schema::{NodeKind, NonCodeFormat};

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0")
        .unwrap()
}

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

// ─── Context validation ─────────────────────────────────────────────

#[test]
fn context_rejects_relative_repo_root() {
    use aethyme_graph_indexer::context::IndexerContextError;
    let err = IndexerContext::new(
        "testrepo",
        std::path::PathBuf::from("relative/path"),
        "0.1.0",
    )
    .unwrap_err();
    assert!(matches!(err, IndexerContextError::RelativeRepoRoot { .. }));
}

#[test]
fn context_rejects_repo_name_with_colon() {
    use aethyme_graph_indexer::context::IndexerContextError;
    let tmp = tempfile::tempdir().unwrap();
    let err = IndexerContext::new(
        "bad:name",
        tmp.path().to_path_buf(),
        "0.1.0",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        IndexerContextError::RepoNameContainsColon { .. }
    ));
}

// ─── Language mapping ───────────────────────────────────────────────

#[test]
fn extension_maps_to_canonical_language() {
    assert_eq!(infer_language_from_extension("py"), Some("python"));
    assert_eq!(infer_language_from_extension("rs"), Some("rust"));
    assert_eq!(infer_language_from_extension("ts"), Some("typescript"));
    assert_eq!(infer_language_from_extension("tsx"), Some("typescript"));
    assert_eq!(infer_language_from_extension("php"), Some("php"));
    assert_eq!(infer_language_from_extension("unknown"), None);
}

#[test]
fn file_classification_handles_code_non_code_and_ignored() {
    assert!(matches!(
        classify_file("cli.py"),
        FileClassification::Code { language: "python" }
    ));
    assert!(matches!(
        classify_file("README.md"),
        FileClassification::NonCode {
            format: NonCodeFormat::Markdown
        }
    ));
    assert!(matches!(
        classify_file("README.MD"),
        FileClassification::NonCode { .. }
    ));
    assert!(matches!(
        classify_file("Dockerfile"),
        FileClassification::Code {
            language: "dockerfile"
        }
    ));
    assert!(matches!(
        classify_file("noextension"),
        FileClassification::Ignored
    ));
    assert!(matches!(
        classify_file("image.png"),
        FileClassification::Ignored
    ));
}

// ─── Filesystem walking ─────────────────────────────────────────────

#[test]
fn walks_basic_source_tree() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"# Python file\n");
    write(tmp.path(), "src/lib.rs", b"// Rust file\n");
    write(tmp.path(), "README.md", b"# Heading\n");

    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    let paths: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(paths, vec!["README.md", "src/cli.py", "src/lib.rs"]);
    assert!(result.skipped.is_empty());
}

#[test]
fn ignores_default_ignore_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"# real source\n");
    write(tmp.path(), ".git/config", b"# git internal\n");
    write(tmp.path(), "node_modules/foo/index.js", b"// dep\n");
    write(tmp.path(), "target/debug/foo.rs", b"// build artifact\n");
    write(tmp.path(), ".aethyme/graph/something.bin", b"\x00");

    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    let paths: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(paths, vec!["src/cli.py"]);
}

#[test]
fn extra_ignore_dirs_extend_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"# real\n");
    write(tmp.path(), "vendor/lib.py", b"# vendored\n");

    let mut options = WalkOptions::default();
    options.extra_ignore_dirs.push("vendor".into());

    let result = walk_source_tree(&ctx(tmp.path()), &options).unwrap();
    let paths: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(paths, vec!["src/cli.py"]);
}

#[test]
fn skips_files_over_size_limit() {
    let tmp = tempfile::tempdir().unwrap();
    // 100 KiB of 'a' — over a 10 KiB limit
    write(tmp.path(), "src/big.py", &vec![b'a'; 100_000]);
    write(tmp.path(), "src/small.py", b"print('hi')\n");

    let options = WalkOptions {
        max_file_size_bytes: Some(10_000),
        ..Default::default()
    };

    let result = walk_source_tree(&ctx(tmp.path()), &options).unwrap();
    let kept: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(kept, vec!["src/small.py"]);

    let skipped: Vec<(&str, SkipReason)> = result
        .skipped
        .iter()
        .map(|s| (s.source_path.as_ref(), s.reason))
        .collect();
    assert_eq!(skipped, vec![("src/big.py", SkipReason::OverSizeLimit)]);
}

#[test]
fn skips_unrecognized_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"# good\n");
    write(tmp.path(), "image.png", b"\x89PNG\r\n");
    write(tmp.path(), "data.bin", b"\x00\x01\x02");

    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    let kept: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(kept, vec!["src/cli.py"]);

    let skipped: Vec<&str> =
        result.skipped.iter().map(|s| s.source_path.as_ref()).collect();
    assert!(skipped.contains(&"image.png"));
    assert!(skipped.contains(&"data.bin"));
}

#[test]
fn indexed_file_nodes_carry_correct_kind_and_language() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"print('hi')\n");
    write(tmp.path(), "README.md", b"# md\n");

    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();

    let py = result.files.iter().find(|f| &*f.source_path == "cli.py").unwrap();
    assert_eq!(py.top_node.kind(), NodeKind::File);
    assert_eq!(&*py.language, "python");

    let md = result
        .files
        .iter()
        .find(|f| &*f.source_path == "README.md")
        .unwrap();
    assert_eq!(md.top_node.kind(), NodeKind::NonCodeFile);
    assert_eq!(&*md.language, "markdown");
}

#[test]
fn output_is_canonically_sorted_by_source_path() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "z/last.py", b"# z\n");
    write(tmp.path(), "a/first.py", b"# a\n");
    write(tmp.path(), "m/middle.py", b"# m\n");

    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    let paths: Vec<&str> =
        result.files.iter().map(|f| f.source_path.as_ref()).collect();
    assert_eq!(paths, vec!["a/first.py", "m/middle.py", "z/last.py"]);
}

#[test]
fn empty_repo_produces_empty_result() {
    let tmp = tempfile::tempdir().unwrap();
    let result = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    assert!(result.files.is_empty());
    assert!(result.skipped.is_empty());
}

#[test]
fn same_repo_state_produces_byte_identical_results_across_walks() {
    // Determinism check: the canonical sort + deterministic
    // metadata extraction should make repeated walks produce
    // exactly equal results.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"print('hi')\n");
    write(tmp.path(), "src/lib.rs", b"fn main() {}\n");
    write(tmp.path(), "README.md", b"# md\n");

    let a = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    let b = walk_source_tree(&ctx(tmp.path()), &WalkOptions::default())
        .unwrap();
    assert_eq!(a, b);
}
