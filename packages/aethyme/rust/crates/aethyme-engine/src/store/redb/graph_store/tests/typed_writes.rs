use super::*;

#[test]
fn typed_insert_repository_round_trip() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let repository = RepositoryNode::new("Repo", root.to_str().unwrap());
    let key = repository.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_repository(&mut session, &repository).expect("insert_repository");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), REPOSITORIES, &key).expect("present");
    let got: RepositoryNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, repository);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_directory_indexes_path() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let directory = sample_directory("Repo", "src", Some("area:Repo:src"));
    let id = directory.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_directory(&mut session, &directory).expect("insert_directory");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), DIRECTORIES, &id).expect("present");
    let got: DirectoryNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, directory);
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "src"),
        vec![id]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_area_round_trip() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let key = area.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("insert_area");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), AREAS, &key).expect("present");
    let got: AreaNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, area);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_file_indexes_path() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let id = file.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_file(&mut session, &file).expect("insert_file");
    session.commit().expect("commit");

    // Primary row.
    let bytes = read_node_bytes(store.db(), FILES, &id).expect("present");
    let got: FileNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, file);

    // NODES_BY_PATH lookup.
    let txn = store.db().begin_read().expect("read");
    let by_path = txn
        .open_multimap_table(NODES_BY_PATH)
        .expect("NODES_BY_PATH");
    let hits: Vec<String> = by_path
        .get("src/lib.rs")
        .expect("get")
        .map(|r| r.expect("row").value().to_string())
        .collect();
    assert_eq!(hits, vec![id]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_function_populates_symbol_and_path_indexes() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let function = sample_function(&file, "LoadToken", None);
    let id = function.id.to_string();

    let mut session = store.begin_index().expect("session");
    insert_function(&mut session, &function).expect("insert_function");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), FUNCTIONS, &id).expect("present");
    let got: FunctionNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, function);
    assert_eq!(
        collect_str_multimap(store.db(), FUNCTIONS_BY_PATH, "src/lib.rs"),
        vec![id.clone()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
        vec![id.clone()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_NAME, "loadtoken"),
        vec![id]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "load"),
        vec![function.id.to_string()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "token"),
        vec![function.id.to_string()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_PATH_COMPONENT, "lib"),
        vec![function.id.to_string()]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn exact_file_callable_lookup_is_deterministic_and_strictly_bounded() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let prefixed_file = sample_file("Repo", "src/lib.rs.extra", Some("area:Repo:src"));
    let alpha = sample_function(&file, "Alpha", None);
    let zeta = sample_function(&file, "Zeta", None);
    let prefixed = sample_function(&prefixed_file, "Prefixed", None);

    let mut session = store.begin_index().expect("session");
    for function in [&zeta, &prefixed, &alpha] {
        insert_function(&mut session, function).expect("insert_function");
    }
    session.commit().expect("commit");

    let bounded = store
        .function_ids_for_path("src/lib.rs", 1)
        .expect("bounded exact lookup");
    assert_eq!(bounded.ids, vec![alpha.id.to_string()]);
    assert!(bounded.truncated);

    let complete = store
        .function_ids_for_path("src/lib.rs", 2)
        .expect("complete exact lookup");
    assert_eq!(
        complete.ids,
        vec![alpha.id.to_string(), zeta.id.to_string()]
    );
    assert!(!complete.truncated);
    assert!(!complete.ids.contains(&prefixed.id.to_string()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_class_populates_symbol_and_path_indexes() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let class = sample_class(&file, "TokenLoader");
    let id = class.id.to_string();

    let mut session = store.begin_index().expect("session");
    insert_class(&mut session, &class).expect("insert_class");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), CLASSES, &id).expect("present");
    let got: ClassNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, class);
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
        vec![id.clone()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_NAME, "tokenloader"),
        vec![id]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "token"),
        vec![class.id.to_string()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_COMPONENT, "loader"),
        vec![class.id.to_string()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), SYMBOL_BY_PATH_COMPONENT, "lib"),
        vec![class.id.to_string()]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_doc_and_config_index_paths() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let doc = DocNode::new(
        "Repo",
        "file:Repo:docs/auth.md",
        "docs/auth.md",
        "Auth",
        "markdown",
        Some("area:Repo:docs".to_string()),
    );
    let config = ConfigNode::new(
        "Repo",
        "file:Repo:pyproject.toml",
        "pyproject.toml",
        "toml",
        None,
    );

    let mut session = store.begin_index().expect("session");
    insert_doc(&mut session, &doc).expect("insert_doc");
    insert_config(&mut session, &config).expect("insert_config");
    session.commit().expect("commit");

    let doc_bytes = read_node_bytes(store.db(), DOCS, &doc.id).expect("doc present");
    let got_doc: DocNode = bincode::deserialize(&doc_bytes).expect("decode doc");
    assert_eq!(got_doc, doc);
    let config_bytes = read_node_bytes(store.db(), CONFIGS, &config.id).expect("config present");
    let got_config: ConfigNode = bincode::deserialize(&config_bytes).expect("decode config");
    assert_eq!(got_config, config);
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "docs/auth.md"),
        vec![doc.id.clone()]
    );
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "pyproject.toml"),
        vec![config.id.clone()]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_unresolved_indexes_source_path() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let unresolved = sample_unresolved(&file, "missing_call");
    let id = unresolved.id.to_string();

    let mut session = store.begin_index().expect("session");
    insert_unresolved(&mut session, &unresolved).expect("insert_unresolved");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), UNRESOLVED, &id).expect("present");
    let got: UnresolvedNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, unresolved);
    assert_eq!(
        collect_str_multimap(store.db(), NODES_BY_PATH, "src/lib.rs"),
        vec![id]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_edge_passes_through() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let edge = Edge::new(
        "file:Repo:a.rs",
        "file:Repo:b.rs",
        EdgeKind::Imports,
        100,
        "import",
    );

    let mut session = store.begin_index().expect("session");
    insert_edge(&mut session, &edge).expect("insert_edge");
    session.commit().expect("commit");

    let out = collect_multimap(store.db(), EDGES_OUT, "file:Repo:a.rs");
    assert_eq!(out.len(), 1);
    let rec: AdjacencyRecord = bincode::deserialize(&out[0]).expect("decode");
    assert_eq!(rec.kind, EdgeKind::Imports);
    assert_eq!(rec.other.as_str(), "file:Repo:b.rs");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typed_insert_risk_round_trip() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let risk = RiskFlag::new("auth/", RiskArea::Auth, RiskLevel::High, "secrets");

    let mut session = store.begin_index().expect("session");
    insert_risk(&mut session, &risk).expect("insert_risk");
    session.commit().expect("commit");

    let txn = store.db().begin_read().expect("read");
    let t = txn.open_multimap_table(RISK_FLAGS).expect("RISK_FLAGS");
    let hits: Vec<RiskFlag> = t
        .get("auth/")
        .expect("get")
        .map(|r| bincode::deserialize::<RiskFlag>(r.expect("row").value()).expect("decode"))
        .collect();
    assert_eq!(hits, vec![risk]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn set_and_read_repo_metadata() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    assert!(store.repo_metadata().expect("read").is_none());

    let meta = RepoMetadata {
        root_path: "/tmp/repo".into(),
        commit_hash: Some("deadbeef".into()),
        indexed_at_unix: 1_700_000_000,
        file_count: 4093,
        languages: vec!["php".into(), "js".into()],
    };
    store.set_repo_metadata(&meta).expect("write meta");

    let got = store.repo_metadata().expect("read").expect("present");
    assert_eq!(got, meta);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reset_drops_all_data_but_preserves_schema() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let key = area.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("insert");
    session.commit().expect("commit");
    assert!(read_node_bytes(store.db(), AREAS, &key).is_some());

    // Drop the original handle so the file lock is released, then reset.
    drop(store);
    let store2 = GraphStore::reset(&root).expect("reset");
    assert!(read_node_bytes(store2.db(), AREAS, &key).is_none(), "wiped");

    // Schema is fresh — schema_version sentinel must round-trip again.
    let txn = store2.db().begin_read().expect("read");
    let meta = txn.open_table(META).expect("META");
    let value = meta
        .get(META_KEY_SCHEMA_VERSION)
        .expect("get")
        .expect("present");
    let bytes: [u8; 4] = value.value().try_into().expect("4 bytes");
    assert_eq!(u32::from_le_bytes(bytes), SCHEMA_VERSION);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn delete_file_data_removes_file_and_its_edges() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let a = sample_file("Repo", "a.rs", None);
    let b = sample_file("Repo", "b.rs", None);
    let c = sample_file("Repo", "c.rs", None);
    let a_id = a.id.clone();

    let mut session = store.begin_index().expect("session");
    insert_file(&mut session, &a).expect("a");
    insert_file(&mut session, &b).expect("b");
    insert_file(&mut session, &c).expect("c");

    // Edges: a → b, a → c, c → a (a has both outgoing and incoming).
    insert_edge(
        &mut session,
        &Edge::new(&a.id, &b.id, EdgeKind::Imports, 100, "imp"),
    )
    .expect("a→b");
    insert_edge(
        &mut session,
        &Edge::new(&a.id, &c.id, EdgeKind::Imports, 100, "imp"),
    )
    .expect("a→c");
    insert_edge(
        &mut session,
        &Edge::new(&c.id, &a.id, EdgeKind::Imports, 100, "imp"),
    )
    .expect("c→a");
    session.commit().expect("commit");
    assert_eq!(
        edges_by_kind_limited_from(store.db(), EdgeKind::Imports, 10)
            .expect("imports by kind")
            .len(),
        3
    );

    store.delete_file_data(&a_id).expect("delete");

    // a's row is gone.
    assert!(read_node_bytes(store.db(), FILES, &a_id).is_none());
    // a's outgoing edges are gone.
    assert_eq!(collect_multimap(store.db(), EDGES_OUT, &a_id).len(), 0);
    // a's incoming edges are gone.
    assert_eq!(collect_multimap(store.db(), EDGES_IN, &a_id).len(), 0);
    // b no longer has an incoming from a.
    assert_eq!(collect_multimap(store.db(), EDGES_IN, &b.id).len(), 0);
    // c no longer has an incoming from a (the a→c edge), but DOES still
    // have its outgoing to a removed too.
    assert_eq!(collect_multimap(store.db(), EDGES_IN, &c.id).len(), 0);
    assert_eq!(collect_multimap(store.db(), EDGES_OUT, &c.id).len(), 0);
    // b and c rows themselves remain.
    assert!(read_node_bytes(store.db(), FILES, &b.id).is_some());
    assert!(read_node_bytes(store.db(), FILES, &c.id).is_some());
    assert!(
        edges_by_kind_limited_from(store.db(), EdgeKind::Imports, 10)
            .expect("imports by kind after delete")
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_areas_filters_by_depth_and_sorts_stable() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");
    // Insert areas in non-sorted order to verify the sort.
    for prefix in ["src/lib", "tests", "src", "src/bin", "docs"] {
        insert_area(&mut session, &AreaNode::new("Repo", prefix, false)).expect("insert");
    }
    session.commit().expect("commit");

    let all = store.list_areas(None).expect("list all");
    let prefixes: Vec<&str> = all.iter().map(|a| a.path_prefix.as_str()).collect();
    // depth 1 areas first (alphabetical), then depth 2.
    assert_eq!(prefixes, vec!["docs", "src", "tests", "src/bin", "src/lib"]);

    let depth1 = store.list_areas(Some(1)).expect("depth 1");
    let prefixes1: Vec<&str> = depth1.iter().map(|a| a.path_prefix.as_str()).collect();
    assert_eq!(prefixes1, vec!["docs", "src", "tests"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn edges_from_and_edges_to() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");
    insert_edge(
        &mut session,
        &Edge::new("file:R:a.rs", "file:R:b.rs", EdgeKind::Imports, 100, "imp"),
    )
    .expect("a→b");
    insert_edge(
        &mut session,
        &Edge::new("file:R:a.rs", "file:R:c.rs", EdgeKind::Imports, 100, "imp"),
    )
    .expect("a→c");
    insert_edge(
        &mut session,
        &Edge::new("file:R:c.rs", "file:R:b.rs", EdgeKind::Imports, 100, "imp"),
    )
    .expect("c→b");
    session.commit().expect("commit");

    let from_a = store.edges_from("file:R:a.rs").expect("from a");
    let mut targets: Vec<&str> = from_a.iter().map(|r| r.other.as_str()).collect();
    targets.sort();
    assert_eq!(targets, vec!["file:R:b.rs", "file:R:c.rs"]);

    let to_b = store.edges_to("file:R:b.rs").expect("to b");
    let mut srcs: Vec<&str> = to_b.iter().map(|r| r.other.as_str()).collect();
    srcs.sort();
    assert_eq!(srcs, vec!["file:R:a.rs", "file:R:c.rs"]);

    // Unknown id: empty, not error.
    assert!(store.edges_from("file:R:nope.rs").expect("ok").is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
