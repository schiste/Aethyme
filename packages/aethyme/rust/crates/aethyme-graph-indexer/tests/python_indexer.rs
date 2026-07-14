//! Integration tests for the Python language indexer.

use aethyme_graph_indexer::{
    IndexerContext, LanguageIndexer, PythonIndexer, WalkOptions, index_repo_to_disk,
};
use aethyme_graph_schema::{EdgeKind, NodeKind, Visibility};
use aethyme_graph_storage::read_fragment;

fn write(root: &std::path::Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0").unwrap()
}

// ─── Direct indexer tests (unit-style, in-memory) ───────────────────

fn index_source(content: &str) -> aethyme_graph_indexer::LanguageIndexResult {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/x.py", content.as_bytes());
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let indexed = walked
        .files
        .iter()
        .find(|f| &*f.source_path == "src/x.py")
        .expect("indexed file missing");
    let indexer = PythonIndexer::new();
    indexer
        .index_file(&ctx(tmp.path()), indexed, content)
        .unwrap()
}

#[test]
fn extracts_top_level_function() {
    let result = index_source("def hello(name):\n    return f'hi {name}'\n");
    assert_eq!(result.additional_nodes.len(), 1);
    assert_eq!(result.additional_edges.len(), 1);
    assert_eq!(result.additional_nodes[0].kind(), NodeKind::Function);
    assert_eq!(result.additional_edges[0].kind(), EdgeKind::Contains);
}

#[test]
fn extracts_top_level_class_and_its_methods() {
    let result = index_source(
        "class Foo:\n\
         \x20   def bar(self):\n\
         \x20       return 1\n\
         \x20   def baz(self, x):\n\
         \x20       return x\n",
    );
    // Class + 2 methods = 3 nodes; Contains(file→class) + 2×Defines(class→method) = 3 edges
    assert_eq!(result.additional_nodes.len(), 3);
    assert_eq!(result.additional_edges.len(), 3);
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Class));
    assert!(kinds.contains(&NodeKind::Method));
    let methods: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Method)
        .collect();
    assert_eq!(methods.len(), 2);
}

#[test]
fn extracts_global_variables() {
    let result = index_source(
        "VERSION = '1.0'\n\
         COUNT: int = 0\n\
         \n\
         def fn():\n    pass\n",
    );
    let globals: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::GlobalVariable)
        .collect();
    assert_eq!(globals.len(), 2);
}

#[test]
fn private_name_convention_maps_to_private_visibility() {
    // We can't pull Visibility off the Node enum directly (no public
    // accessor), so we serialize and check the JSON shape contains
    // the expected visibility.
    let result = index_source("def _internal():\n    pass\n");
    let json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        json.contains("\"visibility\":\"private\""),
        "expected private visibility, got JSON: {json}"
    );
}

#[test]
fn dunder_method_name_maps_to_public_visibility() {
    let result = index_source("class Foo:\n    def __init__(self):\n        pass\n");
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(json.contains("\"visibility\":\"public\""));
}

#[test]
fn staticmethod_decorator_marks_is_static() {
    let result = index_source(
        "class Foo:\n\
         \x20   @staticmethod\n\
         \x20   def bar():\n\
         \x20       pass\n",
    );
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(
        json.contains("\"is_static\":true"),
        "expected is_static=true, JSON: {json}"
    );
}

#[test]
fn async_function_signature_carries_async_keyword() {
    let result = index_source("async def fetch():\n    pass\n");
    let function = &result.additional_nodes[0];
    let json = serde_json::to_string(function).unwrap();
    assert!(json.contains("\"signature\":\"async def fetch()\""));
}

#[test]
fn parse_error_returns_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/x.py", b"def broken(:\n    pass\n");
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let indexed = walked.files.first().unwrap();
    let indexer = PythonIndexer::new();
    let err = indexer
        .index_file(&ctx(tmp.path()), indexed, "def broken(:\n    pass\n")
        .unwrap_err();
    assert!(matches!(
        err,
        aethyme_graph_indexer::LanguageIndexError::Parse { .. }
    ));
}

