//! Binary-level tests for the redb-backed engine CLI surfaces.
//!
//! The fixture writes `.aethyme/graph/` fragments in-process, then exercises
//! `aethyme-engine-cli` as a subprocess. That keeps the repos tiny while still
//! pinning the CLI contract that scripts and playground setup consume.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Instant;

use aethyme_engine::graph::navigation::{
    callees_view, callers_view, children_view, configs_view, docs_view, graph_expand_view,
    graph_overview_view, node_view, parents_view, task_anchors_view, task_expand_view,
    task_next_view, task_scope_view,
};
use aethyme_engine::graph::search::symbol_search;
use aethyme_engine::map::RepositoryMap;
use aethyme_engine::pipeline::{build_context_pack, build_context_pack_with_content};
use aethyme_graph_indexer::{index_repo_to_disk, IndexerContext, WalkOptions};
use aethyme_graph_schema::{Confidence, Edge, EdgeAttributes, EdgeSite, Source};
use aethyme_graph_storage::{bootstrap_repo, read_fragment, write_fragment, Fragment};
use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};

const REPOSITORIES: TableDefinition<&str, &[u8]> = TableDefinition::new("repositories");
const DIRECTORIES: TableDefinition<&str, &[u8]> = TableDefinition::new("directories");
const FUNCTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("functions");
const CLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("classes");
const DOCS: TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
const CONFIGS: TableDefinition<&str, &[u8]> = TableDefinition::new("configs");
const UNRESOLVED: TableDefinition<&str, &[u8]> = TableDefinition::new("unresolved");
const EDGES_OUT: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_out");
const EDGES_IN: MultimapTableDefinition<&str, &[u8]> = MultimapTableDefinition::new("edges_in");
const FUNCTIONS_BY_PATH: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("functions_by_path");
const SYMBOL_BY_NAME: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_name");
const SYMBOL_BY_PATH_COMPONENT: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_path_component");

fn engine_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-engine-cli")
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn run_engine<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(engine_bin())
        .args(args)
        .output()
        .expect("spawn aethyme-engine-cli")
}

fn run_engine_with_env<I, S>(args: I, env_key: &str, env_value: &str) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(engine_bin())
        .args(args)
        .env(env_key, env_value)
        .output()
        .expect("spawn aethyme-engine-cli")
}

fn run_engine_timed<I, S>(args: I) -> (Output, u128)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let start = Instant::now();
    let output = run_engine(args);
    (output, start.elapsed().as_millis())
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_failure(output: &Output) {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_duration_below(label: &str, elapsed_ms: u128, env_key: &str, default_ms: u128) {
    let limit_ms = env::var(env_key)
        .ok()
        .and_then(|raw| raw.parse::<u128>().ok())
        .unwrap_or(default_ms);
    assert!(
        elapsed_ms <= limit_ms,
        "{label} took {elapsed_ms}ms, above {limit_ms}ms ({env_key})"
    );
}

fn build_fragment_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/auth/token.py",
        b"SECRET_TOKEN = 'test'\n\ndef load_token():\n    return SECRET_TOKEN\n",
    );
    write(
        tmp.path(),
        "tests/test_token.py",
        b"from src.auth.token import load_token\n\ndef test_token():\n    assert load_token()\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("TinyRepo", root.clone(), "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 2);
    tmp
}

fn build_unresolved_fragment_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/app.py",
        b"import missing_sdk\n\n\ndef main():\n    return missing_sdk.run()\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("UnresolvedRepo", root.clone(), "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 1);
    tmp
}

fn build_medium_fragment_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/auth/token.py",
        b"class TokenLoader:\n    def load(self):\n        return load_token()\n\ndef load_token():\n    return 'token'\n",
    );
    write(
        tmp.path(),
        "src/web/handler.py",
        b"from src.auth.token import load_token\n\ndef handle_request():\n    return load_token()\n",
    );
    write(
        tmp.path(),
        "src/cli/main.py",
        b"from src.web.handler import handle_request\n\ndef main():\n    return handle_request()\n",
    );
    write(
        tmp.path(),
        "docs/auth.md",
        b"# Auth\n\nToken loading notes.\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"medium-fixture\"\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("MediumRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 5);
    tmp
}

fn build_medium_redb_fixture() -> tempfile::TempDir {
    let tmp = build_medium_fragment_fixture();
    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    assert!(tmp.path().join(".aethyme/graph_store.redb").is_file());
    tmp
}

fn build_task_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", b"# Task Fixture\n");
    write(
        tmp.path(),
        "docs/architecture.md",
        b"# Architecture\n\nAuth and web flow notes.\n",
    );
    write(
        tmp.path(),
        "src/auth/token.py",
        b"class TokenLoader:\n    def load(self):\n        return load_token()\n\ndef load_token():\n    return 'token'\n",
    );
    write(
        tmp.path(),
        "src/web/handler.py",
        b"from src.auth.token import load_token\n\ndef handle_request():\n    return load_token()\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"task-fixture\"\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("TaskRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 5);

    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    assert!(tmp.path().join(".aethyme/graph_store.redb").is_file());
    tmp
}

fn build_expand_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/calls.py",
        b"def callee():\n    return 'callee'\n\ndef caller():\n    return callee()\n",
    );
    write(
        tmp.path(),
        "src/wide.py",
        b"def f00():\n    return 0\n\ndef f01():\n    return 1\n\ndef f02():\n    return 2\n\ndef f03():\n    return 3\n\ndef f04():\n    return 4\n\ndef f05():\n    return 5\n\ndef f06():\n    return 6\n\ndef f07():\n    return 7\n\ndef f08():\n    return 8\n\ndef f09():\n    return 9\n\ndef f10():\n    return 10\n\ndef f11():\n    return 11\n",
    );
    for index in 0..12 {
        write(
            tmp.path(),
            &format!("src/dir{index:02}/mod.py"),
            b"def marker():\n    return True\n",
        );
    }
    write(
        tmp.path(),
        "docs/calls.md",
        b"# Calls\n\nCall graph fixture notes.\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"expand-fixture\"\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("ExpandRepo", root.clone(), "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 16);
    add_call_edge_to_fragment(&root, "src/calls.py", "caller", "callee");

    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    tmp
}

