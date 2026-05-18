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

// ─── Phase 4.6 stage 1: import extraction ───────────────────────────

#[test]
fn php_simple_use_emits_placeholder_for_last_segment() {
    let result = index_source(
        "<?php\nuse App\\Models\\User;\n",
    );
    let n_unresolved = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"User\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(
        edge_json.contains("\"import_path\":\"App\\\\Models\\\\User\""),
        "edge: {edge_json}"
    );
    assert!(edge_json.contains("\"is_named\":true"));
}

#[test]
fn php_aliased_use_uses_alias_as_binding() {
    let result = index_source(
        "<?php\nuse App\\Models\\User as U;\n",
    );
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"U\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    // import_path keeps the unaliased path.
    assert!(
        edge_json.contains("\"import_path\":\"App\\\\Models\\\\User\""),
        "edge: {edge_json}"
    );
}

#[test]
fn php_group_use_flattens_into_multiple_placeholders() {
    let result = index_source(
        "<?php\nuse App\\Models\\{User, Post as P, Tag};\n",
    );
    let n_unresolved = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 3);
    let edge_jsons: Vec<String> = result
        .additional_edges
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(edge_jsons
        .iter()
        .any(|j| j.contains("\"import_path\":\"App\\\\Models\\\\User\"")));
    assert!(edge_jsons
        .iter()
        .any(|j| j.contains("\"import_path\":\"App\\\\Models\\\\Post\"")));
    assert!(edge_jsons
        .iter()
        .any(|j| j.contains("\"import_path\":\"App\\\\Models\\\\Tag\"")));
    // The aliased one's binding is P.
    let node_jsons: Vec<String> = result
        .additional_nodes
        .iter()
        .map(|n| serde_json::to_string(n).unwrap())
        .collect();
    assert!(node_jsons.iter().any(|j| j.contains("\"name\":\"P\"")));
}

#[test]
fn php_imports_coexist_with_other_extractions() {
    let result = index_source(
        "<?php\nuse App\\Models\\User;\n\nfunction helper() {}\n\nclass C {\n    function m() {}\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::UnresolvedSymbol));
    assert!(kinds.contains(&NodeKind::Function));
    assert!(kinds.contains(&NodeKind::Class));
}

#[test]
fn php_use_function_sets_expected_kind_function() {
    // `use function Foo\bar;` — the `type` field on
    // namespace_use_declaration is set to `function`. The
    // placeholder's `expected_kind` must reflect this so a future
    // kind-aware linker can prefer Function nodes when resolving.
    let result = index_source(
        "<?php\nuse function Foo\\bar;\n",
    );
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"bar\""), "node: {node_json}");
    assert!(
        node_json.contains("\"expected_kind\":\"function\""),
        "node: {node_json}"
    );
}

#[test]
fn php_use_const_sets_expected_kind_global_variable() {
    // `use const Foo\BAZ;` — the `type` field is `const`. PHP
    // constants live at top-level scope so the schema kind is
    // GlobalVariable.
    let result = index_source(
        "<?php\nuse const Foo\\BAZ;\n",
    );
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"BAZ\""), "node: {node_json}");
    assert!(
        node_json.contains("\"expected_kind\":\"global_variable\""),
        "node: {node_json}"
    );
}

#[test]
fn php_plain_use_leaves_expected_kind_none() {
    // Plain `use Foo\Bar;` could target a Class, Interface, Trait,
    // or Enum — PHP doesn't constrain it. The placeholder's
    // expected_kind stays None to be honest about the ambiguity.
    let result = index_source(
        "<?php\nuse Foo\\Bar;\n",
    );
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    // serde with `Option::None` skips the field entirely (or
    // renders it as null depending on serde_json settings). Either
    // way, the JSON must NOT carry a concrete kind.
    assert!(!node_json.contains("\"expected_kind\":\"function\""));
    assert!(!node_json.contains("\"expected_kind\":\"global_variable\""));
    assert!(!node_json.contains("\"expected_kind\":\"class\""));
}

#[test]
fn php_multiple_flat_clauses_each_emit_a_placeholder() {
    // `use App\Models\User, App\Models\Post;` — multiple
    // namespace_use_clause children at the top level of a
    // namespace_use_declaration, no group syntax. The walker must
    // collect all of them rather than just the first.
    let result = index_source(
        "<?php\nuse App\\Models\\User, App\\Models\\Post;\n",
    );
    let n_unresolved = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 2);
    let edge_jsons: Vec<String> = result
        .additional_edges
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(edge_jsons
        .iter()
        .any(|j| j.contains("\"import_path\":\"App\\\\Models\\\\User\"")));
    assert!(edge_jsons
        .iter()
        .any(|j| j.contains("\"import_path\":\"App\\\\Models\\\\Post\"")));
}
