use std::fs;

use aethyme_graph_indexer::{
    IndexerContext, LanguageRegistry, WalkOptions, index_repo_to_disk, index_repo_to_disk_with,
};
use aethyme_graph_storage::{read_coverage, read_units};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn context(root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("coverage-fixture", root, env!("CARGO_PKG_VERSION"))
        .unwrap()
        .with_source_revision(REVISION)
        .unwrap()
        .with_source_tree_digest("source-tree-digest")
        .unwrap()
}

fn write_python(root: &std::path::Path, source: &str) {
    fs::write(root.join("app.py"), source).unwrap();
}

#[test]
fn revision_bound_artifacts_are_content_free_and_exactly_digest_units() {
    let temporary = tempfile::tempdir().unwrap();
    let source = "# leading comment\ndef answer():\n    return 42\n";
    write_python(temporary.path(), source);

    let summary = index_repo_to_disk(&context(temporary.path()), &WalkOptions::default()).unwrap();
    let coverage = read_coverage(temporary.path()).unwrap();
    let units = read_units(temporary.path()).unwrap();

    assert!(coverage.available);
    assert_eq!(coverage.source_revision.as_deref(), Some(REVISION));
    assert_eq!(coverage.indexed_revision.as_deref(), Some(REVISION));
    assert_eq!(
        coverage.source_tree_sha256.as_deref(),
        Some("source-tree-digest")
    );
    assert_eq!(
        coverage.indexed_tree_sha256.as_deref(),
        Some("source-tree-digest")
    );
    assert_eq!(coverage.unit_count, units.len() as u64);
    assert_eq!(summary.coverage.report, coverage);
    assert!(units.iter().any(|unit| unit.kind == "function"));

    let function = units.iter().find(|unit| unit.kind == "function").unwrap();
    assert_eq!(function.path, "app.py");
    assert_eq!(function.start.line, 2);
    assert_eq!(function.end.line, 3);
    assert_eq!(
        function.content_digest,
        blake3::hash(&source.as_bytes()["# leading comment\n".len()..])
            .to_hex()
            .to_string()
    );

    let coverage_bytes = fs::read(temporary.path().join(".aethyme/graph/coverage.json")).unwrap();
    let units_bytes = fs::read(temporary.path().join(".aethyme/graph/units.ndjson")).unwrap();
    for bytes in [&coverage_bytes, &units_bytes] {
        assert!(
            !bytes
                .windows(b"return 42".len())
                .any(|window| window == b"return 42")
        );
        assert!(
            !bytes
                .windows(b"vector".len())
                .any(|window| window == b"vector")
        );
        assert!(
            !bytes
                .windows(b"embedding".len())
                .any(|window| window == b"embedding")
        );
    }
    assert!(coverage.safe_to_use);
}

#[test]
fn identical_revision_and_source_produce_byte_identical_artifacts() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let source = "class Answer:\n    def value(self):\n        return 42\n";
    write_python(first.path(), source);
    write_python(second.path(), source);

    index_repo_to_disk(&context(first.path()), &WalkOptions::default()).unwrap();
    index_repo_to_disk(&context(second.path()), &WalkOptions::default()).unwrap();

    for relative in [
        ".aethyme/graph/coverage.json",
        ".aethyme/graph/units.ndjson",
    ] {
        assert_eq!(
            fs::read(first.path().join(relative)).unwrap(),
            fs::read(second.path().join(relative)).unwrap(),
            "artifact {relative} must not depend on the checkout path"
        );
    }
}

#[test]
fn parser_gaps_are_recorded_without_aborting_the_file_pass() {
    let temporary = tempfile::tempdir().unwrap();
    write_python(temporary.path(), "def broken(:\n    return 1\n");

    let summary = index_repo_to_disk(&context(temporary.path()), &WalkOptions::default()).unwrap();

    assert_eq!(summary.coverage.report.files.partial, 1);
    assert_eq!(
        summary
            .coverage
            .report
            .exclusion_reasons
            .get(&aethyme_graph_storage::ExclusionReason::ParseError),
        Some(&1)
    );
    assert!(
        summary
            .coverage
            .report
            .gaps
            .iter()
            .any(|gap| gap == "partial_parses")
    );
    assert!(!summary.coverage.report.safe_to_use);
    assert!(temporary.path().join(".aethyme/graph/app.py.bin").is_file());
}

#[test]
fn missing_language_parser_is_visible_as_an_unsupported_gap() {
    let temporary = tempfile::tempdir().unwrap();
    write_python(temporary.path(), "def answer():\n    return 1\n");

    let registry = LanguageRegistry::new();
    let summary = index_repo_to_disk_with(
        &context(temporary.path()),
        &WalkOptions::default(),
        &registry,
    )
    .unwrap();

    assert_eq!(summary.coverage.report.files.unsupported, 1);
    assert_eq!(
        summary
            .coverage
            .report
            .exclusion_reasons
            .get(&aethyme_graph_storage::ExclusionReason::ParserUnavailable),
        Some(&1)
    );
    assert!(
        summary
            .coverage
            .report
            .by_parser
            .contains_key("unavailable")
    );
    assert!(!summary.coverage.report.safe_to_use);
}
