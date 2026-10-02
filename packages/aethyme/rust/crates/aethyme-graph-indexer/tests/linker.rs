//! Integration tests for the Phase 4.5 linker pass.
//!
//! Each test indexes a tiny repo (via `index_repo_to_disk` with
//! `link_repo` disabled), then exercises `link_with_store` or
//! `link_repo` against the resulting on-disk graph and asserts the
//! invariants the linker is supposed to guarantee.

use std::path::Path;

use aethyme_graph_indexer::{
    IndexerContext, WalkOptions, index_repo_to_disk, link_repo, link_with_store,
};
use aethyme_graph_schema::{EdgeKind, NodeKind};
use aethyme_graph_storage::{FragmentStore, read_fragment};

fn write(root: &Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &Path) -> IndexerContext {
    IndexerContext::new("testrepo", repo_root.to_path_buf(), "0.1.0").unwrap()
}

// ─── Resolution cases ────────────────────────────────────────────────

#[test]
fn from_import_resolves_to_target_function() {
    // `from util import helper` in cli.py — util.py defines helper.
    // After link: both the Imports edge and the Calls edge in cli.py
    // point at the Function node in util.py, and placeholders are removed.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"from util import helper\n\ndef main():\n    return helper()\n",
    );
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 2);
    assert_eq!(summary.placeholders_resolved, 2);
    assert_eq!(summary.edges_rewritten, 2);
    assert_eq!(summary.orphans_removed, 2);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    // Placeholder is gone after resolution.
    let unresolved = cli_frag
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(unresolved, 0);

    // The Imports edge now points at util.py's helper Function.
    let util_frag = read_fragment(tmp.path(), "util.py").unwrap();
    let helper_id = util_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .expect("helper function should exist in util.py");
    let resolved_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .expect("Imports edge should survive");
    assert_eq!(resolved_edge.dst_id(), &helper_id);
    let call_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("Calls edge should survive");
    assert_eq!(call_edge.dst_id(), &helper_id);
    assert_eq!(call_edge.sites().len(), 1);
}

#[test]
fn call_edge_resolves_through_import_binding_when_name_is_ambiguous() {
    // Two modules define `helper`, so a whole-repo name-only call
    // lookup would be ambiguous. The call is still resolvable because
    // cli.py has an exact local import binding from util.helper.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"from util import helper\n\ndef main():\n    return helper()\n",
    );
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    write(tmp.path(), "other.py", b"def helper():\n    return 2\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 2);
    assert_eq!(summary.placeholders_resolved, 2);
    assert_eq!(summary.edges_rewritten, 2);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let util_frag = read_fragment(tmp.path(), "util.py").unwrap();
    let helper_id = util_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .expect("util helper function");
    let call_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("resolved call edge");
    assert_eq!(call_edge.dst_id(), &helper_id);
}

#[test]
fn same_module_call_resolves_to_function() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "service.py",
        b"def helper():\n    return 1\n\n\ndef main():\n    return helper()\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 1);
    assert_eq!(summary.placeholders_resolved, 1);
    assert_eq!(summary.edges_rewritten, 1);

    let frag = read_fragment(tmp.path(), "service.py").unwrap();
    let helper_id = frag
        .nodes()
        .iter()
        .find(|n| {
            n.kind() == NodeKind::Function
                && serde_json::to_string(n)
                    .unwrap()
                    .contains("\"name\":\"helper\"")
        })
        .map(|n| n.id().clone())
        .expect("helper function");
    let call_edge = frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("resolved call edge");
    assert_eq!(call_edge.dst_id(), &helper_id);
}

#[test]
fn namespace_import_call_resolves_through_import_binding() {
    // `util.helper()` is only resolvable because `util` is a local
    // namespace import binding. The linker must not need a repo-wide
    // dotted-name fallback for this common case.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"import util\n\ndef main():\n    return util.helper()\n",
    );
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 2);
    assert_eq!(summary.placeholders_resolved, 2);
    assert_eq!(summary.edges_rewritten, 2);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let util_frag = read_fragment(tmp.path(), "util.py").unwrap();
    let helper_id = util_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .expect("helper function");
    let call_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("resolved call edge");
    assert_eq!(call_edge.dst_id(), &helper_id);
}

