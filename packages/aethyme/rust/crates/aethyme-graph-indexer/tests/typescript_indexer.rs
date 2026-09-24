//! Integration tests for the TypeScript / JavaScript indexer.

use aethyme_graph_indexer::{
    IndexerContext, LanguageIndexer, TypeScriptIndexer, WalkOptions, index_repo_to_disk,
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

fn index_source(rel: &str, content: &str) -> aethyme_graph_indexer::LanguageIndexResult {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), rel, content.as_bytes());
    let walked =
        aethyme_graph_indexer::walk_source_tree(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let indexed = walked
        .files
        .iter()
        .find(|f| &*f.source_path == rel)
        .expect("indexed file missing");
    let indexer = TypeScriptIndexer::new();
    indexer
        .index_file(&ctx(tmp.path()), indexed, content)
        .unwrap()
}

// ─── Function / Class / Method extraction ───────────────────────────

#[test]
fn extracts_top_level_function_declaration() {
    let result = index_source(
        "src/x.ts",
        "function hello(name: string): string { return `hi ${name}`; }\n",
    );
    assert_eq!(result.additional_nodes.len(), 1);
    assert_eq!(result.additional_nodes[0].kind(), NodeKind::Function);
}

#[test]
fn extracts_class_and_its_methods() {
    let result = index_source(
        "src/x.ts",
        "class Foo {\n  bar() { return 1; }\n  baz(x: number) { return x; }\n}\n",
    );
    // Class + 2 Methods
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Class).count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == NodeKind::Method).count(), 2);
    // 1 Contains + 2 Defines
    assert_eq!(result.additional_edges.len(), 3);
}

#[test]
fn extracts_static_method_marked_as_is_static() {
    let result = index_source(
        "src/x.ts",
        "class Foo {\n  static helper() { return 1; }\n}\n",
    );
    let method = result
        .additional_nodes
        .iter()
        .find(|n| n.kind() == NodeKind::Method)
        .unwrap();
    let json = serde_json::to_string(method).unwrap();
    assert!(json.contains("\"is_static\":true"));
}

// ─── TS-specific kinds ──────────────────────────────────────────────

#[test]
fn extracts_interface() {
    let result = index_source(
        "src/x.ts",
        "interface User {\n  id: number;\n  name: string;\n}\n",
    );
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Interface));
}

#[test]
fn extracts_type_alias() {
    let result = index_source("src/x.ts", "type UserId = number;\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::TypeAlias));
}

#[test]
fn extracts_enum() {
    let result = index_source("src/x.ts", "enum Color { Red, Green, Blue }\n");
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Enum));
}

// ─── Variable declarations ──────────────────────────────────────────

#[test]
fn extracts_const_let_var_as_global_variables() {
    let result = index_source(
        "src/x.ts",
        "const PI = 3.14;\nlet count = 0;\nvar legacy = true;\n",
    );
    let globals: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::GlobalVariable)
        .collect();
    assert_eq!(globals.len(), 3);
}

#[test]
fn destructuring_pattern_skipped_in_v1() {
    // const { a, b } = obj; — not a simple identifier binding,
    // so v1 skips it.
    let result = index_source(
        "src/x.ts",
        "const obj = {a: 1, b: 2};\nconst { a, b } = obj;\n",
    );
    let globals: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::GlobalVariable)
        .collect();
    // Only `obj` should be captured; the destructured `a` and `b`
    // are skipped.
    assert_eq!(globals.len(), 1);
}

// ─── File extension handling ────────────────────────────────────────

#[test]
fn handles_tsx_extension_with_jsx() {
    let result = index_source(
        "src/component.tsx",
        "function Button() { return <button>Click</button>; }\n",
    );
    let functions: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::Function)
        .collect();
    assert_eq!(functions.len(), 1);
}

#[test]
fn handles_javascript_files() {
    let result = index_source(
        "src/x.js",
        "function hello() { return 'hi'; }\nclass Foo { bar() {} }\n",
    );
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::Function));
    assert!(kinds.contains(&NodeKind::Class));
}

// ─── End-to-end pipeline ────────────────────────────────────────────

#[test]
fn end_to_end_typescript_file_produces_enriched_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/component.ts",
        b"interface Props { id: number; }\n\
          class Component {\n  render(props: Props) { return null; }\n}\n",
    );

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(summary.total_files, 1);

    let frag = read_fragment(tmp.path(), "src/component.ts").unwrap();
    // File + Interface + Class + Method = 4 nodes
    assert_eq!(frag.node_count(), 4);
}

