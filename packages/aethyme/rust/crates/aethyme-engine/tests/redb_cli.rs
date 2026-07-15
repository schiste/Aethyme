//! Binary-level tests for the redb-backed engine CLI surfaces.
//!
//! The fixture writes `.aethyme/graph/` fragments in-process, then exercises
//! `aethyme-engine-cli` as a subprocess. That keeps the repos tiny while still
//! pinning the CLI contract that scripts and playground setup consume.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use aethyme_graph_indexer::{index_repo_to_disk, IndexerContext, WalkOptions};
use aethyme_graph_storage::bootstrap_repo;
use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};

const FUNCTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("functions");
const CLASSES: TableDefinition<&str, &[u8]> = TableDefinition::new("classes");
const DOCS: TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
const CONFIGS: TableDefinition<&str, &[u8]> = TableDefinition::new("configs");
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
