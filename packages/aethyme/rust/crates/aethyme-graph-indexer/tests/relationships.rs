//! Playground acceptance tests for cross-file relationships in graph bootstrap.

use std::path::{Path, PathBuf};

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk};
use aethyme_graph_schema::{EdgeKind, Node, NodeId, NodeKind};
use aethyme_graph_storage::read_fragment;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(from, to).unwrap();
        }
    }
}

fn node_id(fragment: &aethyme_graph_storage::Fragment, kind: NodeKind, name: &str) -> NodeId {
    fragment
        .nodes()
        .iter()
        .find(|node| node.kind() == kind && node.name() == Some(name))
        .unwrap_or_else(|| panic!("missing {kind:?} node named {name:?}"))
        .id()
        .clone()
}

fn fixture_repo() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("playground");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playground_docs");
    copy_tree(&fixture, &repository);
    (temp, repository)
}

#[test]
fn playground_bootstrap_indexes_docs_config_and_test_relationships() {
    let (_temp, repository) = fixture_repo();
    let context = IndexerContext::new("playground-docs", &repository, "0.1.0").unwrap();

    let summary = index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 8);
    assert_eq!(summary.counts_by_kind.get(&NodeKind::DocSection), Some(&3));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::ConfigValue), Some(&3));
    assert_eq!(
        summary.total_nodes,
        summary.counts_by_kind.values().sum::<usize>()
    );
    assert!(summary.total_edges >= 11);
    assert!(!summary.coverage.report.safe_to_use);

    let handler = read_fragment(&repository, "src/handlers.py").unwrap();
    let handler_id = node_id(&handler, NodeKind::Function, "handle_request");

    let docs = read_fragment(&repository, "README.md").unwrap();
    let api_section = node_id(&docs, NodeKind::DocSection, "API");
    let narrative_section = node_id(&docs, NodeKind::DocSection, "Narrative");
    let ambiguous_section = node_id(&docs, NodeKind::DocSection, "Ambiguous");
    assert!(
        docs.edges().iter().any(|edge| {
            edge.kind() == EdgeKind::Documents
                && edge.src_id() == &api_section
                && edge.dst_id() == &handler_id
        }),
        "explicit inline-code and Markdown-fragment references should resolve"
    );
    assert!(
        !docs.edges().iter().any(|edge| {
            edge.kind() == EdgeKind::Documents && edge.src_id() == &narrative_section
        }),
        "plain prose must not be promoted into a symbol link"
    );
    assert!(
        !docs.edges().iter().any(|edge| {
            edge.kind() == EdgeKind::Documents && edge.src_id() == &ambiguous_section
        }),
        "ambiguous symbol names must remain unresolved"
    );

    for (path, config_path, line) in [
        ("config.yaml", "server.handler", 2),
        ("config.json", "bootstrap.handler", 3),
        ("config.toml", "bootstrap.handler", 2),
    ] {
        let config = read_fragment(&repository, path).unwrap();
        let value = config
            .nodes()
            .iter()
            .find(|node| node.kind() == NodeKind::ConfigValue && node.name() == Some(config_path))
            .unwrap_or_else(|| panic!("missing {config_path} in {path}"));
        let Node::ConfigValue(config_value) = value else {
            panic!("expected ConfigValue node");
        };
        assert_eq!(config_value.source_range().start_line(), line);
        assert!(
            config.edges().iter().any(|edge| {
                edge.kind() == EdgeKind::References
                    && edge.src_id() == value.id()
                    && edge.dst_id() == &handler_id
            }),
            "qualified configuration value in {path} should reference the unique handler"
        );
        assert!(
            config
                .edges()
                .iter()
                .any(|edge| { edge.kind() == EdgeKind::Contains && edge.dst_id() == value.id() }),
            "configuration value should retain its non-code file parent"
        );
    }

    let tests = read_fragment(&repository, "tests/test_handlers.py").unwrap();
    let test_id = node_id(&tests, NodeKind::Function, "test_handles_request");
    assert!(
        tests.edges().iter().any(|edge| {
            edge.kind() == EdgeKind::Tests
                && edge.src_id() == &test_id
                && edge.dst_id() == &handler_id
        }),
        "a test function calling a uniquely resolved symbol should receive a Tests edge"
    );

    let expected_source_bytes: u64 = [
        "README.md",
        "src/handlers.py",
        "src/left.py",
        "src/right.py",
        "tests/test_handlers.py",
        "config.yaml",
        "config.json",
        "config.toml",
    ]
    .iter()
    .map(|path| std::fs::metadata(repository.join(path)).unwrap().len())
    .sum();
    assert_eq!(
        summary.observability.source_bytes_read,
        expected_source_bytes
    );

    let indexed_paths = [
        "README.md",
        "config.yaml",
        "config.json",
        "config.toml",
        "tests/test_handlers.py",
    ];
    let first_run: Vec<Vec<u8>> = indexed_paths
        .iter()
        .map(|path| {
            std::fs::read(
                repository
                    .join(".aethyme/graph")
                    .join(format!("{path}.bin")),
            )
            .unwrap()
        })
        .collect();
    index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
    let second_run: Vec<Vec<u8>> = indexed_paths
        .iter()
        .map(|path| {
            std::fs::read(
                repository
                    .join(".aethyme/graph")
                    .join(format!("{path}.bin")),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        first_run, second_run,
        "relationship fragments should be deterministic"
    );
}