fn build_usage_boundary_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "includes/Watchlist/Store.php",
        b"<?php\nclass Store {\n    public function externalUsed() {}\n    public function internalOnly() {}\n    public function unusedMethod() {}\n    public function docsOnly() {}\n    public function configOnly() {}\n}\nclass Manager {\n    private function run($store) { $store->internalOnly(); }\n}\n",
    );
    write(
        tmp.path(),
        "includes/Api/Controller.php",
        b"<?php\nclass Controller {\n    public function handle($store) { $store->externalUsed(); }\n}\n",
    );
    write(
        tmp.path(),
        "docs/watchlist.md",
        b"# Watchlist\n\nThe docsOnly hook is configured by operations.\n",
    );
    write(
        tmp.path(),
        "config/watchlist.yaml",
        b"watchlist:\n  callback: configOnly\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("UsageBoundaryRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 4);

    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    assert!(tmp.path().join(".aethyme/graph_store.redb").is_file());
    tmp
}

fn build_redb_fixture() -> tempfile::TempDir {
    let tmp = build_fragment_fixture();
    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    assert!(tmp.path().join(".aethyme/graph_store.redb").is_file());
    tmp
}

fn add_call_edge_to_fragment(root: &Path, source_path: &str, caller: &str, callee: &str) {
    let fragment = read_fragment(root, source_path).expect("read fragment");
    let caller_id = fragment
        .nodes()
        .iter()
        .find(|node| node.name() == Some(caller))
        .expect("caller node")
        .id()
        .clone();
    let callee_id = fragment
        .nodes()
        .iter()
        .find(|node| node.name() == Some(callee))
        .expect("callee node")
        .id()
        .clone();
    let mut edges = fragment.edges().to_vec();
    edges.push(
        Edge::new(
            caller_id,
            callee_id,
            EdgeAttributes::Calls,
            Source::Code,
            Confidence::FULL,
        )
        .with_site(EdgeSite {
            line: 5,
            is_in_branch: false,
            is_in_loop: false,
            kind_tag: "direct".into(),
        }),
    );
    let updated = Fragment::new(source_path, fragment.nodes().to_vec(), edges).expect("fragment");
    write_fragment(root, source_path, &updated).expect("write fragment");
}

fn build_symbol_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/auth/token.py",
        b"class TokenLoader:\n    def load(self):\n        return load_token()\n\ndef load_token():\n    return 'token'\n",
    );
    write(
        tmp.path(),
        "docs/auth.md",
        b"# Auth\n\nToken loading notes.\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"token-fixture\"\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("TinyRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 3);

    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    tmp
}

fn open_store(repo: &Path) -> Database {
    Database::open(repo.join(".aethyme/graph_store.redb")).expect("open redb store")
}

fn table_has_row(db: &Database, table: TableDefinition<&str, &[u8]>, key: &str) -> bool {
    let txn = db.begin_read().expect("read txn");
    let table = txn.open_table(table).expect("open table");
    table.get(key).expect("get row").is_some()
}

fn table_row_count(db: &Database, table: TableDefinition<&str, &[u8]>) -> usize {
    let txn = db.begin_read().expect("read txn");
    let table = txn.open_table(table).expect("open table");
    let mut count = 0;
    for row in table.iter().expect("iter table") {
        row.expect("row");
        count += 1;
    }
    count
}

fn table_keys(db: &Database, table: TableDefinition<&str, &[u8]>) -> Vec<String> {
    let txn = db.begin_read().expect("read txn");
    let table = txn.open_table(table).expect("open table");
    table
        .iter()
        .expect("iter table")
        .map(|row| row.expect("row").0.value().to_string())
        .collect()
}

fn str_multimap_values(
    db: &Database,
    table: MultimapTableDefinition<&str, &str>,
    key: &str,
) -> Vec<String> {
    let txn = db.begin_read().expect("read txn");
    let table = txn.open_multimap_table(table).expect("open multimap");
    table
        .get(key)
        .expect("get values")
        .map(|row| row.expect("row").value().to_string())
        .collect()
}

fn bytes_multimap_count(
    db: &Database,
    table: MultimapTableDefinition<&str, &[u8]>,
    key: &str,
) -> usize {
    let txn = db.begin_read().expect("read txn");
    let table = txn.open_multimap_table(table).expect("open multimap");
    table
        .get(key)
        .expect("get values")
        .map(|row| row.expect("row"))
        .count()
}

fn query_area_prefixes(repo: &Path) -> Vec<String> {
    let output = run_engine([
        "query-areas",
        "--repo",
        repo.to_str().unwrap(),
        "--depth",
        "1",
    ]);
    assert_success(&output);
    let areas: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("query-areas JSON parses");
    areas
        .as_array()
        .expect("areas array")
        .iter()
        .map(|area| {
            area["path_prefix"]
                .as_str()
                .expect("path_prefix")
                .to_string()
        })
        .collect()
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn query_json<I, S>(args: I) -> serde_json::Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_engine(args);
    assert_success(&output);
    serde_json::from_slice(&output.stdout).expect("JSON parses")
}

fn graph_cli_json(repo: &Path, command: &str, target: &str) -> serde_json::Value {
    query_json([
        command,
        "--repo",
        repo.to_str().unwrap(),
        "--target",
        target,
    ])
}

fn graph_overview_cli_json(repo: &Path) -> serde_json::Value {
    query_json(["graph-overview", "--repo", repo.to_str().unwrap()])
}

fn task_cli_json(repo: &Path, command: &str, task: &str) -> serde_json::Value {
    query_json([command, "--repo", repo.to_str().unwrap(), "--task", task])
}

fn task_expand_cli_json(repo: &Path, target: &str) -> serde_json::Value {
    query_json([
        "task-expand",
        "--repo",
        repo.to_str().unwrap(),
        "--target",
        target,
    ])
}

fn context_pack_cli_json(repo: &Path, command: &str, task: &str) -> serde_json::Value {
    query_json([command, "--repo", repo.to_str().unwrap(), "--task", task])
}

fn context_with_content_cli_json(repo: &Path, command: &str, task: &str) -> serde_json::Value {
    query_json([
        command,
        "--repo",
        repo.to_str().unwrap(),
        "--task",
        task,
        "--content-budget",
        "4096",
    ])
}