#[test]
fn function_has_correct_line_range() {
    let result = index_source("# header\ndef target():\n    return 1\n");
    let function = &result.additional_nodes[0];
    let json = serde_json::to_string(function).unwrap();
    // The def statement starts on line 2; range expectation is
    // start_line=2 (def is on line 2, after the comment).
    assert!(
        json.contains("\"start_line\":2"),
        "expected start_line=2, JSON: {json}"
    );
}

// ─── End-to-end pipeline tests ──────────────────────────────────────

#[test]
fn end_to_end_python_file_produces_enriched_fragment_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/cli.py",
        b"def explore_command():\n    return 'hi'\n\nclass Helper:\n    def run(self):\n        pass\n",
    );

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 1);
    // File + Function + Class + Method = 4 nodes
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&1));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Function), Some(&1));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Class), Some(&1));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Method), Some(&1));

    let frag = read_fragment(tmp.path(), "src/cli.py").unwrap();
    assert_eq!(frag.node_count(), 4);
    // Edges: Contains(file→fn), Contains(file→class), Defines(class→method)
    assert_eq!(frag.edge_count(), 3);
}

#[test]
fn non_python_files_are_untouched_by_python_indexer() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", b"# heading\n");
    write(tmp.path(), "src/lib.rs", b"fn main() {}\n");

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    // lib.rs gets a File node from the filesystem walker, plus
    // (now that the Rust indexer is registered) a Function node
    // for `fn main`. The test name still applies: the Python
    // indexer doesn't touch this file. We verify the rust-derived
    // nodes are NOT Method (the only Python-specific node the
    // indexer would emit for trivial input).
    let rs_frag = read_fragment(tmp.path(), "src/lib.rs").unwrap();
    let rs_kinds: Vec<NodeKind> = rs_frag.nodes().iter().map(|n| n.kind()).collect();
    assert!(rs_kinds.contains(&NodeKind::File));
    assert!(
        !rs_kinds.contains(&NodeKind::Method),
        "Python's Method node leaked into Rust file: kinds={rs_kinds:?}"
    );

    let md_frag = read_fragment(tmp.path(), "README.md").unwrap();
    assert_eq!(md_frag.node_count(), 1);
    assert_eq!(md_frag.nodes()[0].kind(), NodeKind::NonCodeFile);

    // Sanity: summary should report counts including the File / NonCodeFile.
    assert!(summary.counts_by_kind.contains_key(&NodeKind::File));
    assert!(summary.counts_by_kind.contains_key(&NodeKind::NonCodeFile));
}

#[test]
fn parallel_indexing_produces_correct_results_for_many_files() {
    // Stress test for the rayon parallel file loop. Creates many
    // Python files, runs the pipeline, asserts every file has the
    // expected node count after indexing. With rayon enabled this
    // exercises concurrent reads + parses + writes; without rayon
    // it's a sanity sweep.
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..50 {
        let content = format!(
            "def f{i}():\n    return {i}\n\nclass C{i}:\n    def m{i}(self):\n        pass\n",
        );
        write(tmp.path(), &format!("src/mod{i}.py"), content.as_bytes());
    }

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 50);

    // Each file should have: 1 File + 1 Function + 1 Class + 1 Method = 4 nodes.
    // 50 files × 4 = 200 nodes total.
    let total_nodes: usize = summary.counts_by_kind.values().sum();
    assert_eq!(total_nodes, 200);
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&50));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Function), Some(&50));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Class), Some(&50));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Method), Some(&50));

    // Spot-check a few file fragments to confirm correct decoding.
    for i in [0, 17, 49] {
        let frag = read_fragment(tmp.path(), &format!("src/mod{i}.py")).unwrap();
        assert_eq!(frag.node_count(), 4);
    }
}