#[test]
fn namespace_import_resolves_to_file_node() {
    // `import util` in cli.py — util.py exists.
    // The placeholder is named `util` (Python binds the top-level
    // segment); resolution should target util.py's File node.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"import util\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 1);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let util_frag = read_fragment(tmp.path(), "util.py").unwrap();
    let util_file_id = util_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::File)
        .map(|n| n.id().clone())
        .unwrap();
    let imports_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    assert_eq!(imports_edge.dst_id(), &util_file_id);
}

#[test]
fn dotted_namespace_import_resolves_to_nested_file() {
    // `import pkg.sub` in app.py — pkg/sub.py defines a function.
    // The binding is `pkg` (top segment); the import_path is
    // `pkg.sub`; the linker should resolve to pkg/sub.py's File
    // because that's the module the import points at.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "app.py", b"import pkg.sub\n");
    write(tmp.path(), "pkg/sub.py", b"def fn():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 1);

    let app_frag = read_fragment(tmp.path(), "app.py").unwrap();
    let sub_frag = read_fragment(tmp.path(), "pkg/sub.py").unwrap();
    let sub_file_id = sub_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::File)
        .map(|n| n.id().clone())
        .unwrap();
    let imports_edge = app_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    assert_eq!(imports_edge.dst_id(), &sub_file_id);
}

#[test]
fn from_import_with_dotted_module_resolves() {
    // `from pkg.sub import fn` → import_path = "pkg.sub.fn",
    // looking for symbol "fn" in module "pkg.sub".
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "app.py", b"from pkg.sub import fn\n");
    write(tmp.path(), "pkg/sub.py", b"def fn():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 1);

    let app_frag = read_fragment(tmp.path(), "app.py").unwrap();
    let sub_frag = read_fragment(tmp.path(), "pkg/sub.py").unwrap();
    let fn_id = sub_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .unwrap();
    let edge = app_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    assert_eq!(edge.dst_id(), &fn_id);
}

#[test]
fn from_import_class_resolves() {
    // `from app import App` where app.py defines class App.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "main.py", b"from app import App\n");
    write(
        tmp.path(),
        "app.py",
        b"class App:\n    def run(self):\n        pass\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    link_repo(&ctx(tmp.path())).unwrap();

    let main_frag = read_fragment(tmp.path(), "main.py").unwrap();
    let app_frag = read_fragment(tmp.path(), "app.py").unwrap();
    let class_id = app_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Class)
        .map(|n| n.id().clone())
        .unwrap();
    let edge = main_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    assert_eq!(edge.dst_id(), &class_id);
}

// ─── Non-resolution cases ────────────────────────────────────────────

#[test]
fn unknown_external_import_stays_unresolved() {
    // `import os` — no os.py in this repo. Placeholder must
    // survive; Imports edge must still point at it.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"import os\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 1);
    assert_eq!(summary.placeholders_resolved, 0);
    assert_eq!(summary.edges_rewritten, 0);
    assert_eq!(summary.orphans_removed, 0);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let unresolved = cli_frag
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(unresolved, 1);
    let edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    // Edge target is still the placeholder, which is still present.
    let placeholder_id = cli_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .map(|n| n.id())
        .unwrap();
    assert_eq!(edge.dst_id(), placeholder_id);
}

#[test]
fn ambiguous_name_only_match_stays_unresolved() {
    // Two files define a Function named "helper". `from somewhere
    // import helper` would be ambiguous if the linker fell back to
    // name-only matching. The conservative resolution policy must
    // leave the placeholder unresolved.
    //
    // To force the by_module_and_name fast path to MISS, use a
    // module name that doesn't exist (`shared` — not a real
    // module in this repo) but the by_name fallback would see
    // two helpers.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from shared import helper\n");
    write(tmp.path(), "a.py", b"def helper():\n    return 1\n");
    write(tmp.path(), "b.py", b"def helper():\n    return 2\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    // The ambiguous name-only fallback sees two `helper`s → conservative.
    assert_eq!(summary.placeholders_resolved, 0);
    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let unresolved = cli_frag
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(unresolved, 1);
}

#[test]
fn unimported_receiver_call_stays_unresolved() {
    // The receiver name `util` is a parameter, not an import binding.
    // Even though this repo has util.py::helper, a type-free linker
    // must not infer that `util.helper()` points there.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"def main(util):\n    return util.helper()\n",
    );
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_seen, 1);
    assert_eq!(summary.placeholders_resolved, 0);
    assert_eq!(summary.edges_rewritten, 0);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let placeholder_id = cli_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .map(|n| n.id())
        .expect("unresolved call placeholder");
    let call_edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("call edge should survive");
    assert_eq!(call_edge.dst_id(), placeholder_id);
}