fn repository_map_graph_json(
    map: &RepositoryMap,
    command: &str,
    target: &str,
) -> serde_json::Value {
    let json = match command {
        "graph-node" => aethyme_engine::json::graph_node_view(
            &node_view(map, target).expect("RepositoryMap node view"),
        ),
        "graph-children" => aethyme_engine::json::graph_relation(&children_view(map, target)),
        "graph-parents" => aethyme_engine::json::graph_relation(&parents_view(map, target)),
        "graph-callers" => aethyme_engine::json::graph_relation(&callers_view(map, target)),
        "graph-callees" => aethyme_engine::json::graph_relation(&callees_view(map, target)),
        "graph-docs" => aethyme_engine::json::graph_relation(&docs_view(map, target)),
        "graph-configs" => aethyme_engine::json::graph_relation(&configs_view(map, target)),
        "graph-expand" => aethyme_engine::json::graph_expand_view(
            &graph_expand_view(map, target).expect("RepositoryMap expand view"),
        ),
        other => panic!("unsupported graph command: {other}"),
    };
    serde_json::from_str(&json).expect("RepositoryMap graph JSON parses")
}

fn repository_map_graph_overview_json(map: &RepositoryMap) -> serde_json::Value {
    let json = aethyme_engine::json::repo_overview_view(&graph_overview_view(map));
    serde_json::from_str(&json).expect("RepositoryMap graph-overview JSON parses")
}

fn repository_map_task_json(map: &RepositoryMap, command: &str, task: &str) -> serde_json::Value {
    let task = aethyme_engine::model::task::TaskInput::from_task_text(task);
    let json = match command {
        "task-anchors" => aethyme_engine::json::task_anchors_view(&task_anchors_view(map, &task)),
        "task-scope" => aethyme_engine::json::task_scope_view(&task_scope_view(map, &task)),
        "task-next" => aethyme_engine::json::graph_relation(&task_next_view(map, &task)),
        "task-localize" => {
            let anchors = task_anchors_view(map, &task);
            let scope = task_scope_view(map, &task);
            let next = task_next_view(map, &task);
            aethyme_engine::json::task_localization_view(&anchors, &scope, &next)
        }
        other => panic!("unsupported task command: {other}"),
    };
    serde_json::from_str(&json).expect("RepositoryMap task JSON parses")
}

fn repository_map_task_expand_json(map: &RepositoryMap, target: &str) -> serde_json::Value {
    let json = aethyme_engine::json::task_expand_view(&task_expand_view(map, target));
    serde_json::from_str(&json).expect("RepositoryMap task-expand JSON parses")
}

fn repository_map_context_pack_json(
    repo: &Path,
    map: &RepositoryMap,
    task: &str,
) -> serde_json::Value {
    let pack = build_context_pack(
        repo,
        map,
        aethyme_engine::model::task::TaskInput::from_task_text(task),
    );
    let json = aethyme_engine::json::context_pack(&pack);
    serde_json::from_str(&json).expect("RepositoryMap context pack JSON parses")
}

fn repository_map_context_with_content_json(
    repo: &Path,
    map: &RepositoryMap,
    task: &str,
) -> serde_json::Value {
    let pack = build_context_pack_with_content(
        repo,
        map,
        aethyme_engine::model::task::TaskInput::from_task_text(task),
        4096,
    );
    let json = aethyme_engine::json::context_pack(&pack);
    serde_json::from_str(&json).expect("RepositoryMap context pack with content JSON parses")
}

fn symbol_cli_hits(repo: &Path, query: &str, limit: usize) -> serde_json::Value {
    let limit = limit.to_string();
    query_json([
        "symbol-batch",
        "--repo",
        repo.to_str().unwrap(),
        "--query",
        query,
        "--limit",
        limit.as_str(),
    ])[query]
        .clone()
}

fn hit_names(hits: &serde_json::Value) -> Vec<String> {
    hits.as_array()
        .expect("hits array")
        .iter()
        .map(|hit| hit["name"].as_str().expect("hit name").to_string())
        .collect()
}

fn hit_name_kinds(hits: &serde_json::Value) -> Vec<(String, String)> {
    hits.as_array()
        .expect("hits array")
        .iter()
        .map(|hit| {
            (
                hit["name"].as_str().expect("hit name").to_string(),
                hit["kind"].as_str().expect("hit kind").to_string(),
            )
        })
        .collect()
}

fn dead_code_item_by_name<'a>(items: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    items
        .iter()
        .find(|item| item["function"]["name"] == name)
        .unwrap_or_else(|| panic!("missing dead-code item for {name}"))
}

fn object_keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn stable_redb_query_snapshot(repo: &Path) -> serde_json::Value {
    let mut overview = query_json(["query-overview", "--repo", repo.to_str().unwrap()]);
    if let Some(repo) = overview
        .get_mut("repo")
        .and_then(|value| value.as_object_mut())
    {
        repo.insert("indexed_at_unix".to_string(), serde_json::json!(0));
    }

    let deps = run_engine([
        "deps",
        "--repo",
        repo.to_str().unwrap(),
        "--file",
        "tests/test_token.py",
    ]);
    assert_success(&deps);
    let importers = run_engine([
        "importers",
        "--repo",
        repo.to_str().unwrap(),
        "--file",
        "src/auth/token.py",
    ]);
    assert_success(&importers);

    serde_json::json!({
        "overview": overview,
        "graph_overview": graph_overview_cli_json(repo),
        "areas": query_json([
            "query-areas",
            "--repo",
            repo.to_str().unwrap(),
            "--depth",
            "1",
        ]),
        "symbol": query_json([
            "symbol",
            "--repo",
            repo.to_str().unwrap(),
            "--query",
            "load_token",
        ]),
        "deps": stdout_lines(&deps),
        "importers": stdout_lines(&importers),
    })
}