#[test]
fn end_to_end_mixed_python_and_typescript_indexed_separately() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def foo():\n    pass\n");
    write(tmp.path(), "src/web.ts", b"function bar() { return 1; }\n");

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    // Each file has File + Function = 2 nodes; total 4 nodes.
    assert_eq!(summary.counts_by_kind.get(&NodeKind::File), Some(&2));
    assert_eq!(summary.counts_by_kind.get(&NodeKind::Function), Some(&2));
}

#[test]
fn end_to_end_determinism_for_typescript() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/x.ts",
        b"function f() {}\nclass C { m() {} }\ninterface I {}\n",
    );

    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_a = std::fs::read(tmp.path().join(".aethyme/graph/src/x.ts.bin")).unwrap();
    std::fs::remove_dir_all(tmp.path().join(".aethyme")).unwrap();
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_b = std::fs::read(tmp.path().join(".aethyme/graph/src/x.ts.bin")).unwrap();
    assert_eq!(bytes_a, bytes_b);
}

// ─── Phase 4.6 stage 1: import extraction ───────────────────────────

#[test]
fn ts_default_import_emits_placeholder() {
    let result = index_source("src/x.ts", "import React from 'react';\n");
    let n_unresolved = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(n_unresolved, 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(
        node_json.contains("\"name\":\"React\""),
        "node: {node_json}"
    );
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"react\""));
    assert!(edge_json.contains("\"is_default\":true"));
    assert!(edge_json.contains("\"is_named\":false"));
}

#[test]
fn ts_named_import_emits_placeholder_per_name() {
    let result = index_source(
        "src/x.ts",
        "import { useState, useEffect as ue } from 'react';\n",
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
    // useState: binding=useState, import_path=react::useState
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"react::useState\""))
    );
    // useEffect aliased to ue: binding=ue, import_path=react::useEffect
    assert!(
        edge_jsons
            .iter()
            .any(|j| j.contains("\"import_path\":\"react::useEffect\""))
    );
    // All named imports
    assert!(edge_jsons.iter().all(|j| j.contains("\"is_named\":true")));
    // Binding names visible in node JSON
    let node_names: Vec<String> = result
        .additional_nodes
        .iter()
        .map(|n| serde_json::to_string(n).unwrap())
        .collect();
    assert!(
        node_names
            .iter()
            .any(|j| j.contains("\"name\":\"useState\""))
    );
    assert!(node_names.iter().any(|j| j.contains("\"name\":\"ue\"")));
}

#[test]
fn ts_namespace_import_emits_module_placeholder() {
    let result = index_source("src/x.ts", "import * as fs from 'node:fs';\n");
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"fs\""), "node: {node_json}");
    assert!(node_json.contains("\"expected_kind\":\"module\""));
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"is_namespace\":true"));
}

#[test]
fn ts_side_effect_import_emits_one_placeholder() {
    let result = index_source("src/x.ts", "import 'react-dom';\n");
    assert_eq!(result.additional_nodes.len(), 1);
    let node_json = serde_json::to_string(&result.additional_nodes[0]).unwrap();
    assert!(node_json.contains("\"name\":\"react-dom\""));
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(edge_json.contains("\"import_path\":\"react-dom\""));
    assert!(edge_json.contains("\"is_namespace\":true"));
}

#[test]
fn ts_relative_import_path_preserved() {
    let result = index_source("src/x.ts", "import { Helper } from './util/helper';\n");
    let edge_json = serde_json::to_string(&result.additional_edges[0]).unwrap();
    assert!(
        edge_json.contains("\"import_path\":\"./util/helper::Helper\""),
        "edge: {edge_json}"
    );
}

#[test]
fn ts_imports_coexist_with_other_extractions() {
    let result = index_source(
        "src/x.ts",
        "import { X } from './x';\n\nfunction f() {}\n\nclass C {}\n",
    );
    let kinds: Vec<NodeKind> = result.additional_nodes.iter().map(|n| n.kind()).collect();
    assert!(kinds.contains(&NodeKind::UnresolvedSymbol));
    assert!(kinds.contains(&NodeKind::Function));
    assert!(kinds.contains(&NodeKind::Class));
}

// ─── Export-wrapped declarations ────────────────────────────────────

/// `(kind, name, start_line, end_line)` for every defined node,
/// sorted, so a fixture's expectations read as a table.
fn definitions(
    result: &aethyme_graph_indexer::LanguageIndexResult,
) -> Vec<(NodeKind, String, u32, u32)> {
    let mut out: Vec<_> = result
        .additional_nodes
        .iter()
        .filter(|n| n.kind() != NodeKind::UnresolvedSymbol)
        .map(|n| {
            let range = n.source_range().expect("definition has a source range");
            (
                n.kind(),
                n.name().expect("definition has a name").to_string(),
                range.start_line(),
                range.end_line(),
            )
        })
        .collect();
    out.sort_by(|a, b| (a.2, &a.1).cmp(&(b.2, &b.1)));
    out
}

