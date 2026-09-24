use std::collections::BTreeMap;

use super::*;
use redb::{ReadableDatabase, ReadableTableMetadata};
use serde::Deserialize;

macro_rules! function_name {
    () => {{
        fn f() {}
        fn type_name<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let n = type_name(f);
        &n[..n.len() - 3]
    }};
}

fn tmp_root(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("aethyme_graph_store_{name}_{nonce}"))
}

fn mark_graph_store_as_redb_v2(root: &Path) {
    let db_path = root.join(".aethyme").join(DB_FILE_NAME);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&db_path)
        .expect("open db file");
    for offset in [64u64, 192u64] {
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))
            .expect("seek version byte");
        std::io::Write::write_all(&mut file, &[2]).expect("write v2 marker");
    }
}

#[test]
fn open_creates_dotaethyme_and_db_file() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    assert!(root.join(".aethyme").is_dir(), ".aethyme dir created");
    assert!(store.path().exists(), "graph_store.redb file created");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reopen_is_idempotent() {
    let root = tmp_root(function_name!());
    let _ = GraphStore::open(&root).expect("first open");
    let _ = GraphStore::open(&root).expect("reopen");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn all_tables_exist_on_fresh_db() {
    // Reads from a fresh store should not trip TableDoesNotExist.
    // ensure_schema must touch every table so downstream queries
    // don't have to special-case empty-DB lookups.
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let txn = store.db().begin_read().expect("read txn");

    // Single tables: open_table on a missing table is a TableError.
    txn.open_table(REPOSITORIES).expect("REPOSITORIES");
    txn.open_table(DIRECTORIES).expect("DIRECTORIES");
    txn.open_table(FILES).expect("FILES");
    txn.open_table(AREAS).expect("AREAS");
    txn.open_table(FUNCTIONS).expect("FUNCTIONS");
    txn.open_table(CLASSES).expect("CLASSES");
    txn.open_table(DOCS).expect("DOCS");
    txn.open_table(CONFIGS).expect("CONFIGS");
    txn.open_table(SURFACES).expect("SURFACES");
    txn.open_table(UNRESOLVED).expect("UNRESOLVED");
    txn.open_table(META).expect("META");

    // Multimap tables.
    let edges_out = txn.open_multimap_table(EDGES_OUT).expect("EDGES_OUT");
    let edges_in = txn.open_multimap_table(EDGES_IN).expect("EDGES_IN");
    let edges_by_kind = txn
        .open_multimap_table(EDGES_BY_KIND)
        .expect("EDGES_BY_KIND");
    assert_eq!(edges_out.len().unwrap(), 0);
    assert_eq!(edges_in.len().unwrap(), 0);
    assert_eq!(edges_by_kind.len().unwrap(), 0);
    txn.open_multimap_table(FUNCTIONS_BY_PATH)
        .expect("FUNCTIONS_BY_PATH");
    txn.open_multimap_table(NODES_BY_PATH)
        .expect("NODES_BY_PATH");
    txn.open_multimap_table(SYMBOL_BY_NAME)
        .expect("SYMBOL_BY_NAME");
    txn.open_multimap_table(SYMBOL_BY_COMPONENT)
        .expect("SYMBOL_BY_COMPONENT");
    txn.open_multimap_table(RISK_FLAGS).expect("RISK_FLAGS");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn schema_version_is_persisted() {
    let root = tmp_root(function_name!());
    let _ = GraphStore::open(&root).expect("open");
    // Reopen and confirm the sentinel reads back as the current schema.
    let store = GraphStore::open(&root).expect("reopen");
    let txn = store.db().begin_read().expect("read txn");
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
fn read_only_open_does_not_create_missing_store() {
    let root = tmp_root(function_name!());
    assert!(GraphStore::open_read_only(&root).is_err());
    assert!(
        !root.join(".aethyme").exists(),
        "read-only open must not initialize .aethyme"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Test-only sample node — small Serialize/Deserialize struct so we can
/// exercise insert_node without depending on FileNode/FunctionNode shape.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct SampleNode {
    id: String,
    path: String,
}

fn read_node_bytes(
    db: &Database,
    table: TableDefinition<&str, &[u8]>,
    key: &str,
) -> Option<Vec<u8>> {
    let txn = db.begin_read().expect("read txn");
    let t = txn.open_table(table).expect("open");
    t.get(key).expect("get").map(|v| v.value().to_vec())
}

fn collect_multimap(
    db: &Database,
    table: MultimapTableDefinition<&str, &[u8]>,
    key: &str,
) -> Vec<Vec<u8>> {
    let txn = db.begin_read().expect("read txn");
    let t = txn.open_multimap_table(table).expect("open");
    let iter = t.get(key).expect("get");
    iter.map(|r| r.expect("row").value().to_vec()).collect()
}

fn collect_str_multimap(
    db: &Database,
    table: MultimapTableDefinition<&str, &str>,
    key: &str,
) -> Vec<String> {
    let txn = db.begin_read().expect("read txn");
    let t = txn.open_multimap_table(table).expect("open");
    let iter = t.get(key).expect("get");
    iter.map(|r| r.expect("row").value().to_string()).collect()
}

#[test]
fn insert_node_and_read_back() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");
    let node = SampleNode {
        id: "file:Repo:src/lib.rs".into(),
        path: "src/lib.rs".into(),
    };
    session.insert_node(FILES, &node.id, &node).expect("insert");
    session.commit().expect("commit");

    let bytes = read_node_bytes(store.db(), FILES, "file:Repo:src/lib.rs").expect("present");
    let got: SampleNode = bincode::deserialize(&bytes).expect("decode");
    assert_eq!(got, node);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn insert_edge_writes_both_directions() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");
    session
        .insert_edge(
            "file:Repo:a.rs",
            "file:Repo:b.rs",
            EdgeKind::Imports,
            100,
            InternedStr::from("import"),
        )
        .expect("insert_edge");
    session.commit().expect("commit");

    let out = collect_multimap(store.db(), EDGES_OUT, "file:Repo:a.rs");
    assert_eq!(out.len(), 1, "EDGES_OUT has one row keyed by src");
    let out_rec: AdjacencyRecord = bincode::deserialize(&out[0]).expect("decode");
    assert_eq!(out_rec.kind, EdgeKind::Imports);
    assert_eq!(out_rec.other.as_str(), "file:Repo:b.rs");
    assert_eq!(out_rec.confidence, 100);

    let inv = collect_multimap(store.db(), EDGES_IN, "file:Repo:b.rs");
    assert_eq!(inv.len(), 1, "EDGES_IN has one row keyed by dst");
    let in_rec: AdjacencyRecord = bincode::deserialize(&inv[0]).expect("decode");
    assert_eq!(in_rec.kind, EdgeKind::Imports);
    assert_eq!(in_rec.other.as_str(), "file:Repo:a.rs");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_only_store_reads_query_surfaces() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let file_a = FileNode::new(
        "Repo",
        "src/a.rs",
        Some("Rust".to_string()),
        crate::model::file::FileRole::Source,
        10,
        100,
        false,
        Some(area.id.clone()),
    );
    let file_b = FileNode::new(
        "Repo",
        "src/b.rs",
        Some("Rust".to_string()),
        crate::model::file::FileRole::Source,
        20,
        200,
        false,
        Some(area.id.clone()),
    );
    let edge = Edge::new(
        file_a.id.clone(),
        file_b.id.clone(),
        EdgeKind::Imports,
        100,
        "read-only-test",
    );

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("area");
    insert_file(&mut session, &file_a).expect("file a");
    insert_file(&mut session, &file_b).expect("file b");
    insert_edge(&mut session, &edge).expect("edge");
    session.commit().expect("commit");
    store
        .set_repo_metadata(&RepoMetadata {
            root_path: root.to_string_lossy().to_string(),
            commit_hash: Some("abc123".to_string()),
            indexed_at_unix: 1,
            file_count: 2,
            languages: vec!["Rust".to_string()],
        })
        .expect("metadata");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only open");
    assert_eq!(
        readonly
            .repo_metadata()
            .expect("metadata")
            .unwrap()
            .file_count,
        2
    );
    assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);
    assert_eq!(
        readonly.edges_from(&file_a.id).expect("edges from")[0]
            .other
            .as_str(),
        file_b.id
    );
    assert_eq!(
        readonly.edges_to(&file_b.id).expect("edges to")[0]
            .other
            .as_str(),
        file_a.id
    );

    let _ = std::fs::remove_dir_all(&root);
}

fn surface_node(kind: SurfaceKind, path: &str, name: &str, line: usize) -> SurfaceNode {
    let schema_id = aethyme_graph_schema::NodeId::new(
        match kind {
            SurfaceKind::BehaviorTestSurface => aethyme_graph_schema::NodeKind::BehaviorTestSurface,
            SurfaceKind::CliSurface => aethyme_graph_schema::NodeKind::CliSurface,
            SurfaceKind::CredentialOperation => aethyme_graph_schema::NodeKind::CredentialOperation,
            SurfaceKind::JobSurface => aethyme_graph_schema::NodeKind::JobSurface,
            SurfaceKind::MiddlewareInstallation => {
                aethyme_graph_schema::NodeKind::MiddlewareInstallation
            }
            SurfaceKind::ProxySurface => aethyme_graph_schema::NodeKind::ProxySurface,
            SurfaceKind::QueueSurface => aethyme_graph_schema::NodeKind::QueueSurface,
            SurfaceKind::RouteSurface => aethyme_graph_schema::NodeKind::RouteSurface,
            SurfaceKind::WebhookSurface => aethyme_graph_schema::NodeKind::WebhookSurface,
            SurfaceKind::WorkerSurface => aethyme_graph_schema::NodeKind::WorkerSurface,
        },
        "Repo",
        path,
        name,
    )
    .expect("schema id");
    SurfaceNode {
        id: InternedStr::from(schema_id.as_str()),
        kind,
        name: InternedStr::from(name),
        file_id: InternedStr::from(format!("file:Repo:{path}")),
        file_path: InternedStr::from(path),
        area_id: Some(InternedStr::from("area:Repo:src")),
        language: InternedStr::from("python"),
        line,
        detail: InternedStr::from(kind.label()),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn surface_nodes_are_persisted_and_queryable() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let file = FileNode::new(
        "Repo",
        "src/routes.py",
        Some("python".to_string()),
        crate::model::file::FileRole::Source,
        20,
        300,
        false,
        Some(area.id.clone()),
    );
    let route = surface_node(
        SurfaceKind::RouteSurface,
        "src/routes.py",
        "GET /api/token",
        3,
    );
    let middleware = surface_node(
        SurfaceKind::MiddlewareInstallation,
        "src/routes.py",
        "TokenAuthMiddleware",
        7,
    );
    let proxy = surface_node(
        SurfaceKind::ProxySurface,
        "src/routes.py",
        "https://api.example.com",
        12,
    );

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("area");
    insert_file(&mut session, &file).expect("file");
    for surface in [&route, &middleware, &proxy] {
        insert_surface(&mut session, surface).expect("surface");
    }
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            route.id.as_str(),
            EdgeKind::Exposes,
            850,
            "surface-flow",
        ),
    )
    .expect("route edge");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            middleware.id.as_str(),
            EdgeKind::InstallsMiddleware,
            850,
            "surface-flow",
        ),
    )
    .expect("middleware edge");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            proxy.id.as_str(),
            EdgeKind::ForwardsTo,
            850,
            "surface-flow",
        ),
    )
    .expect("proxy edge");
    session.commit().expect("commit");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only open");
    assert_eq!(
        readonly
            .node_display(route.id.as_str())
            .expect("display")
            .unwrap()
            .kind,
        StoredNodeKind::RouteSurface
    );
    assert!(matches!(
        readonly.get_node(middleware.id.as_str()).expect("node"),
        Some(StoredNode::Surface(node)) if node.kind == SurfaceKind::MiddlewareInstallation
    ));
    let nodes = readonly.nodes_under_path("src/").expect("nodes under path");
    assert!(
        nodes
            .iter()
            .filter(|node| matches!(node, StoredNode::Surface(_)))
            .count()
            >= 3
    );
    let symbols = readonly
        .find_symbols("GET /api/token", Some(StoredNodeKind::RouteSurface))
        .expect("symbol lookup");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].id, route.id.as_str());
    let children = readonly.children(&file.id, None).expect("children");
    assert!(
        children
            .iter()
            .any(|node| node.kind == StoredNodeKind::RouteSurface)
    );
    assert!(
        children
            .iter()
            .any(|node| node.kind == StoredNodeKind::MiddlewareInstallation)
    );
    assert!(
        children
            .iter()
            .any(|node| node.kind == StoredNodeKind::ProxySurface)
    );
    assert_eq!(
        readonly
            .overview_v2(OverviewV2Limits {
                area_limit: 0,
                directory_limit: 0,
                entrypoint_limit: 0,
                risk_limit: 0,
                file_limit: 0,
                function_limit: 0,
                class_limit: 0,
                doc_limit: 0,
                config_limit: 0,
                surface_limit: 10,
                unresolved_limit: 0,
            })
            .expect("overview")
            .surfaces
            .len(),
        3
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn surface_flow_edges_round_trip_through_redb() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let file = FileNode::new(
        "Repo",
        "src/flow.py",
        Some("python".to_string()),
        crate::model::file::FileRole::Source,
        20,
        300,
        false,
        Some(area.id.clone()),
    );
    let surface = surface_node(
        SurfaceKind::CredentialOperation,
        "src/flow.py",
        "token flow",
        5,
    );
    let flow_edges = [
        EdgeKind::Exposes,
        EdgeKind::ForwardsTo,
        EdgeKind::RewritesHeader,
        EdgeKind::InstallsMiddleware,
        EdgeKind::ValidatesCredential,
        EdgeKind::Authorizes,
        EdgeKind::IssuesCredential,
        EdgeKind::StoresCredential,
        EdgeKind::UsesCredential,
        EdgeKind::TestedBy,
    ];

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("area");
    insert_file(&mut session, &file).expect("file");
    insert_surface(&mut session, &surface).expect("surface");
    for kind in &flow_edges {
        insert_edge(
            &mut session,
            &Edge::new(
                &file.id,
                surface.id.as_str(),
                kind.clone(),
                850,
                "surface-flow",
            ),
        )
        .expect("flow edge");
    }
    session.commit().expect("commit");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only open");
    let persisted = readonly
        .neighbors(&file.id, NeighborDirection::Outgoing, None)
        .expect("outgoing neighbors")
        .into_iter()
        .filter(|edge| edge.other == surface.id.as_str())
        .map(|edge| edge.kind)
        .collect::<BTreeSet<_>>();
    for kind in &flow_edges {
        assert!(
            persisted.contains(kind),
            "expected persisted flow edge {kind:?}; got {persisted:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compact_preserves_committed_query_data() {
    let root = tmp_root(function_name!());
    let mut store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("area");
    session.commit().expect("commit");

    let _ = store.compact().expect("compact");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only reopen");
    assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn non_durable_index_commits_are_persisted_by_final_metadata_commit() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "src", false);
    let mut session = store
        .begin_index_with_durability(IndexDurability::None)
        .expect("session");
    insert_area(&mut session, &area).expect("area");
    session.commit().expect("commit");
    store
        .set_repo_metadata(&RepoMetadata {
            root_path: root.to_string_lossy().to_string(),
            commit_hash: None,
            indexed_at_unix: 1,
            file_count: 0,
            languages: Vec::new(),
        })
        .expect("metadata");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only reopen");
    assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![area]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn staging_store_does_not_replace_public_store_until_publish() {
    let root = tmp_root(function_name!());
    let old_area = AreaNode::new("Repo", "old", false);
    let new_area = AreaNode::new("Repo", "new", false);
    let store = GraphStore::reset(&root).expect("open public");
    let mut session = store.begin_index().expect("public session");
    insert_area(&mut session, &old_area).expect("old area");
    session.commit().expect("public commit");
    drop(store);

    let staging = GraphStore::reset_staging(&root).expect("open staging");
    let mut session = staging
        .begin_index_with_durability(IndexDurability::None)
        .expect("staging session");
    insert_area(&mut session, &new_area).expect("new area");
    session.commit().expect("staging commit");
    staging
        .set_repo_metadata(&RepoMetadata {
            root_path: root.to_string_lossy().to_string(),
            commit_hash: None,
            indexed_at_unix: 1,
            file_count: 0,
            languages: Vec::new(),
        })
        .expect("metadata");
    drop(staging);

    let readonly = GraphStore::open_read_only(&root).expect("read public");
    assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![old_area]);
    drop(readonly);

    GraphStore::publish_staging(&root).expect("publish staging");
    let readonly = GraphStore::open_read_only(&root).expect("read published");
    assert_eq!(readonly.list_areas(Some(1)).expect("areas"), vec![new_area]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reset_removes_stale_staging_store() {
    let root = tmp_root(function_name!());
    let staging = GraphStore::reset_staging(&root).expect("open staging");
    let staging_path = GraphStore::staging_path(&root);
    assert!(staging_path.exists(), "staging file exists");
    drop(staging);

    let store = GraphStore::reset(&root).expect("reset public");
    assert!(!staging_path.exists(), "normal reset cleans stale staging");
    assert!(store.path().exists(), "public store exists");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn drop_without_commit_loses_writes() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    {
        let mut session = store.begin_index().expect("session");
        let node = SampleNode {
            id: "x".into(),
            path: "x.rs".into(),
        };
        session.insert_node(FILES, &node.id, &node).expect("insert");
        // session dropped without commit — txn aborts
    }
    assert!(
        read_node_bytes(store.db(), FILES, "x").is_none(),
        "uncommitted writes must not be visible"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn manual_rotate_persists_then_continues() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");

    let a = SampleNode {
        id: "a".into(),
        path: "a.rs".into(),
    };
    session.insert_node(FILES, &a.id, &a).expect("insert a");
    session.rotate().expect("rotate");

    // After rotate, `a` is durable; subsequent inserts continue in a fresh txn.
    assert!(
        read_node_bytes(store.db(), FILES, "a").is_some(),
        "a is durable"
    );

    let b = SampleNode {
        id: "b".into(),
        path: "b.rs".into(),
    };
    session.insert_node(FILES, &b.id, &b).expect("insert b");
    session.commit().expect("commit");

    assert!(
        read_node_bytes(store.db(), FILES, "b").is_some(),
        "b after second commit"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn secondary_indexes_record_node_ids() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");

    session
        .add_path_index(FUNCTIONS_BY_PATH, "src/lib.rs", "fn:Repo:src/lib.rs:foo")
        .expect("path");
    session
        .add_symbol_index("foo", "fn:Repo:src/lib.rs:foo")
        .expect("name");
    session
        .add_symbol_component_index("foo", "fn:Repo:src/lib.rs:foo")
        .expect("component");
    session
        .add_symbol_path_component_index("lib", "fn:Repo:src/lib.rs:foo")
        .expect("path component");
    session.commit().expect("commit");

    let txn = store.db().begin_read().expect("read");
    let by_path = txn
        .open_multimap_table(FUNCTIONS_BY_PATH)
        .expect("FUNCTIONS_BY_PATH");
    let path_hits: Vec<String> = by_path
        .get("src/lib.rs")
        .expect("get")
        .map(|r| r.expect("row").value().to_string())
        .collect();
    assert_eq!(path_hits, vec!["fn:Repo:src/lib.rs:foo"]);

    let by_name = txn
        .open_multimap_table(SYMBOL_BY_NAME)
        .expect("SYMBOL_BY_NAME");
    let name_hits: Vec<String> = by_name
        .get("foo")
        .expect("get")
        .map(|r| r.expect("row").value().to_string())
        .collect();
    assert_eq!(name_hits, vec!["fn:Repo:src/lib.rs:foo"]);

    let by_component = txn
        .open_multimap_table(SYMBOL_BY_COMPONENT)
        .expect("SYMBOL_BY_COMPONENT");
    let component_hits: Vec<String> = by_component
        .get("foo")
        .expect("get")
        .map(|r| r.expect("row").value().to_string())
        .collect();
    assert_eq!(component_hits, vec!["fn:Repo:src/lib.rs:foo"]);

    let by_path_component = txn
        .open_multimap_table(SYMBOL_BY_PATH_COMPONENT)
        .expect("SYMBOL_BY_PATH_COMPONENT");
    let path_component_hits: Vec<String> = by_path_component
        .get("lib")
        .expect("get")
        .map(|r| r.expect("row").value().to_string())
        .collect();
    assert_eq!(path_component_hits, vec!["fn:Repo:src/lib.rs:foo"]);

    let _ = std::fs::remove_dir_all(&root);
}

use crate::model::file::FileRole;
use crate::model::risk::{RiskArea, RiskLevel};

fn sample_file(repo: &str, path: &str, area_id: Option<&str>) -> FileNode {
    FileNode::new(
        repo,
        path,
        Some("rust".into()),
        FileRole::Source,
        42,
        1024,
        false,
        area_id.map(|s| s.to_string()),
    )
}

fn sample_directory(repo: &str, path: &str, area_id: Option<&str>) -> DirectoryNode {
    DirectoryNode::new(repo, path, area_id.map(|s| s.to_string()))
}

fn sample_class(file: &FileNode, name: &str) -> ClassNode {
    ClassNode::new(
        "Repo",
        InternedStr::from(file.id.clone()),
        InternedStr::from(file.path.clone()),
        file.area_id.clone().map(InternedStr::from),
        InternedStr::from(
            file.language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        InternedStr::from(name),
        7,
        InternedStr::from(format!("class {name}")),
    )
}

fn sample_function(
    file: &FileNode,
    name: &str,
    parent_class_id: Option<InternedStr>,
) -> FunctionNode {
    FunctionNode::new(
        "Repo",
        InternedStr::from(file.id.clone()),
        InternedStr::from(file.path.clone()),
        file.area_id.clone().map(InternedStr::from),
        parent_class_id,
        InternedStr::from(
            file.language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        InternedStr::from(name),
        12,
        InternedStr::from(format!("def {name}()")),
    )
}

fn sample_unresolved(file: &FileNode, name: &str) -> UnresolvedNode {
    UnresolvedNode::new(
        InternedStr::from(format!("unresolved_symbol:Repo:{name}")),
        InternedStr::from(name),
        Some(InternedStr::from("function")),
        InternedStr::from(file.id.clone()),
        InternedStr::from(file.id.clone()),
        InternedStr::from(file.path.clone()),
        file.area_id.clone().map(InternedStr::from),
        InternedStr::from(
            file.language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
    )
}

mod reads;
mod typed_writes;