#[test]
fn index_creates_graph_store_redb() {
    let tmp = build_fragment_fixture();

    let output = run_engine(["index", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&output);

    let store_path = tmp.path().join(".aethyme/graph_store.redb");
    assert!(store_path.is_file(), "missing {}", store_path.display());
}

#[test]
fn normal_index_removes_stale_staging_store() {
    let tmp = build_fragment_fixture();
    let staging_path = tmp.path().join(".aethyme/graph_store.redb.indexing");
    std::fs::write(&staging_path, b"stale staged store").unwrap();
    assert!(staging_path.exists(), "test setup creates stale staging");

    let output = run_engine(["index", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&output);

    assert!(tmp.path().join(".aethyme/graph_store.redb").is_file());
    assert!(
        !staging_path.exists(),
        "normal index must remove stale {}",
        staging_path.display()
    );
}

#[test]
fn query_areas_reads_existing_store() {
    let tmp = build_redb_fixture();

    let output = run_engine([
        "query-areas",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--depth",
        "1",
    ]);
    assert_success(&output);

    let areas: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("query-areas JSON parses");
    let prefixes: Vec<&str> = areas
        .as_array()
        .expect("areas array")
        .iter()
        .map(|area| area["path_prefix"].as_str().expect("path_prefix"))
        .collect();
    assert_eq!(prefixes, vec!["src", "tests"]);
}

#[test]
fn index_populates_symbol_tables_and_symbol_edges() {
    let tmp = build_symbol_redb_fixture();
    let db = open_store(tmp.path());
    let function_hits = str_multimap_values(&db, SYMBOL_BY_NAME, "load_token");
    let function_id = function_hits
        .first()
        .expect("load_token should be indexed by name");
    let class_hits = str_multimap_values(&db, SYMBOL_BY_NAME, "tokenloader");
    let class_id = class_hits
        .first()
        .expect("TokenLoader should be indexed by name");

    assert!(table_has_row(&db, FUNCTIONS, function_id.as_str()));
    assert!(table_has_row(&db, CLASSES, class_id.as_str()));
    assert!(
        table_row_count(&db, DOCS) > 0,
        "docs table should be populated"
    );
    assert!(
        table_row_count(&db, CONFIGS) > 0,
        "configs table should be populated"
    );

    assert!(str_multimap_values(&db, FUNCTIONS_BY_PATH, "src/auth/token.py").contains(function_id));
    assert!(
        str_multimap_values(&db, SYMBOL_BY_PATH_COMPONENT, "auth").contains(function_id),
        "symbol path-component index should support bounded path fuzzy lookup"
    );
    assert!(
        bytes_multimap_count(&db, EDGES_IN, function_id.as_str()) > 0,
        "symbol endpoint should have incoming adjacency"
    );
}

#[test]
fn index_persists_complete_node_shape_and_unresolved_edges() {
    let tmp = build_unresolved_fragment_fixture();
    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("skipped (unpersisted endpoints)"),
        "edge writer should no longer skip unresolved endpoints: {stderr}"
    );

    let db = open_store(tmp.path());
    assert_eq!(table_row_count(&db, REPOSITORIES), 1);
    assert!(
        table_row_count(&db, DIRECTORIES) > 0,
        "directory/container rows should be populated"
    );
    let unresolved_ids = table_keys(&db, UNRESOLVED);
    assert!(
        !unresolved_ids.is_empty(),
        "missing import fixture should produce unresolved placeholder rows"
    );

    for unresolved_id in unresolved_ids {
        let adjacency = bytes_multimap_count(&db, EDGES_IN, &unresolved_id)
            + bytes_multimap_count(&db, EDGES_OUT, &unresolved_id);
        assert!(
            adjacency > 0,
            "unresolved node {unresolved_id} should participate in adjacency"
        );
    }
}

#[test]
fn symbol_command_uses_redb_v2_lookup_when_fragments_are_unavailable() {
    let tmp = build_symbol_redb_fixture();
    std::fs::remove_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let output = run_engine([
        "symbol",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--query",
        "load token",
    ]);
    assert_success(&output);

    let hits: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("symbol JSON parses");
    let hits = hits.as_array().expect("hits array");
    assert!(!hits.is_empty(), "expected redb symbol hit");
    assert_eq!(hits[0]["name"], "load_token");
    assert_eq!(hits[0]["kind"], "function");
    let reason = hits[0]["reason"].as_str().expect("reason");
    assert!(reason.starts_with("redb-symbol-search:"));
    assert!(
        reason.contains("component-name"),
        "expected component signal in reason, got {reason}"
    );
    assert!(hits[0]["score"].as_i64().expect("score") > 0);
}

#[test]
fn symbol_batch_uses_redb_v2_lookup_when_fragments_are_unavailable() {
    let tmp = build_symbol_redb_fixture();
    std::fs::remove_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let output = run_engine([
        "symbol-batch",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--query",
        "load_token",
        "--query",
        "TokenLoader",
        "--limit",
        "5",
    ]);
    assert_success(&output);

    let results: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("symbol-batch JSON parses");
    let load_hits = results["load_token"].as_array().expect("load_token hits");
    let class_hits = results["TokenLoader"].as_array().expect("TokenLoader hits");
    assert_eq!(load_hits[0]["name"], "load_token");
    assert_eq!(class_hits[0]["name"], "TokenLoader");
    assert_eq!(class_hits[0]["kind"], "class");
}

fn assert_redb_symbol_parity_with_repository_map(repo: &Path, queries: &[&str], limit: usize) {
    let map = RepositoryMap::build(repo).expect("build RepositoryMap parity oracle");
    for query in queries {
        let expected = symbol_search(&map, query, limit)
            .into_iter()
            .map(|hit| (hit.name, hit.kind))
            .collect::<Vec<_>>();
        let actual = hit_name_kinds(&symbol_cli_hits(repo, query, limit));
        assert_eq!(
            actual, expected,
            "redb V2 symbol search should match RepositoryMap fuzzy scorer for query {query:?}"
        );
    }
}

#[test]
fn redb_symbol_search_matches_repository_map_fuzzy_scorer_on_tiny_fixture() {
    let tmp = build_redb_fixture();

    assert_redb_symbol_parity_with_repository_map(tmp.path(), &["load token", "auth", "token"], 5);
}

#[test]
fn redb_symbol_search_matches_repository_map_fuzzy_scorer_on_medium_fixture() {
    let tmp = build_medium_redb_fixture();

    assert_redb_symbol_parity_with_repository_map(
        tmp.path(),
        &["handle request", "auth", "token"],
        5,
    );
}

#[test]
fn redb_symbol_search_ordering_is_deterministic() {
    let tmp = build_medium_redb_fixture();

    let first = symbol_cli_hits(tmp.path(), "token", 10);
    let second = symbol_cli_hits(tmp.path(), "token", 10);
    assert_eq!(
        first, second,
        "same store/query should produce stable order"
    );

    let rebuild = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&rebuild);
    let after_rebuild = symbol_cli_hits(tmp.path(), "token", 10);
    assert_eq!(
        first, after_rebuild,
        "same fragments should rebuild to the same symbol ordering"
    );
}

fn assert_rendered_graph_command_parity(repo: &Path, targets: &[&str]) {
    let map = RepositoryMap::build(repo).expect("build RepositoryMap parity oracle");
    let commands = [
        "graph-node",
        "graph-children",
        "graph-parents",
        "graph-callers",
        "graph-callees",
        "graph-docs",
        "graph-configs",
        "graph-expand",
    ];

    for target in targets {
        for command in commands {
            let expected = repository_map_graph_json(&map, command, target);
            let actual = graph_cli_json(repo, command, target);
            assert_eq!(
                actual, expected,
                "{command} should preserve RepositoryMap JSON for target {target:?}"
            );
        }
    }
}

