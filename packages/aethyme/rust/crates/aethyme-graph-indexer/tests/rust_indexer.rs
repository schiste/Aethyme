//! Integration tests for the Rust indexer.

use aethyme_graph_indexer::{
    IndexerContext, LanguageIndexer, RustIndexer, WalkOptions, index_repo_to_disk,
};
use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::read_fragment;

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0").unwrap()
}

fn index_source(content: &str) -> aethyme_graph_indexer::LanguageIndexResult {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/lib.rs", content.as_bytes());
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
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
    let result = index_source("pub fn hello(name: &str) -> String { format!(\"hi {}\", name) }\n");
    let functions: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
}

#[test]
fn extracts_struct() {
    let result = index_source("pub struct User {\n    pub id: u64,\n    pub name: String,\n}\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Struct));
}

#[test]
fn extracts_enum() {
    let result = index_source("pub enum Color {\n    Red,\n    Green,\n    Blue,\n}\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Enum));
}

#[test]
fn extracts_trait_with_methods() {
    let result = index_source(
        "pub trait Greet {\n    fn hello(&self) -> String;\n    fn goodbye(&self) -> String;\n}\n",
    );
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Trait));
    // Trait + 2 Methods = 3 nodes
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Method).count(), 2);
}

#[test]
fn extracts_type_alias() {
    let result = index_source("pub type UserId = u64;\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::TypeAlias));
}

// ─── Global variables: const + static ───────────────────────────────

#[test]
fn extracts_const_and_static_as_global_variables() {
    let result = index_source("pub const PI: f64 = 3.14;\nstatic GLOBAL: u32 = 0;\n");
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
    let json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"public\""),
        "expected public, got: {json}"
    );
}

#[test]
fn no_pub_keyword_maps_to_module_visibility() {
    let result = index_source("fn foo() {}\n");
    let json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"module\""),
        "expected module, got: {json}"
    );
}

#[test]
fn pub_crate_maps_to_module_visibility() {
    let result = index_source("pub(crate) fn foo() {}\n");
    let json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
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
    let result =
        index_source("pub async fn fetch() -> Result<String, String> { Ok(String::new()) }\n");
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

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
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

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
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
    let bytes_a = std::fs::read(tmp.path().join(".aethyme/graph/src/lib.rs.bin")).unwrap();
    std::fs::remove_dir_all(tmp.path().join(".aethyme")).unwrap();
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_b = std::fs::read(tmp.path().join(".aethyme/graph/src/lib.rs.bin")).unwrap();
    assert_eq!(bytes_a, bytes_b);
}

// ─── Phase 4.6 stage 1: import extraction ───────────────────────────

#[test]
fn rust_plain_use_emits_placeholder_for_last_segment() {
    let result = index_source("use std::collections::HashMap;\n");
    let n_unresolved = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    // Binding is the last segment: HashMap.
    assert!(
        node_json.contains("\"name\":\"HashMap\""),
        "node: {node_json}"
    );
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(
        edge_json.contains("\"import_path\":\"std::collections::HashMap\""),
        "edge: {edge_json}"
    );
    assert!(edge_json.contains("\"is_named\":true"));
    assert!(edge_json.contains("\"is_namespace\":false"));
}

#[test]
fn rust_aliased_use_uses_alias_as_binding() {
    let result = index_source("use std::collections::HashMap as HM;\n");
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"HM\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    // import_path keeps the original (unaliased) path.
    assert!(edge_json.contains("\"import_path\":\"std::collections::HashMap\""));
}

#[test]
fn rust_use_group_flattens_into_multiple_placeholders() {
    let result = index_source("use std::collections::{HashMap, BTreeMap, HashSet as HS};\n");
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
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"std::collections::HashMap\""))
    );
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"std::collections::BTreeMap\""))
    );
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"std::collections::HashSet\""))
    );
    // The aliased binding lives on the placeholder node.
    let node_jsons: Vec<String> = result
        .additional_nodes
        .iter()
        .map(|n| serde_json::to_string(n).unwrap())
        .collect();
    assert!(node_jsons.iter().any(|j| j.contains("\"name\":\"HS\"")));
}

#[test]
fn rust_use_glob_marks_namespace_with_star_binding() {
    let result = index_source("use std::collections::*;\n");
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"*\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"std::collections::*\""));
    assert!(edge_json.contains("\"is_namespace\":true"));
}

#[test]
fn rust_nested_use_group_recurses() {
    // `use a::{b::c, b::d as e};` → flattens to a::b::c and a::b::d.
    let result = index_source("use a::{b::c, b::d as e};\n");
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
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"a::b::c\""))
    );
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"a::b::d\""))
    );
}

#[test]
fn rust_use_crate_root_path_preserved() {
    // `use crate::foo::bar;` — leading `crate::` is part of the path.
    let result = index_source("use crate::foo::bar;\n");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(
        edge_json.contains("\"import_path\":\"crate::foo::bar\""),
        "edge: {edge_json}"
    );
}

#[test]
fn rust_imports_coexist_with_other_extractions() {
    let result = index_source("use std::collections::HashMap;\n\nfn helper() {}\n\nstruct S;\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::UnresolvedSymbol));
    assert!(kinds.contains(&NodeKind::Function));
    assert!(kinds.contains(&NodeKind::Struct));
}
