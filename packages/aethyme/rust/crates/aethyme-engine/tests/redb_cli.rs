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

use aethyme_graph_indexer::{index_repo_to_disk, IndexerContext, WalkOptions};
use aethyme_graph_storage::bootstrap_repo;
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
fn symbol_command_uses_redb_exact_lookup_when_fragments_are_unavailable() {
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
    assert!(hits[0]["reason"]
        .as_str()
        .expect("reason")
        .starts_with("redb-exact-symbol-name:"));
}

#[test]
fn symbol_batch_uses_redb_exact_lookup_when_fragments_are_unavailable() {
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
        "doViewUpdates",
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
    assert!(
        hits.as_array().is_some(),
        "MediaWiki symbol output should remain a JSON array"
    );
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