#[test]
fn rendered_graph_commands_match_repository_map_snapshots_on_tiny_fixture() {
    let tmp = build_redb_fixture();

    assert_rendered_graph_command_parity(tmp.path(), &["load_token", "src/auth/token.py"]);
}

#[test]
fn rendered_graph_commands_match_repository_map_snapshots_on_medium_fixture() {
    let tmp = build_medium_redb_fixture();

    assert_rendered_graph_command_parity(tmp.path(), &["load_token", "src/auth/token.py"]);
}

fn assert_graph_overview_parity(repo: &Path) {
    let map = RepositoryMap::build(repo).expect("build RepositoryMap graph-overview oracle");
    let expected = repository_map_graph_overview_json(&map);
    let actual = graph_overview_cli_json(repo);
    assert_eq!(
        actual, expected,
        "graph-overview should preserve RepositoryMap JSON"
    );
}

#[test]
fn graph_overview_matches_repository_map_snapshot_on_tiny_fixture() {
    let tmp = build_redb_fixture();

    assert_graph_overview_parity(tmp.path());
}

#[test]
fn graph_overview_matches_repository_map_snapshot_on_medium_fixture() {
    let tmp = build_medium_redb_fixture();

    assert_graph_overview_parity(tmp.path());
}

#[test]
fn task_expand_command_matches_repository_map_snapshot_on_relation_fixture() {
    let tmp = build_expand_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap task-expand oracle");

    for target in ["caller", "callee", "pyproject.toml"] {
        let expected = repository_map_task_expand_json(&map, target);
        let actual = task_expand_cli_json(tmp.path(), target);
        assert_eq!(
            actual, expected,
            "task-expand should preserve RepositoryMap JSON for target {target:?}"
        );
    }
}

fn assert_task_command_parity(repo: &Path, tasks: &[&str]) {
    let map = RepositoryMap::build(repo).expect("build RepositoryMap task parity oracle");
    let commands = ["task-anchors", "task-scope", "task-next", "task-localize"];

    for task in tasks {
        for command in commands {
            let expected = repository_map_task_json(&map, command, task);
            let actual = task_cli_json(repo, command, task);
            assert_eq!(
                actual, expected,
                "{command} should preserve RepositoryMap JSON for task {task:?}"
            );
        }
    }
}

#[test]
fn redb_task_views_match_repository_map_snapshots_for_phase6_task_kinds() {
    let tmp = build_task_redb_fixture();

    assert_task_command_parity(
        tmp.path(),
        &[
            "Explain this repo",
            "Update load_token flow",
            "Trace impact of load_token",
            "Find the manifest that owns the top-level area",
        ],
    );
}

fn context_pack_metrics(value: &serde_json::Value) -> (usize, Vec<String>, Vec<String>, usize) {
    let serialized_size = value.to_string().len();
    let files = value["in_scope"]["files"]
        .as_array()
        .expect("in_scope files")
        .iter()
        .map(|item| item["value"].as_str().expect("file value").to_string())
        .collect::<Vec<_>>();
    let symbols = value["in_scope"]["symbols"]
        .as_array()
        .expect("in_scope symbols")
        .iter()
        .map(|item| item["value"].as_str().expect("symbol value").to_string())
        .collect::<Vec<_>>();
    let snippet_count = value["snippets"].as_array().expect("snippets").len();
    (serialized_size, files, symbols, snippet_count)
}

fn assert_context_pack_parity(repo: &Path, tasks: &[&str]) {
    let map = RepositoryMap::build(repo).expect("build RepositoryMap context-pack oracle");
    for task in tasks {
        let expected = repository_map_context_pack_json(repo, &map, task);
        let actual = context_pack_cli_json(repo, "pack", task);
        assert_eq!(
            actual, expected,
            "pack should preserve RepositoryMap JSON for task {task:?}"
        );
        assert_eq!(
            context_pack_cli_json(repo, "task-pack", task),
            actual,
            "task-pack should be a redb-backed alias for pack"
        );
    }
}

#[test]
fn redb_context_pack_matches_repository_map_snapshots_for_phase2_tasks() {
    let tmp = build_task_redb_fixture();

    assert_context_pack_parity(
        tmp.path(),
        &[
            "Explain this repo",
            "Update load_token flow",
            "Trace impact of load_token",
            "Find the manifest that owns the top-level area",
        ],
    );
}

#[test]
fn redb_context_command_matches_repository_map_snapshot_with_content() {
    let tmp = build_task_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap context oracle");
    let task = "Update load_token flow";

    let expected = repository_map_context_with_content_json(tmp.path(), &map, task);
    let actual = context_with_content_cli_json(tmp.path(), "context", task);
    assert_eq!(
        actual, expected,
        "context should preserve RepositoryMap JSON with content"
    );
    assert_eq!(
        context_with_content_cli_json(tmp.path(), "task-context", task),
        actual,
        "task-context should be a redb-backed alias for context"
    );
}

#[test]
fn redb_context_pack_token_regression_gate_on_playground_fixture() {
    let tmp = build_task_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap token oracle");

    for task in [
        "Explain this repo",
        "Update load_token flow",
        "Trace impact of load_token",
        "Find the manifest that owns the top-level area",
    ] {
        let expected = repository_map_context_pack_json(tmp.path(), &map, task);
        let actual = context_pack_cli_json(tmp.path(), "pack", task);
        let (expected_size, expected_files, expected_symbols, expected_snippets) =
            context_pack_metrics(&expected);
        let (actual_size, actual_files, actual_symbols, actual_snippets) =
            context_pack_metrics(&actual);

        assert_eq!(
            actual_files, expected_files,
            "selected files should not drift for task {task:?}"
        );
        assert_eq!(
            actual_symbols, expected_symbols,
            "selected symbols should not drift for task {task:?}"
        );
        assert_eq!(
            actual_snippets, expected_snippets,
            "snippet count should not drift for task {task:?}"
        );
        assert!(
            actual_size <= expected_size.saturating_mul(120) / 100 + 512,
            "redb pack size regressed for task {task:?}: actual={actual_size}, expected={expected_size}"
        );
    }
}

#[test]
fn redb_context_pack_output_is_deterministic() {
    let tmp = build_task_redb_fixture();
    let task = "Trace impact of load_token";
    let first = context_pack_cli_json(tmp.path(), "pack", task);
    let second = context_pack_cli_json(tmp.path(), "pack", task);
    assert_eq!(
        first, second,
        "same redb store should produce stable context-pack output"
    );

    let rebuild = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&rebuild);
    let after_rebuild = context_pack_cli_json(tmp.path(), "pack", task);
    assert_eq!(
        first, after_rebuild,
        "same fragments should rebuild to the same context-pack output"
    );
}

