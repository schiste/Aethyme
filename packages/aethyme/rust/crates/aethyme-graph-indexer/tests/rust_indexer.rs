//! Integration tests for the Rust indexer.

use aethyme_graph_indexer::{
    index_repo_to_disk, IndexerContext, LanguageIndexer, RustIndexer,
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
    write(tmp.path(), "src/lib.rs", content.as_bytes());
    let walked = aethyme_graph_indexer::walk_source_tree(
        &ctx(tmp.path()),
        &WalkOptions::default(),
    )
    .unwrap();
    let indexed = walked
        .files
        .iter()
        .find(|f| &*f.source_path == "src/lib.rs")
        .expect("indexed file missing");
    let indexer = RustIndexer::new();
    indexer
        .index_file(&ctx(tmp.path()), indexed, content)
        .unwrap()
}

// ─── Function / Struct / Enum extraction ────────────────────────────

#[test]
fn extracts_top_level_function() {
    let result = index_source(
        "pub fn hello(name: &str) -> String { format!(\"hi {}\", name) }\n",
    );
    let functions: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
}

#[test]
fn extracts_struct() {
    let result = index_source(
        "pub struct User {\n    pub id: u64,\n    pub name: String,\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Struct));
}

#[test]
fn extracts_enum() {
    let result = index_source(
        "pub enum Color {\n    Red,\n    Green,\n    Blue,\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Enum));
}

#[test]
fn extracts_trait_with_methods() {
    let result = index_source(
        "pub trait Greet {\n    fn hello(&self) -> String;\n    fn goodbye(&self) -> String;\n}\n",
    );
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Trait));
    // Trait + 2 Methods = 3 nodes
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Method).count(), 2);
}

#[test]
fn extracts_type_alias() {
    let result = index_source("pub type UserId = u64;\n");
    let kinds: Vec<NodeKind> =
        result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::TypeAlias));
}

// ─── Global variables: const + static ───────────────────────────────

#[test]
fn extracts_const_and_static_as_global_variables() {
    let result = index_source(
        "pub const PI: f64 = 3.14;\nstatic GLOBAL: u32 = 0;\n",
    );
    let globals: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::GlobalVariable)
        .collect();
    assert_eq!(globals.len(), 2);
}

// ─── Visibility heuristic ───────────────────────────────────────────

#[test]
fn pub_keyword_maps_to_public_visibility() {
    let result = index_source("pub fn foo() {}\n");
    let json =
        serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"public\""),
        "expected public, got: {json}"
    );
}

#[test]
fn no_pub_keyword_maps_to_module_visibility() {
    let result = index_source("fn foo() {}\n");
    let json =
        serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"module\""),
        "expected module, got: {json}"
    );
}

#[test]
fn pub_crate_maps_to_module_visibility() {
    let result = index_source("pub(crate) fn foo() {}\n");
    let json =
        serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"module\""),
        "expected module, got: {json}"
    );
}

// ─── impl block handling ────────────────────────────────────────────

#[test]
fn impl_block_methods_emit_as_functions_in_v1() {
    let result = index_source(
        "pub struct Foo;\n\nimpl Foo {\n    pub fn new() -> Self { Foo }\n    pub fn bar(&self) -> u32 { 1 }\n}\n",
    );
    let functions: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Function)
        .collect();
    // 2 impl methods emit as Functions
    assert_eq!(functions.len(), 2);
}

// ─── async functions ────────────────────────────────────────────────

#[test]
fn async_fn_signature_includes_async_keyword() {
    let result = index_source(
        "pub async fn fetch() -> Result<String, String> { Ok(String::new()) }\n",
    );
    let func = &result.additional_nodes[0];
    let json = serde_json::to_string(func).unwrap();
    assert!(
        json.contains("\"signature\":\"async fn fetch"),
        "expected async fn in signature, got: {json}"
    );
}

// ─── End-to-end pipeline ────────────────────────────────────────────

#[test]
fn end_to_end_rust_file_produces_enriched_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        b"pub struct Foo;\n\npub trait Greet {\n    fn hello(&self);\n}\n\npub fn run() {}\n",
    );

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 1);

    let frag = read_fragment(tmp.path(), "src/lib.rs").unwrap();
    // File + Struct + Trait + Method + Function = 5 nodes
    assert_eq!(frag.node_count(), 5);
}

#[test]
fn end_to_end_python_typescript_and_rust_indexed_separately() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def foo():\n    pass\n");
    write(tmp.path(), "src/web.ts", b"function bar() { return 1; }\n");
    write(tmp.path(), "src/core.rs", b"pub fn baz() {}\n");

    let summary =
        index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    // 3 File nodes + 3 Function nodes
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&3));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Function), Some(&3));
}

#[test]
fn rust_indexing_determinism() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        b"pub fn f() {}\npub struct S;\npub enum E { A, B }\n",
    );

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_a =
        std::fs::read(tmp.path().join(".aethyme/graph/src/lib.rs.bin"))
            .unwrap();
    std::fs::remove_dir_all(tmp.path().join(".aethyme")).unwrap();
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_b =
        std::fs::read(tmp.path().join(".aethyme/graph/src/lib.rs.bin"))
            .unwrap();
    assert_eq!(bytes_a, bytes_b);
}