#[test]
fn relative_import_stays_unresolved_for_now() {
    // `from . import sibling` — relative imports need the
    // importing file's package context. Phase 4.5 conservatively
    // leaves them unresolved; a later commit can extend the
    // resolver to use the importer's source_path as the relative
    // anchor.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from . import sibling\n");
    write(tmp.path(), "sibling.py", b"def fn():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 0);
}

// ─── Idempotence + invariants ────────────────────────────────────────

#[test]
fn link_is_idempotent_when_rerun() {
    // Running the linker twice on the same store must produce the
    // same on-disk bytes. After the first pass: resolved
    // placeholders are gone; unresolved ones stay. After the
    // second pass: no work to do.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"from util import helper\nimport os\n",
    );
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let first = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(first.placeholders_resolved, 1);
    let bytes_after_first = std::fs::read(tmp.path().join(".aethyme/graph/cli.py.bin")).unwrap();

    let second = link_repo(&ctx(tmp.path())).unwrap();
    // Only the unresolvable `os` placeholder remains; the second
    // pass does no work on it (it's still unresolvable).
    assert_eq!(second.placeholders_seen, 1);
    assert_eq!(second.edges_rewritten, 0);

    let bytes_after_second = std::fs::read(tmp.path().join(".aethyme/graph/cli.py.bin")).unwrap();
    assert_eq!(bytes_after_first, bytes_after_second);
}

#[test]
fn link_preserves_non_imports_edges_untouched() {
    // The linker must not touch Contains / Defines / etc. edges.
    // util.py has a top-level function and a class with a method —
    // those produce Contains + Defines edges that must survive
    // unchanged.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "util.py",
        b"def helper():\n    return 1\n\nclass C:\n    def m(self):\n        pass\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let before = read_fragment(tmp.path(), "util.py").unwrap();
    let before_edges: Vec<_> = before
        .edges()
        .iter()
        .map(|e| (e.src_id().clone(), e.dst_id().clone(), e.kind()))
        .collect();

    link_repo(&ctx(tmp.path())).unwrap();
    let after = read_fragment(tmp.path(), "util.py").unwrap();
    let after_edges: Vec<_> = after
        .edges()
        .iter()
        .map(|e| (e.src_id().clone(), e.dst_id().clone(), e.kind()))
        .collect();
    assert_eq!(before_edges, after_edges);
}

#[test]
fn link_summary_counts_are_sane() {
    // Multiple imports across multiple files. Sanity-check that
    // the summary aggregates correctly.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.py",
        b"from util import h1\nfrom util import h2\nimport os\n",
    );
    write(tmp.path(), "b.py", b"from util import h1\nimport sys\n");
    write(
        tmp.path(),
        "util.py",
        b"def h1():\n    return 1\n\ndef h2():\n    return 2\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    // 5 placeholders total: a.py has h1, h2, os; b.py has h1, sys.
    assert_eq!(summary.placeholders_seen, 5);
    // 3 resolvable: h1 (a.py), h2 (a.py), h1 (b.py). `os` and
    // `sys` aren't in this repo.
    assert_eq!(summary.placeholders_resolved, 3);
    assert_eq!(summary.edges_rewritten, 3);
    assert_eq!(summary.orphans_removed, 3);
    // a.py + b.py both have edges to rewrite; util.py has none.
    assert_eq!(summary.fragments_rewritten, 2);
}

// ─── via FragmentStore directly ──────────────────────────────────────

#[test]
fn link_with_store_works_on_already_opened_store() {
    // Exercise the alternate entry point: pass a FragmentStore
    // explicitly rather than re-opening from path.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "x.py", b"from y import z\n");
    write(tmp.path(), "y.py", b"def z():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let store = FragmentStore::open(tmp.path()).unwrap();
    let summary = link_with_store(&store).unwrap();
    assert_eq!(summary.placeholders_resolved, 1);
}

// ─── Repo-without-imports fast path ──────────────────────────────────

#[test]
fn star_import_stays_unresolved_with_documented_behavior() {
    // `from util import *` — placeholder named "*" with
    // import_path "util.*". The current linker treats namespace
    // imports by literal-string lookup against module names, so
    // "util.*" doesn't match the module "util" — the placeholder
    // survives. This test locks in the current behavior; a future
    // commit may extend the resolver to strip `.*` and resolve to
    // the target module (the wildcard then means "anything from
    // this module").
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import *\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 0);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let star_placeholder = cli_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::UnresolvedSymbol);
    assert!(
        star_placeholder.is_some(),
        "star-import placeholder must survive"
    );
}