#[test]
fn usage_boundary_uses_redb_seeds_for_callers_and_docs_config_references() {
    let tmp = build_usage_boundary_redb_fixture();
    std::fs::remove_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let output = run_engine([
        "analyze-usage-boundary",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--scope",
        "includes/Watchlist",
        "--include-methods",
        "--budget-ms",
        "5000",
        "--max-evidence-per-symbol",
        "4",
    ]);
    assert_success(&output);
    let answer: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("usage-boundary JSON parses");

    assert_eq!(answer["analyzer"], "usage-boundary");
    assert_eq!(answer["query"]["scope"], "includes/Watchlist");
    assert_eq!(answer["query"]["include_methods"], true);
    assert!(
        answer["observability"]["degraded_reasons"]
            .as_array()
            .expect("degraded reasons")
            .iter()
            .any(|reason| reason == "redb_seed_discovery"),
        "usage-boundary should declare the redb seed path"
    );

    let candidates = answer["candidates"].as_array().expect("candidates");
    let excluded = answer["excluded"].as_array().expect("excluded");

    let used = dead_code_item_by_name(excluded, "externalUsed");
    assert_eq!(used["status"], "Used");
    assert!(
        used["evidence"]["external_callers"]
            .as_array()
            .expect("external callers")
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|value| value.contains("includes/Api/Controller.php"))),
        "external caller evidence should come from source-text scanning over redb-discovered files"
    );

    let internal = dead_code_item_by_name(candidates, "internalOnly");
    assert_eq!(internal["status"], "Ambiguous");
    assert!(internal["ambiguity"]
        .as_array()
        .expect("ambiguity")
        .contains(&serde_json::json!("exported_but_internal_only")));

    let docs_only = dead_code_item_by_name(candidates, "docsOnly");
    assert_eq!(docs_only["status"], "Ambiguous");
    assert!(
        docs_only["evidence"]["docs_config_references"]
            .as_array()
            .expect("docs refs")
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|value| value.contains("docs/watchlist.md"))),
        "doc reference should be retained"
    );

    let config_only = dead_code_item_by_name(candidates, "configOnly");
    assert_eq!(config_only["status"], "Ambiguous");
    assert!(
        config_only["evidence"]["docs_config_references"]
            .as_array()
            .expect("config refs")
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|value| value.contains("config/watchlist.yaml"))),
        "config reference should be retained"
    );

    let unused = dead_code_item_by_name(candidates, "unusedMethod");
    assert_eq!(unused["status"], "Unused");
}

#[test]
fn graph_expand_json_shape_is_stable() {
    let tmp = build_expand_redb_fixture();

    let expand = graph_cli_json(tmp.path(), "graph-expand", "caller");
    assert_eq!(
        object_keys(&expand),
        BTreeSet::from([
            "callees", "callers", "children", "configs", "docs", "parents", "risks", "target",
        ])
    );
    assert_eq!(
        object_keys(&expand["target"]),
        BTreeSet::from([
            "annotations",
            "area",
            "confidence",
            "id",
            "kind",
            "label",
            "language",
            "path",
            "source",
        ])
    );
    assert_eq!(expand["target"]["kind"], "function");
    assert!(expand["risks"].as_array().is_some());

    let callees = expand["callees"].as_array().expect("callees array");
    let first = callees.first().expect("call edge callee");
    assert_eq!(
        object_keys(first),
        BTreeSet::from(["confidence", "display", "id", "kind", "relation"])
    );
}

#[test]
fn graph_expand_reads_docs_configs_and_call_edges_from_redb() {
    let tmp = build_expand_redb_fixture();

    let caller = graph_cli_json(tmp.path(), "graph-expand", "caller");
    assert!(
        !caller["callees"]
            .as_array()
            .expect("callees array")
            .is_empty(),
        "caller should expose a redb-backed callee"
    );

    let callee = graph_cli_json(tmp.path(), "graph-expand", "callee");
    assert!(
        !callee["callers"]
            .as_array()
            .expect("callers array")
            .is_empty(),
        "callee should expose a redb-backed caller"
    );

    let doc = graph_cli_json(tmp.path(), "graph-expand", "docs/calls.md");
    assert!(
        !doc["docs"].as_array().expect("docs array").is_empty(),
        "doc target should expose its documents relation"
    );

    let config = graph_cli_json(tmp.path(), "graph-expand", "pyproject.toml");
    assert!(
        !config["configs"]
            .as_array()
            .expect("configs array")
            .is_empty(),
        "config target should expose its configures relation"
    );
}

#[test]
fn graph_expand_output_is_bounded() {
    let tmp = build_expand_redb_fixture();

    let children = graph_cli_json(tmp.path(), "graph-children", "src");
    assert!(
        children["items"].as_array().expect("children items").len() > 8,
        "fixture should exceed the expand child cap"
    );

    let expand = graph_cli_json(tmp.path(), "graph-expand", "src");
    assert_eq!(expand["children"].as_array().expect("children").len(), 8);
    assert!(expand["parents"].as_array().expect("parents").len() <= 5);
    assert!(expand["callers"].as_array().expect("callers").len() <= 8);
    assert!(expand["callees"].as_array().expect("callees").len() <= 8);
    assert!(expand["docs"].as_array().expect("docs").len() <= 5);
    assert!(expand["configs"].as_array().expect("configs").len() <= 5);
}

#[test]
fn graph_expand_ordering_is_deterministic() {
    let tmp = build_expand_redb_fixture();

    let first = graph_cli_json(tmp.path(), "graph-expand", "src");
    let second = graph_cli_json(tmp.path(), "graph-expand", "src");
    assert_eq!(first, second);

    let rebuild = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&rebuild);
    let after_rebuild = graph_cli_json(tmp.path(), "graph-expand", "src");
    assert_eq!(first, after_rebuild);
}