#[test]
fn parallel_indexing_is_deterministic_across_runs() {
    // The rayon parallel loop must still produce byte-identical
    // output across runs (thread scheduling order must not leak
    // into the on-disk bytes).
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..20 {
        let content =
            format!("def f{i}():\n    pass\n\nclass C{i}:\n    def m(self):\n        pass\n",);
        write(tmp.path(), &format!("src/a{i}.py"), content.as_bytes());
    }

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let snapshots_a: Vec<Vec<u8>> = (0..20)
        .map(|i| std::fs::read(tmp.path().join(format!(".aethyme/graph/src/a{i}.py.bin"))).unwrap())
        .collect();

    // Clear and re-run.
    std::fs::remove_dir_all(tmp.path().join(".aethyme")).unwrap();

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let snapshots_b: Vec<Vec<u8>> = (0..20)
        .map(|i| std::fs::read(tmp.path().join(format!(".aethyme/graph/src/a{i}.py.bin"))).unwrap())
        .collect();

    assert_eq!(snapshots_a, snapshots_b);
}

#[test]
fn end_to_end_determinism_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/cli.py",
        b"def f():\n    pass\n\nclass G:\n    def m(self):\n        pass\n",
    );

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let first = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let second = std::fs::read(tmp.path().join(".aethyme/graph/src/cli.py.bin")).unwrap();
    assert_eq!(first, second);

    // Sanity check: Visibility::Public exists (compile-time check that the import is used).
    let _ = Visibility::Public;
}

// ─── Phase 4.4: import-statement extraction ─────────────────────────
//
// Each `import` / `from … import …` becomes one UnresolvedSymbol per
// imported name plus an Imports edge from the file node. The
// placeholder records the *binding* name (so callers in the same
// file can later resolve `np.foo()` back through it); the edge's
// `import_path` records the source-side dotted path.

#[test]
fn plain_import_emits_unresolved_and_imports_edge() {
    let result = index_source("import numpy\n");
    assert_eq!(result.additional_nodes.len(), 1);
    assert_eq!(result.additional_edges.len(), 1);
    let node = &result.additional_nodes[0];
    assert_eq!(node.kind(), NodeKind::UnresolvedSymbol);
    let json = serde_json::to_string(node).unwrap();
    assert!(json.contains("\"name\":\"numpy\""), "node: {json}");
    assert!(
        json.contains("\"expected_kind\":\"module\""),
        "node: {json}"
    );
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"numpy\""));
    assert!(edge_json.contains("\"is_namespace\":true"));
    assert!(edge_json.contains("\"is_named\":false"));
    assert_eq!(result.additional_edges[0].kind(), EdgeKind::Imports);
}

#[test]
fn aliased_import_uses_binding_name() {
    let result = index_source("import numpy as np\n");
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    // Binding name is what other code in the file references.
    assert!(node_json.contains("\"name\":\"np\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    // Source path is the unaliased module name.
    assert!(edge_json.contains("\"import_path\":\"numpy\""));
}

#[test]
fn dotted_import_binds_top_segment_path_keeps_full_dotted_form() {
    // `import foo.bar.baz` — Python binds only `foo` in the local
    // namespace (subsequent code reaches `foo.bar.baz` via attribute
    // access on `foo`). The placeholder name must therefore be `foo`
    // so the linker pass can later connect `foo.<...>` references
    // back to the import. The edge's `import_path` carries the full
    // dotted source-side path for the linker to follow.
    let result = index_source("import foo.bar.baz\n");
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"foo\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"foo.bar.baz\""));
}

#[test]
fn aliased_dotted_import_uses_alias_for_binding() {
    // `import foo.bar as fb` — the alias is the binding name.
    let result = index_source("import foo.bar as fb\n");
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"fb\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"foo.bar\""));
}

#[test]
fn from_import_emits_one_placeholder_per_name() {
    let result = index_source("from os import path, environ\n");
    assert_eq!(result.additional_nodes.len(), 2);
    assert_eq!(result.additional_edges.len(), 2);
    let names: Vec<String> = result
        .additional_nodes
        .iter()
        .map(|n| {
            let j = serde_json::to_string(n).unwrap();
            j.find("\"name\":\"")
                .map(|i| {
                    let rest = &j[i + 8..];
                    rest[..rest.find('"').unwrap()].to_string()
                })
                .unwrap()
        })
        .collect();
    assert!(names.contains(&"path".to_string()));
    assert!(names.contains(&"environ".to_string()));
    let edge_jsons: Vec<String> = result
        .additional_edges
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"os.path\""))
    );
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"os.environ\""))
    );
    // from-imports flip namespace flag off, named flag on.
    assert!(
        edge_jsons
            .iter()
            .all(|j| j.contains("\"is_namespace\":false"))
    );
    assert!(edge_jsons.iter().all(|j| j.contains("\"is_named\":true")));
}