/// Serialized nodes and edges, sorted, for byte-for-byte comparison
/// of an exported source with its bare equivalent.
fn serialized(result: &aethyme_graph_indexer::LanguageIndexResult) -> (Vec<String>, Vec<String>) {
    let mut nodes: Vec<String> = result
        .additional_nodes
        .iter()
        .map(|n| serde_json::to_string(n).unwrap())
        .collect();
    let mut edges: Vec<String> = result
        .additional_edges
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    nodes.sort();
    edges.sort();
    (nodes, edges)
}

const EXPORTED_TS: &str = "\
export function f(a: number) {
  return a;
}
export class C {
  m() { return 1; }
}
export const x = 1, y = 2;
export interface I {
  a: string;
}
export type T = string;
export enum E { A, B }
export default function g() {
  return 0;
}
";

#[test]
fn ts_export_wrapped_declarations_are_indexed_at_their_lines() {
    let result = index_source("src/x.ts", EXPORTED_TS);
    let expected = vec![
        (NodeKind::Function, "f".to_string(), 1, 3),
        (NodeKind::Class, "C".to_string(), 4, 6),
        (NodeKind::Method, "m".to_string(), 5, 5),
        (NodeKind::GlobalVariable, "x".to_string(), 7, 7),
        (NodeKind::GlobalVariable, "y".to_string(), 7, 7),
        (NodeKind::Interface, "I".to_string(), 8, 10),
        (NodeKind::TypeAlias, "T".to_string(), 11, 11),
        (NodeKind::Enum, "E".to_string(), 12, 12),
        (NodeKind::Function, "g".to_string(), 13, 15),
    ];
    assert_eq!(definitions(&result), expected);
}

#[test]
fn ts_export_wrapped_declarations_match_their_bare_forms() {
    let bare = EXPORTED_TS
        .replace("export default ", "")
        .replace("export ", "");
    assert_eq!(
        serialized(&index_source("src/x.ts", EXPORTED_TS)),
        serialized(&index_source("src/x.ts", &bare)),
    );
}

#[test]
fn ts_export_default_class_interface_and_named_expressions_are_indexed() {
    let cases = [
        (
            "export default class D {\n  run() {}\n}\n",
            vec![
                (NodeKind::Class, "D".to_string(), 1, 3),
                (NodeKind::Method, "run".to_string(), 2, 2),
            ],
        ),
        (
            "export default interface DI {\n  a: string;\n}\n",
            vec![(NodeKind::Interface, "DI".to_string(), 1, 3)],
        ),
        (
            "export default (function h() {\n  return 1;\n});\n",
            vec![(NodeKind::Function, "h".to_string(), 1, 3)],
        ),
        (
            "export default (class K {});\n",
            vec![(NodeKind::Class, "K".to_string(), 1, 1)],
        ),
    ];
    for (source, expected) in cases {
        let result = index_source("src/x.ts", source);
        assert_eq!(definitions(&result), expected, "source: {source}");
    }
}

#[test]
fn ts_anonymous_default_exports_and_re_exports_define_nothing() {
    for source in [
        "export default function () {}\n",
        "export default class {}\n",
        "export default 42;\n",
        "const a = 1;\nexport { a };\n",
        "export { b } from './b';\n",
        "export * from './c';\n",
        "export * as ns from './d';\n",
    ] {
        let result = index_source("src/x.ts", source);
        let defined: Vec<_> = definitions(&result)
            .into_iter()
            .filter(|(_, name, _, _)| name != "a")
            .collect();
        assert!(defined.is_empty(), "source {source:?} defined {defined:?}");
    }
}

#[test]
fn js_export_wrapped_declarations_are_indexed_at_their_lines() {
    let source = "\
export function f() {
  return 1;
}
export class C {
  m() {}
}
export const x = 1;
export let y;
export default class D {
}
";
    let result = index_source("src/x.js", source);
    let expected = vec![
        (NodeKind::Function, "f".to_string(), 1, 3),
        (NodeKind::Class, "C".to_string(), 4, 6),
        (NodeKind::Method, "m".to_string(), 5, 5),
        (NodeKind::GlobalVariable, "x".to_string(), 7, 7),
        (NodeKind::GlobalVariable, "y".to_string(), 8, 8),
        (NodeKind::Class, "D".to_string(), 9, 10),
    ];
    assert_eq!(definitions(&result), expected);
    let bare = source.replace("export default ", "").replace("export ", "");
    assert_eq!(
        serialized(&result),
        serialized(&index_source("src/x.js", &bare)),
    );
}