#[test]
fn medium_fixture_indexes_and_queries_symbol_callers_and_callees() {
    let tmp = build_medium_redb_fixture();

    let symbol_hits = query_json([
        "symbol",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--query",
        "load_token",
    ]);
    let first_hit = symbol_hits
        .as_array()
        .expect("symbol hits array")
        .first()
        .expect("symbol hit");
    let target_id = first_hit["id"].as_str().expect("symbol id");
    assert_eq!(first_hit["name"], "load_token");

    let callers = query_json([
        "graph-callers",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--target",
        target_id,
    ]);
    assert_eq!(callers["target"], target_id);
    assert_eq!(callers["relation"], "callers");
    assert!(
        callers["items"].as_array().is_some(),
        "callers items should be an array"
    );

    let callees = query_json([
        "graph-callees",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--target",
        target_id,
    ]);
    assert_eq!(callees["target"], target_id);
    assert_eq!(callees["relation"], "callees");
    assert!(
        callees["items"].as_array().is_some(),
        "callees items should be an array"
    );
}

#[test]
fn same_fragments_produce_same_redb_query_outputs() {
    let tmp = build_redb_fixture();
    let first = stable_redb_query_snapshot(tmp.path());

    let rebuild = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&rebuild);

    let second = stable_redb_query_snapshot(tmp.path());
    assert_eq!(first, second);
}