#[test]
fn self_import_resolves_to_target_in_same_file() {
    // `from cli import helper` while `cli.py` defines helper.
    // The placeholder is in cli.py's fragment; the resolution
    // target is also in cli.py's fragment. The linker should
    // happily produce a self-referential edge (the importing
    // file's File node → its own helper Function). This is legal
    // Python (the import is a no-op at runtime when used at module
    // scope but legal nonetheless) and a graph traversal must
    // tolerate self-loops without livelocking.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"from cli import helper\n\ndef helper():\n    return 1\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 1);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let helper_id = cli_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .unwrap();
    let edge = cli_frag
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Imports)
        .unwrap();
    assert_eq!(edge.dst_id(), &helper_id);
}

#[test]
fn two_imports_resolving_to_same_target_keep_both_with_distinct_paths() {
    // The reviewer's concern #1: when two distinct Imports edges
    // resolve to the same target NodeId, they have different
    // `import_path` attributes and the storage layer must keep
    // both rather than deduping one of them. After the storage
    // dedup fix (now attribute-aware), both edges survive with
    // their distinct import_paths intact.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "cli.py",
        b"import pkg.sub\nfrom pkg import sub\n",
    );
    write(tmp.path(), "pkg/sub.py", b"def fn():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.placeholders_resolved, 2);

    let cli_frag = read_fragment(tmp.path(), "cli.py").unwrap();
    let sub_frag = read_fragment(tmp.path(), "pkg/sub.py").unwrap();
    let sub_file_id = sub_frag
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::File)
        .map(|n| n.id().clone())
        .unwrap();

    // Both Imports edges should land on pkg/sub.py's File node.
    let imports_edges: Vec<_> = cli_frag
        .edges()
        .iter()
        .filter(|e| e.kind() == EdgeKind::Imports)
        .collect();
    assert_eq!(imports_edges.len(), 2, "both Imports edges must survive");
    for e in &imports_edges {
        assert_eq!(e.dst_id(), &sub_file_id);
    }

    // The two edges must carry distinct import_paths: "pkg.sub"
    // (from `import pkg.sub`) and "pkg.sub" too actually (since
    // `from pkg import sub` produces import_path="pkg.sub").
    // BUT one has is_namespace=true and the other has is_named=true,
    // so the attribute payloads differ. Verify via JSON.
    use aethyme_graph_schema::EdgeAttributes;
    let mut saw_namespace = false;
    let mut saw_named = false;
    for e in &imports_edges {
        if let EdgeAttributes::Imports {
            is_namespace,
            is_named,
            ..
        } = e.attributes()
        {
            if *is_namespace {
                saw_namespace = true;
            }
            if *is_named {
                saw_named = true;
            }
        }
    }
    assert!(saw_namespace, "expected one namespace import edge");
    assert!(saw_named, "expected one named import edge");
}

#[test]
fn link_on_non_indexed_repo_returns_open_store_error() {
    // A repo with no `.aethyme/graph/` directory at all must
    // produce a structured OpenStore error rather than silently
    // succeeding. This is the "you ran link before index" case.
    let tmp = tempfile::tempdir().unwrap();
    let err = link_repo(&ctx(tmp.path())).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("open store"), "got: {msg}");
}

#[test]
fn repo_without_imports_makes_no_changes() {
    // util.py defines a function but doesn't import anything;
    // the linker should visit it but rewrite nothing.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    let bytes_before = std::fs::read(tmp.path().join(".aethyme/graph/util.py.bin")).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert_eq!(summary.fragments_visited, 1);
    assert_eq!(summary.placeholders_seen, 0);
    assert_eq!(summary.fragments_rewritten, 0);

    let bytes_after = std::fs::read(tmp.path().join(".aethyme/graph/util.py.bin")).unwrap();
    assert_eq!(bytes_before, bytes_after);
}

// ─── Rust cross-file call resolution ─────────────────────────────────
//
// The Rust indexer emits dotted call placeholders
// (`crate::store::helper` → `store.helper`). These assert the generic
// linker resolves them against a real repository, which is what makes
// `graph callers` useful for Rust sources.

