//! Integration tests for the PHP indexer (tree-sitter-php).

use aethyme_graph_indexer::{
    index_repo_to_disk, IndexerContext, LanguageIndexer, PhpIndexer,
    WalkOptions,
};
use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::read_fragment;

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0")
        .unwrap()
}

fn index_source(content: &str) -> aethyme_graph_indexer::LanguageIndexResult {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/x.php", content.as_bytes());
    let walked = aethyme_graph_indexer::walk_source_tree(
        &ctx(tmp.path()),
        &WalkOptions::default(),
    )
    .unwrap();
    let indexed = walked
        .files
        .iter()
        .find(|f| &*f.source_path == "src/x.php")
        .expect("indexed file missing");
    let indexer = PhpIndexer::new().unwrap();
    indexer
        .index_file(&ctx(tmp.path()), indexed, content)
        .unwrap()
}

// ─── Function / Class / Method extraction ───────────────────────────

#[test]
fn extracts_top_level_function() {
    let result = index_source(
        "<?php\nfunction hello(string $name): string {\n    return \"hi $name\";\n}\n",
    );
    let functions: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
}

#[test]
fn extracts_class_and_its_methods() {
    let result = index_source(
        "<?php\nclass User {\n    public function getName(): string { return 'x'; }\n    public function getId(): int { return 1; }\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Class).count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Method).count(), 2);
}

#[test]
fn extracts_interface_with_methods() {
    let result = index_source(
        "<?php\ninterface Greet {\n    public function hello(): string;\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Interface));
    assert!(kinds.contains(&NodeKind::Method));
}

#[test]
fn extracts_trait_with_methods() {
    let result = index_source(
        "<?php\ntrait Sayable {\n    public function say(): void {}\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Trait));
    assert!(kinds.contains(&NodeKind::Method));
}

// ─── Method modifiers ──────────────────────────────────────────────

#[test]
fn private_method_marks_private_visibility() {
    let result = index_source(
        "<?php\nclass Foo {\n    private function helper(): void {}\n}\n",
    );
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(
        json.contains("\"visibility\":\"private\""),
        "expected private, got: {json}"
    );
}

#[test]
fn protected_method_marks_protected_visibility() {
    let result = index_source(
        "<?php\nclass Foo {\n    protected function helper(): void {}\n}\n",
    );
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(json.contains("\"visibility\":\"protected\""));
}

#[test]
fn static_method_marks_is_static_true() {
    let result = index_source(
        "<?php\nclass Foo {\n    public static function helper(): void {}\n}\n",
    );
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(json.contains("\"is_static\":true"));
}

// ─── Constants ──────────────────────────────────────────────────────

#[test]
fn extracts_const_declarations_as_globals() {
    let result = index_source(
        "<?php\nconst PI = 3.14;\nconst MAX = 100;\n",
    );
    let globals: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::GlobalVariable)
        .collect();
    assert_eq!(globals.len(), 2);
}

// ─── Namespace walking ─────────────────────────────────────────────

#[test]
fn namespace_body_walked_as_top_level_in_v1() {
    let result = index_source(
        "<?php\nnamespace App\\Domain;\n\nclass User {}\nfunction make_user(): User { return new User; }\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Class));
    assert!(kinds.contains(&NodeKind::Function));
}

// Braced-namespace handling (`namespace Foo { class Bar {} }`)
// is intentionally deferred in v1: it wraps the class in a
// compound_statement that needs recursive walking. MediaWiki and
// most modern PHP use the bare form (`namespace Foo;`) which v1
// handles via the namespace_body_walked_as_top_level_in_v1 test.
// Future PHP indexer commit can add the braced-form walk.

// ─── End-to-end pipeline ────────────────────────────────────────────

#[test]
fn end_to_end_php_file_produces_enriched_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "includes/User.php",
        b"<?php\nnamespace MediaWiki;\n\nclass User {\n    public function getId(): int { return 1; }\n    private function helper(): void {}\n}\n",
    );

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 1);

    let frag = read_fragment(tmp.path(), "includes/User.php").unwrap();
    // File + Class + 2 Methods = 4 nodes
    assert_eq!(frag.node_count(), 4);
}

#[test]
fn end_to_end_mediawiki_style_repo_indexed() {
    // Tiny MediaWiki-ish layout to validate the indexer handles
    // realistic PHP repo shapes.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "includes/WikiPage.php",
        b"<?php\nclass WikiPage {\n    public function getTitle() {}\n    protected function loadFromDB(): void {}\n}\n",
    );
    write(
        tmp.path(),
        "includes/Article.php",
        b"<?php\nclass Article {\n    public function view(): void {}\n}\n",
    );

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 2);
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Class), Some(&2));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Method), Some(&3));
}

#[test]
fn php_indexing_determinism() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/x.php",
        b"<?php\nclass Foo {\n    public function bar(): void {}\n}\nfunction baz() {}\n",
    );

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_a =
        std::fs::read(tmp.path().join(".aethyme/graph/src/x.php.bin")).unwrap();
    std::fs::remove_dir_all(tmp.path().join(".aethyme")).unwrap();
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_b =
        std::fs::read(tmp.path().join(".aethyme/graph/src/x.php.bin")).unwrap();
    assert_eq!(bytes_a, bytes_b);
}