#[test]
fn from_import_with_alias_records_binding() {
    let result = index_source("from foo import bar as b\n");
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"b\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"foo.bar\""));
}

#[test]
fn relative_import_prepends_dots_to_import_path() {
    // `from . import sibling` — level=1, no module.
    let result = index_source("from . import sibling\n");
    assert_eq!(result.additional_nodes.len(), 1);
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(
        edge_json.contains("\"import_path\":\".sibling\""),
        "edge: {edge_json}"
    );

    // `from ..pkg import x` — level=2, module="pkg".
    let result2 = index_source("from ..pkg import x\n");
    let edge2 = serde_json::to_string(&result2.additional_edges[0]).unwrap();
    assert!(
        edge2.contains("\"import_path\":\"..pkg.x\""),
        "edge: {edge2}"
    );
}

#[test]
fn star_import_emits_namespace_placeholder() {
    let result = index_source("from foo import *\n");
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"*\""), "node: {node_json}");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"foo.*\""));
    // Star-imports bring in everything → namespace, not named.
    assert!(edge_json.contains("\"is_namespace\":true"));
    assert!(edge_json.contains("\"is_named\":false"));
}

#[test]
fn imports_coexist_with_function_extraction() {
    // Verify imports don't disturb the existing Function/Class/etc.
    // extraction — both kinds of nodes should appear in one pass.
    let result = index_source("import os\nfrom sys import argv\n\ndef main():\n    return 1\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    let n_unresolved = kinds
        .iter()
        .filter(|&&k| k == NodeKind::UnresolvedSymbol)
        .count();
    let n_function = kinds.iter().filter(|&&k| k == NodeKind::Function).count();
    assert_eq!(n_unresolved, 2, "kinds = {kinds:?}");
    assert_eq!(n_function, 1, "kinds = {kinds:?}");

    let edge_kinds: Vec<EdgeKind> = result.additional_edges.iter().map(|e| e.kind()).collect();
    let n_imports = edge_kinds
        .iter()
        .filter(|&&k| k == EdgeKind::Imports)
        .count();
    let n_contains = edge_kinds
        .iter()
        .filter(|&&k| k == EdgeKind::Contains)
        .count();
    assert_eq!(n_imports, 2);
    // File → Function (Contains).
    assert_eq!(n_contains, 1);
}

#[test]
fn duplicate_imports_are_deduped_via_fragment_layer() {
    // Two identical `import os` lines produce two emitted placeholder
    // nodes with the same NodeId (referenced_from_id + binding name
    // are identical), which Fragment::new silently dedupes. The
    // end-to-end on-disk fragment should contain exactly one
    // UnresolvedSymbol for `os`.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/x.py", b"import os\nimport os\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let frag = read_fragment(tmp.path(), "src/x.py").unwrap();
    let n_unresolved = frag
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 1, "expected single deduped placeholder");
}

#[test]
fn imports_round_trip_through_fragment_on_disk() {
    // End-to-end: write a Python file with imports, run the
    // indexer, decode the binary fragment, and confirm the
    // UnresolvedSymbol nodes and Imports edges survived
    // serialization.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/cli.py",
        b"import json\nfrom typing import List\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let frag = read_fragment(tmp.path(), "src/cli.py").unwrap();

    let unresolved_count = frag
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    let imports_count = frag
        .edges()
        .iter()
        .filter(|e| e.kind() == EdgeKind::Imports)
        .count();
    assert_eq!(unresolved_count, 2);
    assert_eq!(imports_count, 2);
}