#[test]
fn rust_cross_file_call_resolves_to_the_imported_function() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/store.rs",
        b"pub fn helper() -> u32 {\n    1\n}\n",
    );
    write(
        tmp.path(),
        "src/api.rs",
        b"use crate::store::helper;\n\npub fn caller() -> u32 {\n    helper()\n}\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();

    let summary = link_repo(&ctx(tmp.path())).unwrap();
    assert!(
        summary.placeholders_resolved > 0,
        "Rust import and call placeholders must resolve: {summary:?}"
    );

    let store = read_fragment(tmp.path(), "src/store.rs").unwrap();
    let helper_id = store
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .expect("helper should exist in store.rs");

    let api = read_fragment(tmp.path(), "src/api.rs").unwrap();
    let call_edge = api
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("resolved Calls edge should survive linking");
    assert_eq!(
        call_edge.dst_id(),
        &helper_id,
        "the call must point at store.rs's helper"
    );

    let remaining = api
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .count();
    assert_eq!(remaining, 0, "no call placeholder should survive");
}

#[test]
fn rust_same_file_call_resolves_without_any_import() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/a.rs",
        b"fn inner() -> u32 {\n    1\n}\n\npub fn outer() -> u32 {\n    inner()\n}\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    link_repo(&ctx(tmp.path())).unwrap();

    let fragment = read_fragment(tmp.path(), "src/a.rs").unwrap();
    // `inner` is the callee, so the Calls edge's destination is the
    // answer; find the function whose id is neither the call's source
    // (the caller) nor the file node.
    let call_edge = fragment
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("a same-file call must produce a Calls edge");
    let target_id = fragment
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function && n.id() == call_edge.dst_id())
        .map(|n| n.id().clone());
    let Some(target_id) = target_id else {
        let kinds: Vec<_> = fragment
            .nodes()
            .iter()
            .map(|n| format!("{:?}", n.kind()))
            .collect();
        panic!(
            "call destination must resolve to a Function; node kinds present: {kinds:?}, \
             call dst = {:?}",
            call_edge.dst_id()
        );
    };

    let call_edges: Vec<_> = fragment
        .edges()
        .iter()
        .filter(|e| e.kind() == EdgeKind::Calls)
        .collect();
    assert_eq!(call_edges.len(), 1, "one call edge expected");
    assert_eq!(call_edges[0].dst_id(), &target_id);
}

#[test]
fn rust_ambiguous_call_fails_closed_rather_than_guessing() {
    // Two modules define `helper`. `api.rs` calls a bare `helper()`.
    //
    // The linker deliberately refuses whole-repo name-only resolution
    // for `Calls` edges (see the module docs: a bare name cannot be
    // distinguished from a local or callback binding), so the call
    // stays on its placeholder instead of being bound to one of the two
    // same-named symbols at high confidence.
    //
    // This is the behaviour we want: a missing edge is recoverable by
    // an agent that verifies spans, while a confidently wrong edge is
    // not.
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/one.rs",
        b"pub fn helper() -> u32 {\n    1\n}\n",
    );
    write(
        tmp.path(),
        "src/two.rs",
        b"pub fn helper() -> u32 {\n    2\n}\n",
    );
    write(
        tmp.path(),
        "src/api.rs",
        b"use crate::one::helper;\n\npub fn caller() -> u32 {\n    helper()\n}\n",
    );
    index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    link_repo(&ctx(tmp.path())).unwrap();

    let one = read_fragment(tmp.path(), "src/one.rs").unwrap();
    let two = read_fragment(tmp.path(), "src/two.rs").unwrap();
    let one_id = one
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .unwrap();
    let two_id = two
        .nodes()
        .iter()
        .find(|n| n.kind() == NodeKind::Function)
        .map(|n| n.id().clone())
        .unwrap();

    let api = read_fragment(tmp.path(), "src/api.rs").unwrap();
    let call_edge = api
        .edges()
        .iter()
        .find(|e| e.kind() == EdgeKind::Calls)
        .expect("Calls edge should survive");
    assert_ne!(
        call_edge.dst_id(),
        &one_id,
        "an ambiguous bare call must not bind to an arbitrary same-named symbol"
    );
    assert_ne!(call_edge.dst_id(), &two_id);

    // The placeholder survives, so the uncertainty stays visible in the
    // graph rather than being silently dropped.
    assert!(
        api.nodes()
            .iter()
            .any(|n| n.kind() == NodeKind::UnresolvedSymbol && n.id() == call_edge.dst_id()),
        "the unresolved placeholder must be retained"
    );
}

// ─── Auto-link inside index_repo_to_disk's binary wrapper ───────────
// (See cli_binary.rs for end-to-end CLI tests of the --skip-link flag.)