#[test]
fn redb_performance_smoke_tiny_fixture() {
    let tmp = build_fragment_fixture();

    let (index_output, index_ms) =
        run_engine_timed(["index", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&index_output);
    assert_duration_below(
        "redb index",
        index_ms,
        "AETHYME_REDB_PERF_MAX_INDEX_MS",
        15_000,
    );

    let (overview_output, overview_ms) =
        run_engine_timed(["query-overview", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&overview_output);
    assert_duration_below(
        "query-overview",
        overview_ms,
        "AETHYME_REDB_PERF_MAX_QUERY_OVERVIEW_MS",
        2_000,
    );

    let (symbol_output, symbol_ms) = run_engine_timed([
        "symbol",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--query",
        "load_token",
    ]);
    assert_success(&symbol_output);
    assert_duration_below(
        "symbol search",
        symbol_ms,
        "AETHYME_REDB_PERF_MAX_SYMBOL_MS",
        2_000,
    );
    let hits: serde_json::Value =
        serde_json::from_slice(&symbol_output.stdout).expect("symbol JSON parses");
    assert!(!hits.as_array().expect("symbol hits").is_empty());
}

#[test]
#[ignore = "requires AETHYME_MEDIAWIKI_REPO; run when broadening V2 redb graph paths"]
fn mediawiki_scale_redb_smoke_for_v2_paths() {
    let Ok(repo) = env::var("AETHYME_MEDIAWIKI_REPO") else {
        eprintln!("skipping: set AETHYME_MEDIAWIKI_REPO to run MediaWiki-scale redb smoke");
        return;
    };
    let repo = Path::new(&repo);
    assert!(
        repo.is_dir(),
        "MediaWiki repo does not exist: {}",
        repo.display()
    );
    assert!(
        repo.join(".aethyme/graph").is_dir(),
        "MediaWiki-scale gate expects committed fragments under {}",
        repo.join(".aethyme/graph").display()
    );

    let (index_output, index_ms) = run_engine_timed([
        "index",
        "--repo",
        repo.to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&index_output);
    assert_duration_below(
        "MediaWiki redb index",
        index_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_INDEX_MS",
        180_000,
    );

    let (overview_output, overview_ms) =
        run_engine_timed(["query-overview", "--repo", repo.to_str().unwrap()]);
    assert_success(&overview_output);
    assert_duration_below(
        "MediaWiki query-overview",
        overview_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_QUERY_OVERVIEW_MS",
        5_000,
    );

    let (symbol_output, symbol_ms) = run_engine_timed([
        "symbol",
        "--repo",
        repo.to_str().unwrap(),
        "--query",
        "viewing page",
    ]);
    assert_success(&symbol_output);
    assert_duration_below(
        "MediaWiki symbol search",
        symbol_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_SYMBOL_MS",
        5_000,
    );
    let hits: serde_json::Value =
        serde_json::from_slice(&symbol_output.stdout).expect("symbol JSON parses");
    let default_hit_names = hit_names(&hits);
    assert!(
        !default_hit_names.is_empty(),
        "MediaWiki symbol smoke should return default hits for a fuzzy viewing/page query"
    );

    let (broad_symbol_output, broad_symbol_ms) = run_engine_timed([
        "symbol-batch",
        "--repo",
        repo.to_str().unwrap(),
        "--query",
        "viewing page",
        "--limit",
        "1000",
    ]);
    assert_success(&broad_symbol_output);
    assert_duration_below(
        "MediaWiki broad symbol recall",
        broad_symbol_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_BROAD_SYMBOL_MS",
        10_000,
    );
    let broad_hits: serde_json::Value =
        serde_json::from_slice(&broad_symbol_output.stdout).expect("symbol-batch JSON parses");
    let broad_hit_names = hit_names(&broad_hits["viewing page"]);
    assert!(
        broad_hit_names.iter().any(|name| name == "doViewUpdates"),
        "MediaWiki broad symbol smoke should recall doViewUpdates for a fuzzy viewing/page query"
    );

    let (task_output, task_ms) = run_engine_timed([
        "task-localize",
        "--repo",
        repo.to_str().unwrap(),
        "--task",
        "Trace impact of doViewUpdates",
    ]);
    assert_success(&task_output);
    assert_duration_below(
        "MediaWiki task-localize",
        task_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_TASK_LOCALIZE_MS",
        10_000,
    );
    let task: serde_json::Value =
        serde_json::from_slice(&task_output.stdout).expect("task-localize JSON parses");
    assert!(task["anchors"]["anchors"].as_array().is_some());
    assert!(task["scope"]["navigation_order"].as_array().is_some());
    assert!(task["next"]["items"].as_array().is_some());
}

#[cfg(unix)]
#[test]
fn query_areas_reads_with_read_only_graph_store() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = build_redb_fixture();
    let store_path = tmp.path().join(".aethyme/graph_store.redb");
    let mut perms = std::fs::metadata(&store_path).unwrap().permissions();
    perms.set_mode(0o444);
    std::fs::set_permissions(&store_path, perms).unwrap();

    let output = run_engine([
        "query-areas",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--depth",
        "1",
    ]);
    assert_success(&output);
}

#[test]
fn query_overview_json_shape_is_stable() {
    let tmp = build_redb_fixture();

    let output = run_engine(["query-overview", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&output);

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("query-overview JSON parses");
    let obj = parsed.as_object().expect("overview object");
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from(["areas", "entrypoints", "repo", "risks"])
    );

    let repo = parsed["repo"].as_object().expect("repo object");
    let repo_keys: BTreeSet<&str> = repo.keys().map(String::as_str).collect();
    assert_eq!(
        repo_keys,
        BTreeSet::from([
            "commit_hash",
            "file_count",
            "indexed_at_unix",
            "languages",
            "root_path",
        ])
    );
    assert_eq!(parsed["repo"]["file_count"], 2);
    assert!(parsed["repo"]["languages"]
        .as_array()
        .expect("languages array")
        .iter()
        .any(|lang| lang == "python"));

    let areas = parsed["areas"].as_array().expect("areas array");
    assert_eq!(areas.len(), 2);
    let area_keys: BTreeSet<&str> = areas[0]
        .as_object()
        .expect("area object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        area_keys,
        BTreeSet::from(["id", "inferred", "name", "path_prefix"])
    );
    assert!(parsed["entrypoints"].as_array().is_some());
    assert!(parsed["risks"].as_array().is_some());
}

#[test]
fn graph_overview_json_shape_is_stable() {
    let tmp = build_redb_fixture();
    let parsed = graph_overview_cli_json(tmp.path());

    assert_eq!(
        object_keys(&parsed),
        BTreeSet::from([
            "code_areas",
            "entrypoints",
            "key_configs",
            "overview_docs",
            "reference_areas",
            "repo",
            "representative_code_files",
            "representative_docs",
            "signals",
            "subareas",
        ])
    );

    let signals = parsed["signals"].as_object().expect("signals object");
    let signal_keys: BTreeSet<&str> = signals.keys().map(String::as_str).collect();
    assert_eq!(
        signal_keys,
        BTreeSet::from([
            "boundary_clarity",
            "config_hygiene",
            "entrypoint_clarity",
            "hidden_coupling",
            "parser_visibility",
        ])
    );
    for key in signal_keys {
        assert_eq!(
            object_keys(&parsed["signals"][key]),
            BTreeSet::from(["evidence", "level", "score"])
        );
    }
    assert!(parsed["overview_docs"].as_array().is_some());
    assert!(parsed["code_areas"].as_array().is_some());
    assert!(parsed["reference_areas"].as_array().is_some());
    assert!(parsed["subareas"].as_array().is_some());
    assert!(parsed["entrypoints"].as_array().is_some());
    assert!(parsed["key_configs"].as_array().is_some());
    assert!(parsed["representative_code_files"].as_array().is_some());
    assert!(parsed["representative_docs"].as_array().is_some());
}

#[test]
fn graph_overview_query_output_is_deterministic() {
    let tmp = build_medium_redb_fixture();
    let first = graph_overview_cli_json(tmp.path());
    let second = graph_overview_cli_json(tmp.path());
    assert_eq!(
        first, second,
        "same redb store should produce stable graph-overview output"
    );

    let rebuild = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&rebuild);
    let after_rebuild = graph_overview_cli_json(tmp.path());
    assert_eq!(
        first, after_rebuild,
        "same fragments should rebuild to the same graph-overview output"
    );
}

#[test]
fn query_commands_fail_cleanly_and_do_not_create_store_when_missing() {
    let tmp = build_fragment_fixture();
    let store_path = tmp.path().join(".aethyme/graph_store.redb");
    assert!(!store_path.exists());

    let cases: Vec<Vec<&str>> = vec![
        vec!["query-areas", "--repo", tmp.path().to_str().unwrap()],
        vec!["query-overview", "--repo", tmp.path().to_str().unwrap()],
        vec![
            "symbol",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--query",
            "load_token",
        ],
        vec![
            "symbol-batch",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--query",
            "load_token",
        ],
        vec![
            "graph-node",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-children",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-parents",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-callers",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-callees",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-docs",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-configs",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "graph-expand",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec!["graph-overview", "--repo", tmp.path().to_str().unwrap()],
        vec![
            "task-anchors",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-scope",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-next",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-localize",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-expand",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "pack",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-pack",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "context",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "task-context",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec!["explain", "--repo", tmp.path().to_str().unwrap()],
        vec!["task-explain", "--repo", tmp.path().to_str().unwrap()],
        vec![
            "analyze-usage-boundary",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--scope",
            "src",
            "--include-methods",
        ],
        vec![
            "deps",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--file",
            "src/auth/token.py",
        ],
        vec![
            "importers",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--file",
            "src/auth/token.py",
        ],
        vec![
            "callers",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--symbol",
            "load_token",
        ],
    ];

    for args in cases {
        let output = run_engine(args.clone());
        assert_failure(&output);
        assert!(
            output.stdout.is_empty(),
            "missing-store query should not emit stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(".aethyme/graph_store.redb"),
            "stderr={stderr}"
        );
        assert!(
            stderr.contains("aethyme-engine-cli index --repo <repo>"),
            "stderr={stderr}"
        );
        assert!(
            stderr.contains("Query commands are read-only"),
            "stderr={stderr}"
        );
    }

    assert!(
        !store_path.exists(),
        "read-only query commands must not create {}",
        store_path.display()
    );
}

#[test]
fn disposable_fast_only_publishes_after_successful_metadata_write() {
    let tmp = build_redb_fixture();
    let final_path = tmp.path().join(".aethyme/graph_store.redb");
    let staging_path = tmp.path().join(".aethyme/graph_store.redb.indexing");
    assert!(final_path.is_file(), "public store exists before rebuild");
    assert!(!staging_path.exists(), "no staging before rebuild");
    assert_eq!(query_area_prefixes(tmp.path()), vec!["src", "tests"]);

    write(
        tmp.path(),
        "app/main.py",
        b"def main():\n    return 'new top-level area'\n",
    );
    let root = tmp.path().canonicalize().unwrap();
    let ctx = IndexerContext::new("TinyRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(
        &ctx,
        &WalkOptions {
            extra_ignore_dirs: vec![".chau7".to_string()],
            max_file_size_bytes: None,
        },
    )
    .expect("refresh fragments");
    assert_eq!(summary.total_files, 3);

    let output = run_engine_with_env(
        [
            "index",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--disposable-fast",
        ],
        "AETHYME_TEST_FAIL_REDB_METADATA_WRITE",
        "1",
    );
    assert_failure(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("test-injected redb metadata write failure"),
        "stderr={stderr}"
    );

    assert!(
        final_path.is_file(),
        "failed disposable-fast rebuild must leave public store in place"
    );
    assert!(
        staging_path.is_file(),
        "failed disposable-fast rebuild should not publish staging"
    );
    assert_eq!(
        query_area_prefixes(tmp.path()),
        vec!["src", "tests"],
        "public store must still reflect the pre-failure index"
    );
}
