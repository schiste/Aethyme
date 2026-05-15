//! Integration tests for `FragmentStore`. Builds a tiny mock
//! `.aethyme/graph/` tree by writing fragments + shards via the
//! existing disk helpers, then exercises the store's read API.

use aethyme_graph_schema::{
    Confidence, Edge, EdgeAttributes, Function, Node, NodeId, NodeKind,
    ParameterSignature, Source, SourceRange, Visibility,
};
use aethyme_graph_storage::{
    write_fragment, write_index_shard, Fragment, FragmentStore,
    StoreOpenError, SymbolRecord,
};

fn ctx_repo(tmp: &std::path::Path) -> std::path::PathBuf {
    tmp.to_path_buf()
}

fn sample_function(name: &str, line: u32) -> Function {
    Function::new(
        "testrepo",
        "src/x.py",
        name,
        &format!("def {name}()"),
        vec![ParameterSignature {
            name: "x".into(),
            type_str: None,
            default_value: None,
        }],
        None,
        SourceRange::new(line, line + 5).unwrap(),
        Visibility::Public,
        true,
    )
    .unwrap()
}

fn write_test_fragment(tmp: &std::path::Path) {
    use aethyme_graph_schema::Callable;
    let f1 = sample_function("explore_command", 10);
    let f2 = sample_function("run", 50);
    let edge = Edge::new(
        f1.id().clone(),
        f2.id().clone(),
        EdgeAttributes::Calls,
        Source::Code,
        Confidence::FULL,
    );
    let frag = Fragment::new(
        "src/x.py",
        vec![Node::Function(f1.clone()), Node::Function(f2.clone())],
        vec![edge],
    )
    .unwrap();
    write_fragment(tmp, "src/x.py", &frag).unwrap();

    let id_for = |name: &str| -> NodeId {
        NodeId::new(NodeKind::Function, "testrepo", "src/x.py", name).unwrap()
    };
    let records = vec![
        SymbolRecord {
            module: "src.x".into(),
            symbol: "explore_command".into(),
            kind: NodeKind::Function,
            node_id: id_for("explore_command"),
            file: "src/x.py".into(),
        },
        SymbolRecord {
            module: "src.x".into(),
            symbol: "run".into(),
            kind: NodeKind::Function,
            node_id: id_for("run"),
            file: "src/x.py".into(),
        },
    ];
    write_index_shard(tmp, "src.x", &records).unwrap();
}

// ─── Open / layout ──────────────────────────────────────────────────

#[test]
fn open_succeeds_after_bootstrap() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme/graph/_index")).unwrap();
    let store = FragmentStore::open(ctx_repo(tmp.path())).unwrap();
    assert_eq!(store.repo_root(), tmp.path());
}

#[test]
fn open_fails_without_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let err = FragmentStore::open(ctx_repo(tmp.path())).unwrap_err();
    assert!(matches!(err, StoreOpenError::MissingLayout { .. }));
}

// ─── Iteration ──────────────────────────────────────────────────────

#[test]
fn list_indexed_source_paths_returns_sorted_paths() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let paths = store.list_indexed_source_paths().unwrap();
    assert_eq!(paths, vec!["src/x.py".to_string()]);
}

#[test]
fn list_modules_returns_sorted_modules() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let modules = store.list_modules().unwrap();
    assert_eq!(modules, vec!["src.x".to_string()]);
}

#[test]
fn list_modules_empty_when_no_index_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let store = FragmentStore::open(tmp.path()).unwrap();
    let modules = store.list_modules().unwrap();
    assert!(modules.is_empty());
}

// ─── Symbol lookup ──────────────────────────────────────────────────

#[test]
fn find_symbols_by_name_in_specific_module() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let hits = store
        .find_symbols(Some("src.x"), "explore_command", None)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(&*hits[0].symbol, "explore_command");
}

#[test]
fn find_symbols_filters_by_kind() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    // Right name, right kind → 1 hit.
    let hits = store
        .find_symbols(None, "run", Some(NodeKind::Function))
        .unwrap();
    assert_eq!(hits.len(), 1);
    // Right name, wrong kind → 0 hits.
    let hits = store
        .find_symbols(None, "run", Some(NodeKind::Class))
        .unwrap();
    assert_eq!(hits.len(), 0);
}

#[test]
fn find_symbols_across_all_modules_when_module_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let hits = store.find_symbols(None, "run", None).unwrap();
    assert_eq!(hits.len(), 1);
}

// ─── Aggregations ───────────────────────────────────────────────────

#[test]
fn count_nodes_by_kind_aggregates_across_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let counts = store.count_nodes_by_kind().unwrap();
    // Two Functions in our test fragment.
    assert_eq!(counts.get(&NodeKind::Function), Some(&2));
}

#[test]
fn fragment_for_node_locates_the_owning_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let id =
        NodeId::new(NodeKind::Function, "testrepo", "src/x.py", "run")
            .unwrap();
    let frag = store.fragment_for_node(&id).unwrap();
    assert!(frag.is_some());
    assert_eq!(frag.unwrap().file_path(), "src/x.py");
}

#[test]
fn fragment_for_node_returns_none_for_missing_node() {
    let tmp = tempfile::tempdir().unwrap();
    write_test_fragment(tmp.path());

    let store = FragmentStore::open(tmp.path()).unwrap();
    let id =
        NodeId::new(NodeKind::Function, "testrepo", "src/x.py", "absent")
            .unwrap();
    let frag = store.fragment_for_node(&id).unwrap();
    assert!(frag.is_none());
}
