//! Binary-level tests for the redb-backed engine CLI surfaces.
//!
//! The fixture writes `.aethyme/graph/` fragments in-process, then exercises
//! `aethyme-engine-cli` as a subprocess. That keeps the repos tiny while still
//! pinning the CLI contract that scripts and playground setup consume.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use aethyme_engine::graph::activation::{hormone_profile, spread_activation, spread_from_seed};
use aethyme_engine::graph::anchors::resolve_anchors;
use aethyme_engine::graph::navigation::{
    callees_view, callers_view, children_view, configs_view, docs_view, graph_expand_view,
    graph_overview_view, node_view, parents_view, task_anchors_view, task_expand_view,
    task_next_view, task_scope_view,
};
use aethyme_engine::graph::neighborhood::impact_frontier;
use aethyme_engine::graph::search::symbol_search;
use aethyme_engine::map::RepositoryMap;
use aethyme_engine::pipeline::{build_context_pack, build_context_pack_with_content};
use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
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
const SYMBOL_BY_PATH_COMPONENT: MultimapTableDefinition<&str, &str> =
    MultimapTableDefinition::new("symbol_by_path_component");

fn engine_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-engine-cli")
}

use aethyme_testkit::aethyme_bin;

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

fn run_aethyme<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(aethyme_bin())
        .args(args)
        .output()
        .expect("spawn aethyme")
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
    link_repo(&ctx).expect("leave genuinely missing imports unresolved");
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
    link_repo(&ctx).expect("resolve same-module and imported call edges");

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

fn build_duplicate_anchor_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", b"# Duplicate Anchor Fixture\n");
    write(
        tmp.path(),
        "src/auth.py",
        b"def validate_token(token):\n    return token\n\ndef validate_token_claims(token):\n    return token\n",
    );
    write(
        tmp.path(),
        "src/main.py",
        b"from src.auth import validate_token\n\ndef validate_token_request():\n    return validate_token('token')\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("DuplicateAnchorRepo", root, "test-engine").unwrap();
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

fn build_surface_flow_auth_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "gcp-run-proxy/src/worker.mjs",
        br#"
export default {
  async fetch(request, env) {
    const incoming = request.headers.get("Authorization")
    const headers = new Headers(request.headers)
    if (incoming && incoming.startsWith("Bearer pk_")) {
      headers.set("Authorization", incoming)
    } else {
      headers.set("Authorization", `Bearer ${env.PUBLIC_API_FALLBACK_PK}`)
    }
    return fetch(`${env.BACKEND_ORIGIN}/api/projects/`, {
      method: request.method,
      headers,
    })
  }
}
"#,
    );
    write(
        tmp.path(),
        "backend/settings.py",
        br#"
MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
    "backend.api_keys.middleware.PublishableKeyMiddleware",
]

REST_FRAMEWORK = {
    "DEFAULT_AUTHENTICATION_CLASSES": [
        "backend.api_keys.middleware.PublishableKeyAuthentication",
    ],
    "DEFAULT_PERMISSION_CLASSES": [
        "rest_framework.permissions.IsAuthenticated",
    ],
}

OIDC_CLIENT_ID = "oidc-decoy-client"
"#,
    );
    write(
        tmp.path(),
        "backend/urls.py",
        br#"
from django.urls import path
from backend.api_keys.views import project_view

urlpatterns = [
    path("api/projects/", project_view),
]
"#,
    );
    write(
        tmp.path(),
        "backend/api_keys/middleware.py",
        br#"
class PublishableKeyAuthentication:
    def authenticate(self, request):
        header = request.headers.get("Authorization", "")
        if not header.startswith("Bearer pk_"):
            return None
        return validate_publishable_key(header.removeprefix("Bearer "))


class PublishableKeyMiddleware:
    def __call__(self, request):
        request.publishable_key = validate_publishable_key(
            request.headers.get("Authorization", "")
        )
        return self.get_response(request)


def validate_publishable_key(raw_token):
    if not raw_token.startswith("pk_"):
        raise PermissionError("invalid publishable key")
    return raw_token
"#,
    );
    write(
        tmp.path(),
        "backend/api_keys/views.py",
        br#"
from rest_framework.decorators import api_view, permission_classes
from backend.api_keys.middleware import validate_publishable_key


def require_scope(scope):
    def decorator(view):
        view.required_scope = scope
        return view
    return decorator


@api_view(["GET"])
@permission_classes(["IsAuthenticated"])
@require_scope("projects:read")
def project_view(request):
    key = validate_publishable_key(request.headers.get("Authorization", ""))
    if "projects:read" not in key:
        raise PermissionError("missing projects scope")
    return {"ok": True}
"#,
    );
    write(
        tmp.path(),
        "backend/accounts/oidc.py",
        br#"
def verify_oidc_id_token(id_token, provider):
    if provider != "OIDC":
        raise ValueError("wrong provider")
    return {"sub": "user-1"}
"#,
    );
    write(
        tmp.path(),
        "backend/audit/jws.py",
        br#"
def verify_audit_jws(audit_jws):
    return audit_jws.split(".")
"#,
    );
    write(
        tmp.path(),
        "backend/accounts/auth0_management.py",
        br#"
def get_auth0_management_token(client):
    return client.oauth_token("auth0-management")
"#,
    );
    write(
        tmp.path(),
        "backend/accounts/webhook_tokens.py",
        br#"
def verify_webhook_token(signature, payload, secret):
    if not signature.startswith("whsec_"):
        raise PermissionError("invalid webhook token signature")
    return hmac_compare(signature, payload, secret)
"#,
    );
    write(
        tmp.path(),
        "tests/test_proxy_worker.mjs",
        br#"
test("proxy preserves publishable key Authorization", async () => {
  const request = new Request("https://edge.example.test/api/projects/", {
    headers: { Authorization: "Bearer pk_projects_read" },
  })
  const response = await worker.fetch(request, { BACKEND_ORIGIN: "https://backend.test" })
  expect(response.request.headers.get("Authorization")).toEqual("Bearer pk_projects_read")
})
"#,
    );
    write(
        tmp.path(),
        "backend/api_keys/tests/test_backend_auth.py",
        br#"
def test_publishable_key_middleware_validates_pk_prefix(rf):
    request = rf.get("/api/projects/", HTTP_AUTHORIZATION="Bearer pk_projects_read")
    assert validate_publishable_key("pk_projects_read") == "pk_projects_read"


def test_project_route_requires_projects_read_scope(client):
    response = client.get("/api/projects/", HTTP_AUTHORIZATION="Bearer pk_projects_read")
    assert response.status_code == 200
"#,
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("SurfaceFlowAuthRepo", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 11);

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
    link_repo(&ctx).expect("link fragments");

    let output = run_engine([
        "index",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--from-fragments",
    ]);
    assert_success(&output);
    tmp
}

fn build_activation_redb_fixture() -> tempfile::TempDir {
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
        "src/calls.py",
        b"def callee():\n    return 'callee'\n\ndef caller():\n    return callee()\n",
    );
    write(
        tmp.path(),
        "docs/auth.md",
        b"# Auth\n\nToken loading and handler notes.\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"activation-fixture\"\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("ActivationRepo", root.clone(), "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 5);
    link_repo(&ctx).expect("link fragments");

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

fn build_final_v2_medium_redb_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", b"# Final V2 Fixture\n");
    write(
        tmp.path(),
        "docs/architecture.md",
        b"# Architecture\n\nAuth, web, CLI, and watchlist ownership notes.\n",
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
        "src/cli/main.py",
        b"from src.web.handler import handle_request\n\ndef main():\n    return handle_request()\n",
    );
    write(
        tmp.path(),
        "src/calls.py",
        b"def callee():\n    return 'callee'\n\ndef caller():\n    return callee()\n",
    );
    write(
        tmp.path(),
        "pyproject.toml",
        b"[project]\nname = \"final-v2-fixture\"\n",
    );
    write(
        tmp.path(),
        "includes/Watchlist/Store.php",
        b"<?php\nclass Store {\n    public function externalUsed() {}\n    public function internalOnly() {}\n    public function unusedMethod() {}\n}\nclass Manager {\n    private function run($store) { $store->internalOnly(); }\n}\n",
    );
    write(
        tmp.path(),
        "includes/Api/Controller.php",
        b"<?php\nclass Controller {\n    public function handle($store) { $store->externalUsed(); }\n}\n",
    );
    write(
        tmp.path(),
        "docs/watchlist.md",
        b"# Watchlist\n\nThe watchlist docs mention externalUsed.\n",
    );
    write(
        tmp.path(),
        "config/watchlist.yaml",
        b"watchlist:\n  callback: externalUsed\n",
    );

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("FinalV2Repo", root.clone(), "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert_eq!(summary.total_files, 11);
    link_repo(&ctx).expect("link fragments");

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

fn context_pack_cli_json_with_metrics(
    repo: &Path,
    command: &str,
    task: &str,
) -> (serde_json::Value, ContextPackBudgetMetrics) {
    let output = run_engine([command, "--repo", repo.to_str().unwrap(), "--task", task]);
    assert_success(&output);
    let output_text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("context-pack JSON parses");
    let metrics = context_pack_budget_metrics(&value, output_text.as_ref());
    (value, metrics)
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

fn activation_cli_json(repo: &Path, command: &str, key: &str, value: &str) -> serde_json::Value {
    query_json([command, "--repo", repo.to_str().unwrap(), key, value])
}

fn explore_cli_json(
    repo: &Path,
    request: &str,
    intent: &str,
    show_observability: bool,
) -> serde_json::Value {
    let mut args = vec![
        "explore".to_string(),
        "--repo".to_string(),
        repo.to_str().unwrap().to_string(),
        "--request".to_string(),
        request.to_string(),
        "--format".to_string(),
        "answer-json".to_string(),
        "--intent".to_string(),
        intent.to_string(),
    ];
    if show_observability {
        args.push("--show-observability".to_string());
    }
    query_json(args)
}

fn aethyme_explore_cli_json_with_metrics(
    repo: &Path,
    request: &str,
    intent: &str,
    show_observability: bool,
) -> (serde_json::Value, ExploreInvocationMetrics) {
    let mut args = vec![
        "explore".to_string(),
        "--repo".to_string(),
        repo.to_str().unwrap().to_string(),
        "--request".to_string(),
        request.to_string(),
        "--format".to_string(),
        "answer-json".to_string(),
        "--intent".to_string(),
        intent.to_string(),
    ];
    if show_observability {
        args.push("--show-observability".to_string());
    }
    let output = run_aethyme(args);
    assert_success(&output);
    let output_text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("aethyme explore JSON parses");
    let visible_payload = [
        "answer",
        "navigation_hints",
        "verification_steps",
        "next_actions",
    ]
    .into_iter()
    .filter_map(|key| value.get(key))
    .map(serde_json::Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let metrics = ExploreInvocationMetrics {
        command_output_chars: output_text.len(),
        token_estimate: estimate_tokens_from_chars(output_text.len()),
        aethyme_explore_invoked: value["schema_version"] == "aethyme-explore-v1"
            && value["intent"] == intent,
        aethyme_path_leaked: visible_payload.contains(".aethyme"),
    };
    (value, metrics)
}

fn aethyme_verify_targets_cli_json(
    repo: &Path,
    explore_json: &Path,
    max_targets: usize,
    max_lines: usize,
) -> serde_json::Value {
    let output = run_aethyme([
        "verify-targets",
        "--repo",
        repo.to_str().unwrap(),
        "--from",
        explore_json.to_str().unwrap(),
        "--max-targets",
        &max_targets.to_string(),
        "--max-lines",
        &max_lines.to_string(),
    ]);
    assert_success(&output);
    serde_json::from_slice(&output.stdout).expect("verify-targets JSON parses")
}

fn usage_boundary_explore_cli_json(repo: &Path, request: &str, scope: &str) -> serde_json::Value {
    query_json([
        "explore",
        "--repo",
        repo.to_str().unwrap(),
        "--request",
        request,
        "--format",
        "answer-json",
        "--intent",
        "usage_boundary_query",
        "--scope",
        scope,
        "--budget-ms",
        "5000",
        "--max-evidence-per-symbol",
        "4",
    ])
}

fn activate_from_cli_json(repo: &Path, seed: &str, hops: usize) -> serde_json::Value {
    let hops = hops.to_string();
    query_json([
        "activate-from",
        "--repo",
        repo.to_str().unwrap(),
        "--seed",
        seed,
        "--hops",
        &hops,
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

fn repository_map_activation_json(
    map: &RepositoryMap,
    task: &str,
    anchor_limit: usize,
) -> serde_json::Value {
    let task = aethyme_engine::model::task::TaskInput::from_task_text(task);
    let anchors = resolve_anchors(map, &task, anchor_limit);
    let profile = hormone_profile(&task.kind);
    let activation = spread_activation(map, &anchors, &profile);
    let json = aethyme_engine::json::activation_map(&activation);
    serde_json::from_str(&json).expect("RepositoryMap activation JSON parses")
}

fn repository_map_activate_from_json(
    map: &RepositoryMap,
    seed: &str,
    hops: usize,
) -> serde_json::Value {
    let activation = spread_from_seed(map, seed, hops);
    let json = aethyme_engine::json::activation_map(&activation);
    serde_json::from_str(&json).expect("RepositoryMap activate-from JSON parses")
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

fn legacy_symbol_hit_name_kinds(hits: &serde_json::Value) -> Vec<(String, String)> {
    hit_name_kinds(hits)
        .into_iter()
        .filter(|(_, kind)| matches!(kind.as_str(), "function" | "class"))
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
        let actual = legacy_symbol_hit_name_kinds(&symbol_cli_hits(repo, query, limit));
        assert_eq!(
            actual, expected,
            "redb V2 function/class symbol search should match RepositoryMap fuzzy scorer for query {query:?}"
        );
    }
}

#[test]
fn redb_symbol_search_matches_repository_map_fuzzy_scorer_on_tiny_fixture() {
    let tmp = build_redb_fixture();

    assert_redb_symbol_parity_with_repository_map(tmp.path(), &["load token", "auth", "token"], 5);

    let surface_hits = hit_name_kinds(&symbol_cli_hits(tmp.path(), "load token", 5));
    assert!(
        surface_hits
            .iter()
            .any(|(_, kind)| kind == "behavior_test_surface"),
        "unfiltered V2 search should include Surface/Flow hits; got {surface_hits:?}"
    );
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

#[test]
fn same_file_method_call_resolves_to_module_function_with_redb_parity() {
    let tmp = build_task_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap parity oracle");
    let load = map
        .functions
        .iter()
        .find(|function| {
            function.name.as_str() == "load" && function.file_path.as_str() == "src/auth/token.py"
        })
        .expect("TokenLoader.load function");
    let target = load.id.as_str();
    let expected = callees_view(&map, target);

    assert_eq!(expected.items.len(), 1, "{expected:#?}");
    assert_eq!(expected.items[0].kind, "function");
    assert!(
        expected.items[0]
            .id
            .ends_with(":src/auth/token.py:load_token")
    );
    assert_eq!(
        graph_cli_json(tmp.path(), "graph-callees", target),
        serde_json::from_str::<serde_json::Value>(&aethyme_engine::json::graph_relation(&expected))
            .expect("RepositoryMap graph JSON"),
        "redb must expose the same resolved call edge as RepositoryMap"
    );
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

#[test]
fn task_next_deduplicates_same_file_anchor_displays_with_map_redb_parity() {
    let tmp = build_duplicate_anchor_redb_fixture();
    let task = aethyme_engine::model::task::TaskInput::from_task_text("Update validate_token flow");
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap task-next oracle");
    let anchors = task_anchors_view(&map, &task).anchors;
    assert!(anchors.len() >= 2, "expected two ranked symbol anchors");
    assert_eq!(anchors[0].file.as_deref(), Some("src/auth.py"));
    assert_eq!(anchors[1].file.as_deref(), Some("src/auth.py"));
    assert!(
        anchors
            .iter()
            .skip(2)
            .any(|anchor| anchor.file.as_deref() == Some("src/main.py")),
        "expected a lower-ranked distinct anchor file"
    );

    let expected = task_next_view(&map, &task);
    let actual = task_cli_json(tmp.path(), "task-next", &task.raw);
    assert_eq!(
        actual,
        repository_map_task_json(&map, "task-next", &task.raw),
        "redb task-next should preserve RepositoryMap ranking and deduplication"
    );

    let displays: Vec<&str> = expected
        .items
        .iter()
        .map(|item| item.display.as_str())
        .collect();
    assert_eq!(
        displays
            .iter()
            .filter(|display| **display == "src/auth.py")
            .count(),
        1,
        "the first-seen anchor file should appear exactly once"
    );
    assert_eq!(
        displays.get(..2),
        Some(["src/auth.py", "src/main.py"].as_slice()),
        "deduplication should retain the first two distinct displays in rank order"
    );
}

#[test]
fn redb_explore_task_localization_runs_without_daemon_or_fragments_and_preserves_shape() {
    let tmp = build_task_redb_fixture();
    std::fs::remove_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let first = explore_cli_json(
        tmp.path(),
        "Find the load_token flow",
        "task_localization_query",
        false,
    );
    let second = explore_cli_json(
        tmp.path(),
        "Find the load_token flow",
        "task_localization_query",
        false,
    );
    assert_eq!(first, second, "compact explore output should be stable");

    let expected_keys: BTreeSet<&str> = [
        "schema_version",
        "mode",
        "intent",
        "intent_source",
        "status",
        "request",
        "answer",
        "navigation_hints",
        "excluded",
        "ambiguous",
        "subsystems",
        "evidence",
        "confidence",
        "safe_to_use_as_answer",
        "safe_to_use_as_navigation",
        "trust_policy",
        "degraded_reasons",
        "verification_steps",
        "next_actions",
        "available_specialized_intents",
        "output_chars_estimate",
        "truncated",
    ]
    .into_iter()
    .collect();
    assert_eq!(object_keys(&first), expected_keys);
    assert_eq!(first["schema_version"], "aethyme-explore-v1");
    assert_eq!(first["intent"], "task_localization_query");
    assert_eq!(first["intent_source"], "explicit");
    assert!(
        first["answer"]
            .as_array()
            .expect("answer array")
            .iter()
            .any(|item| item["path"] == "src/auth/token.py"
                || item["target"]
                    .as_str()
                    .is_some_and(|target| target.contains("load_token")))
    );
    assert!(
        first.get("observability").is_none(),
        "compact explore should omit observability"
    );
    assert!(
        first["output_chars_estimate"].as_u64().unwrap_or(0) > 0,
        "compact explore should report output size"
    );
    assert!(
        first["truncated"].is_boolean(),
        "compact explore should report whether output was truncated"
    );
}

#[test]
fn redb_explore_behavior_and_auto_intents_use_redb_path() {
    let tmp = build_task_redb_fixture();

    let explicit = explore_cli_json(
        tmp.path(),
        "Implement load_token tracing",
        "behavior_localization_query",
        false,
    );
    assert_eq!(explicit["intent"], "behavior_localization_query");
    assert_eq!(explicit["intent_source"], "explicit");
    assert!(
        !explicit["answer"]
            .as_array()
            .expect("answer array")
            .is_empty(),
        "behavior localization should return bounded candidates"
    );

    let auto = explore_cli_json(tmp.path(), "Implement load_token tracing", "auto", false);
    assert_eq!(auto["intent"], "behavior_localization_query");
    assert_eq!(auto["intent_source"], "auto");
}

#[test]
fn redb_explore_observability_reports_store_freshness() {
    let tmp = build_task_redb_fixture();

    let response = explore_cli_json(
        tmp.path(),
        "Find the load_token flow",
        "task_localization_query",
        true,
    );
    let graph_store = &response["observability"]["graph_store"];
    assert_eq!(graph_store["backend"], "redb");
    assert_eq!(graph_store["exists"], true);
    assert_eq!(graph_store["fragments_exist"], true);
    assert_eq!(graph_store["status"], "fresh");
    assert_eq!(graph_store["stale"], false);
    assert!(
        graph_store.get("path").is_none(),
        "observability should report freshness without leaking generated artifact paths"
    );
    assert!(
        graph_store.get("fragments_path").is_none(),
        "observability should report fragment freshness without leaking generated artifact paths"
    );
}

#[test]
fn redb_explore_auth_surface_fixture_ranks_proxy_backend_and_names_decoys() {
    let tmp = build_surface_flow_auth_redb_fixture();

    let (response, metrics) = aethyme_explore_cli_json_with_metrics(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
        "task_localization_query",
        true,
    );

    assert_eq!(response["schema_version"], "aethyme-explore-v1");
    assert_eq!(response["intent"], "task_localization_query");
    assert_eq!(response["observability"]["graph_store"]["backend"], "redb");
    assert_eq!(
        response["observability"]["output_profile"], "agent_compact",
        "--show-observability should default to compact agent observability"
    );
    assert!(
        response.get("output_adapters").is_none(),
        "compact agent observability should not emit verbose adapters"
    );
    assert!(
        response.get("resolved_parameters").is_none(),
        "compact agent observability should not emit resolved params"
    );
    assert!(
        metrics.aethyme_explore_invoked,
        "fixture should exercise router-level aethyme explore: {metrics:?}"
    );
    assert!(
        !metrics.aethyme_path_leaked,
        "fixture output leaked generated Aethyme paths: {metrics:?}"
    );

    let subsystems = response["subsystems"].as_array().expect("subsystems");
    let roles = subsystems
        .iter()
        .map(|subsystem| subsystem["role"].as_str().expect("role"))
        .collect::<Vec<_>>();
    assert_eq!(
        roles.iter().take(2).copied().collect::<Vec<_>>(),
        vec!["ingress_proxy", "backend_validator"],
        "proxy and backend validator should be the first verification lanes: {roles:?}"
    );

    let ingress_proxy = subsystems
        .iter()
        .find(|subsystem| subsystem["role"] == "ingress_proxy")
        .expect("ingress/proxy subsystem");
    let ingress_target_values = ingress_proxy["top_verification_targets"]
        .as_array()
        .expect("ingress targets");
    let ingress_top = ingress_target_values
        .first()
        .map(serde_json::Value::to_string)
        .unwrap_or_default();
    let ingress_targets = ingress_target_values
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        ingress_top.contains("gcp-run-proxy"),
        "ingress first verification target should point at the worker/proxy: top={ingress_top}, all={ingress_targets}"
    );

    let backend_validator = subsystems
        .iter()
        .find(|subsystem| subsystem["role"] == "backend_validator")
        .expect("backend validator subsystem");
    let backend_target_values = backend_validator["top_verification_targets"]
        .as_array()
        .expect("backend targets");
    let backend_top = backend_target_values
        .first()
        .map(serde_json::Value::to_string)
        .unwrap_or_default();
    let backend_targets = backend_target_values
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        backend_top.contains("backend/api_keys"),
        "backend first verification target should point at API-key validation code: top={backend_top}, all={backend_targets}"
    );

    let ambiguity = response["ambiguous"].to_string();
    assert!(
        ambiguity.contains("OIDC"),
        "token ambiguity should explicitly name the OIDC decoy subsystem: {ambiguity}"
    );
    assert!(
        ambiguity.contains("audit JWS"),
        "token ambiguity should explicitly name the audit JWS decoy subsystem: {ambiguity}"
    );
    assert!(
        ambiguity.contains("Auth0 management"),
        "token ambiguity should explicitly name the Auth0 management decoy subsystem: {ambiguity}"
    );
    assert!(
        ambiguity.contains("webhook tokens"),
        "token ambiguity should explicitly name the webhook token decoy subsystem: {ambiguity}"
    );

    let verification_steps = response["verification_steps"]
        .as_array()
        .expect("verification steps")
        .iter()
        .filter_map(|step| step["step"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        verification_steps.contains("There are multiple token systems"),
        "verification should surface token subsystem ambiguity: {verification_steps}"
    );
    assert!(
        verification_steps.contains("Verify proxy classification first, then backend validation"),
        "verification should force proxy/backend verification order: {verification_steps}"
    );
}

#[test]
fn verify_targets_extracts_bounded_spans_from_explore_json() {
    let tmp = build_surface_flow_auth_redb_fixture();

    let (explore, _) = aethyme_explore_cli_json_with_metrics(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
        "task_localization_query",
        true,
    );
    let explore_path = tmp.path().join("explore.json");
    std::fs::write(&explore_path, serde_json::to_vec_pretty(&explore).unwrap()).unwrap();

    let verified = aethyme_verify_targets_cli_json(tmp.path(), &explore_path, 2, 80);
    assert_eq!(verified["schema_version"], "aethyme-verify-targets-v1");
    assert_eq!(verified["limits"]["max_targets"], 2);
    assert_eq!(verified["limits"]["max_lines"], 80);
    assert!(verified["total_line_count"].as_u64().unwrap() <= 80);
    assert!(
        !verified.to_string().contains(".aethyme"),
        "verification output must not leak generated graph artifacts: {verified}"
    );

    let targets = verified["targets"].as_array().expect("targets array");
    assert_eq!(targets.len(), 2, "expected exactly two bounded targets");
    assert_eq!(targets[0]["status"], "verified_span");
    assert_eq!(targets[1]["status"], "verified_span");
    assert!(
        targets[0]["path"]
            .as_str()
            .is_some_and(|path| path.contains("gcp-run-proxy")),
        "first target should verify the ingress/proxy lane: {targets:?}"
    );
    assert!(
        targets[1]["path"]
            .as_str()
            .is_some_and(|path| path.contains("backend/api_keys")),
        "second target should verify the backend validator lane: {targets:?}"
    );

    let first_lines = targets[0]["lines"].to_string();
    assert!(first_lines.contains("Authorization"));
    assert!(first_lines.contains("Bearer"));
    let second_lines = targets[1]["lines"].to_string();
    assert!(second_lines.contains("validate_publishable_key"));
    assert!(second_lines.contains("pk_"));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextPackBudgetMetrics {
    command_output_chars: usize,
    token_estimate: usize,
    selected_file_count: usize,
    selected_symbol_count: usize,
    snippet_count: usize,
    aethyme_path_leaked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExploreInvocationMetrics {
    command_output_chars: usize,
    token_estimate: usize,
    aethyme_explore_invoked: bool,
    aethyme_path_leaked: bool,
}

fn estimate_tokens_from_chars(chars: usize) -> usize {
    (chars + 3) / 4
}

fn context_pack_budget_metrics(
    value: &serde_json::Value,
    output_text: &str,
) -> ContextPackBudgetMetrics {
    let selected_file_count = value["in_scope"]["files"]
        .as_array()
        .expect("in_scope files")
        .len();
    let selected_symbol_count = value["in_scope"]["symbols"]
        .as_array()
        .expect("in_scope symbols")
        .len();
    let snippet_count = value["snippets"].as_array().expect("snippets").len();
    ContextPackBudgetMetrics {
        command_output_chars: output_text.len(),
        token_estimate: estimate_tokens_from_chars(output_text.len()),
        selected_file_count,
        selected_symbol_count,
        snippet_count,
        aethyme_path_leaked: output_text.contains(".aethyme"),
    }
}

fn repository_map_context_pack_budget_metrics(
    value: &serde_json::Value,
) -> ContextPackBudgetMetrics {
    let output_text = value.to_string();
    context_pack_budget_metrics(value, &output_text)
}

#[test]
fn context_pack_budget_metrics_track_stable_cost_signals() {
    let value = serde_json::json!({
        "in_scope": {
            "files": [{"value": "src/auth/token.py"}, {"value": "src/web/handler.py"}],
            "symbols": [{"value": "fn:load_token"}]
        },
        "snippets": [{"path": "src/auth/token.py"}, {"path": "src/web/handler.py"}]
    });
    let output_text = value.to_string();

    let metrics = context_pack_budget_metrics(&value, &output_text);

    assert_eq!(metrics.command_output_chars, output_text.len());
    assert_eq!(
        metrics.token_estimate,
        estimate_tokens_from_chars(output_text.len())
    );
    assert_eq!(metrics.selected_file_count, 2);
    assert_eq!(metrics.selected_symbol_count, 1);
    assert_eq!(metrics.snippet_count, 2);
    assert!(!metrics.aethyme_path_leaked);

    let leaked = context_pack_budget_metrics(
        &value,
        &format!("{output_text}\ninternal=.aethyme/graph_store.redb"),
    );
    assert!(leaked.aethyme_path_leaked);
}

fn assert_metric_not_above(
    label: &str,
    actual: usize,
    expected: usize,
    percent_limit: usize,
    slack: usize,
    command: &str,
    task: &str,
    actual_metrics: &ContextPackBudgetMetrics,
    expected_metrics: &ContextPackBudgetMetrics,
) {
    let limit = expected.saturating_mul(percent_limit) / 100 + slack;
    assert!(
        actual <= limit,
        "{label} regressed for redb {command} task {task:?}: actual={actual}, expected={expected}, limit={limit}, actual_metrics={actual_metrics:?}, expected_metrics={expected_metrics:?}"
    );
}

fn aethyme_repo_root_for_tests() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|path| path.join("AGENTS.md").is_file() && path.join("packages/aethyme").is_dir())
        .expect("locate Aethyme repo root")
        .to_path_buf()
}

fn assert_external_playground_repo(repo: &Path) -> PathBuf {
    assert!(
        repo.is_dir(),
        "playground repo does not exist: {}",
        repo.display()
    );
    let repo = repo
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize playground repo {}: {e}", repo.display()));
    let aethyme_root = aethyme_repo_root_for_tests()
        .canonicalize()
        .expect("canonicalize Aethyme root");
    assert!(
        !repo.starts_with(&aethyme_root),
        "Cardinal Rule 1: playground gates must never point at Aethyme itself ({})",
        aethyme_root.display()
    );
    repo
}

fn assert_pack_within_token_budget(repo: &Path, map: &RepositoryMap, task: &str, command: &str) {
    let expected = repository_map_context_pack_json(repo, map, task);
    let expected_metrics = repository_map_context_pack_budget_metrics(&expected);
    let (_actual, actual_metrics) = context_pack_cli_json_with_metrics(repo, command, task);

    if expected_metrics.selected_file_count > 0 {
        assert!(
            actual_metrics.selected_file_count > 0,
            "redb {command} selected no files for task {task:?}: actual_metrics={actual_metrics:?}, expected_metrics={expected_metrics:?}"
        );
    }
    assert!(
        !actual_metrics.aethyme_path_leaked,
        "redb {command} leaked .aethyme paths into context-pack output for task {task:?}: actual_metrics={actual_metrics:?}"
    );
    assert_metric_not_above(
        "token estimate",
        actual_metrics.token_estimate,
        expected_metrics.token_estimate,
        120,
        128,
        command,
        task,
        &actual_metrics,
        &expected_metrics,
    );
    assert_metric_not_above(
        "selected file count",
        actual_metrics.selected_file_count,
        expected_metrics.selected_file_count,
        150,
        2,
        command,
        task,
        &actual_metrics,
        &expected_metrics,
    );
    assert_metric_not_above(
        "selected symbol count",
        actual_metrics.selected_symbol_count,
        expected_metrics.selected_symbol_count,
        150,
        4,
        command,
        task,
        &actual_metrics,
        &expected_metrics,
    );
    assert_metric_not_above(
        "snippet count",
        actual_metrics.snippet_count,
        expected_metrics.snippet_count,
        150,
        2,
        command,
        task,
        &actual_metrics,
        &expected_metrics,
    );
    assert_metric_not_above(
        "command-output chars",
        actual_metrics.command_output_chars,
        expected_metrics.command_output_chars,
        120,
        512,
        command,
        task,
        &actual_metrics,
        &expected_metrics,
    );
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
fn redb_explain_aliases_render_from_redb_context_pack() {
    let tmp = build_task_redb_fixture();

    let explain = run_engine(["explain", "--repo", tmp.path().to_str().unwrap()]);
    assert_success(&explain);
    let explain_text = String::from_utf8_lossy(&explain.stdout);
    assert!(explain_text.contains("Task: Explain this repo"));
    assert!(explain_text.contains("Files indexed:"));
    assert!(explain_text.contains("Docs indexed:"));

    let task_explain = run_engine([
        "task-explain",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--task",
        "Trace impact of load_token",
    ]);
    assert_success(&task_explain);
    let task_explain_text = String::from_utf8_lossy(&task_explain.stdout);
    assert!(task_explain_text.contains("Task: Trace impact of load_token"));
    assert!(task_explain_text.contains("Functions indexed:"));
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
        assert_pack_within_token_budget(tmp.path(), &map, task, "pack");
    }
}

#[test]
#[ignore = "requires AETHYME_PLAYGROUND_REPO; never point this at the Aethyme repo"]
fn playground_context_pack_token_regression_gate_never_self_eval() {
    let Ok(repo) = env::var("AETHYME_PLAYGROUND_REPO") else {
        eprintln!("skipping: set AETHYME_PLAYGROUND_REPO to a playground repo");
        return;
    };
    let repo = assert_external_playground_repo(Path::new(&repo));

    let index = run_engine(["index", "--repo", repo.to_str().unwrap()]);
    assert_success(&index);

    let map = RepositoryMap::build(&repo).expect("build RepositoryMap playground token oracle");
    for task in [
        "Explain this repo",
        "Update the authentication flow",
        "Trace impact of the main request handler",
        "Find the manifest that owns the top-level area",
    ] {
        assert_pack_within_token_budget(&repo, &map, task, "pack");
        assert_pack_within_token_budget(&repo, &map, task, "task-pack");
    }

    let (explore, explore_metrics) = aethyme_explore_cli_json_with_metrics(
        &repo,
        "Find the main request handling flow",
        "task_localization_query",
        true,
    );
    assert_eq!(explore["schema_version"], "aethyme-explore-v1");
    assert_eq!(explore["observability"]["graph_store"]["backend"], "redb");
    assert!(
        explore_metrics.aethyme_explore_invoked,
        "playground token gate must invoke router-level aethyme explore: {explore_metrics:?}"
    );
    assert!(
        explore_metrics.command_output_chars > 0 && explore_metrics.token_estimate > 0,
        "explore smoke should record non-empty output metrics: {explore_metrics:?}"
    );
    assert!(
        !explore_metrics.aethyme_path_leaked,
        "explore answer/navigation payload leaked .aethyme internals: {explore_metrics:?}"
    );
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
fn redb_activation_matches_repository_map_snapshot_on_medium_fixture() {
    let tmp = build_task_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap activation oracle");

    for task in [
        "Explain this repo",
        "Update load_token flow",
        "Trace impact of load_token",
        "Find the manifest that owns the top-level area",
    ] {
        let parsed = aethyme_engine::model::task::TaskInput::from_task_text(task);
        let anchor_limit = if parsed.kind.is_explain_repo() { 5 } else { 3 };
        let expected = repository_map_activation_json(&map, task, anchor_limit);
        let actual = activation_cli_json(tmp.path(), "activate", "--task", task);
        assert_eq!(
            actual, expected,
            "activate should preserve RepositoryMap JSON for task {task:?}"
        );
    }
}

#[test]
fn redb_activate_from_matches_repository_map_snapshot() {
    let tmp = build_activation_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap seed oracle");
    let expected = repository_map_activate_from_json(&map, "src/calls.py::caller", 3);
    let actual = activate_from_cli_json(tmp.path(), "src/calls.py::caller", 3);
    assert_eq!(
        actual, expected,
        "activate-from should preserve RepositoryMap JSON"
    );
}

#[test]
fn redb_activation_expands_tiny_relation_fixture() {
    let tmp = build_activation_redb_fixture();

    let call_activation = activate_from_cli_json(tmp.path(), "src/calls.py::caller", 3);
    let call_ids = call_activation["activations"]
        .as_array()
        .expect("activation array")
        .iter()
        .map(|item| item["id"].as_str().expect("activation id").to_string())
        .collect::<Vec<_>>();
    assert!(call_ids.iter().any(|id| id.contains("caller")));
    assert!(call_ids.iter().any(|id| id.contains("callee")));

    let repo_activation =
        activation_cli_json(tmp.path(), "activate", "--task", "Explain this repo");
    let repo_ids = repo_activation["activations"]
        .as_array()
        .expect("activation array")
        .iter()
        .map(|item| item["id"].as_str().expect("activation id").to_string())
        .collect::<Vec<_>>();
    assert!(
        repo_ids.iter().any(|id| id.starts_with("doc:")),
        "docs should be reachable through redb relation expansion"
    );
    assert!(
        repo_ids.iter().any(|id| id.starts_with("config:")),
        "configs should be reachable through redb relation expansion"
    );

    let impact = query_json([
        "impact",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--target",
        "load_token",
    ]);
    let impact_items = impact.as_array().expect("impact array");
    assert!(
        impact_items.iter().any(|item| item
            .as_str()
            .is_some_and(|value| value.starts_with("file:"))),
        "import-adjacent impact should include at least one file frontier item"
    );
}

#[test]
fn redb_activation_order_is_deterministic_and_bounded() {
    let tmp = build_activation_redb_fixture();
    let first = activate_from_cli_json(tmp.path(), "src/calls.py::caller", 4);
    let second = activate_from_cli_json(tmp.path(), "src/calls.py::caller", 4);
    assert_eq!(first, second, "activation order should be deterministic");

    let activations = first["activations"].as_array().expect("activation array");
    assert!(
        activations.len() <= 20,
        "tiny activation fixture should stay bounded, got {} activations",
        activations.len()
    );
}

#[test]
fn redb_impact_matches_repository_map_snapshot() {
    let tmp = build_activation_redb_fixture();
    let map = RepositoryMap::build(tmp.path()).expect("build RepositoryMap impact oracle");
    let expected = serde_json::to_value(impact_frontier(&map, "load_token")).expect("impact JSON");
    let actual = query_json([
        "impact",
        "--repo",
        tmp.path().to_str().unwrap(),
        "--target",
        "load_token",
    ]);
    assert_eq!(actual, expected);
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
    assert!(
        internal["ambiguity"]
            .as_array()
            .expect("ambiguity")
            .contains(&serde_json::json!("exported_but_internal_only"))
    );

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
fn redb_explore_usage_boundary_intent_uses_hybrid_v2_contract() {
    let tmp = build_usage_boundary_redb_fixture();
    std::fs::remove_dir_all(tmp.path().join(".aethyme/graph")).unwrap();

    let response = usage_boundary_explore_cli_json(
        tmp.path(),
        "Find unused public watchlist methods",
        "includes/Watchlist",
    );
    assert_eq!(response["schema_version"], "aethyme-explore-v1");
    assert_eq!(response["intent"], "usage_boundary_query");
    assert_eq!(response["intent_source"], "explicit");
    assert!(
        response["answer"]
            .as_array()
            .expect("answer array")
            .iter()
            .any(|item| item["target"]
                .as_str()
                .is_some_and(|target| target.contains("unusedMethod")))
    );
    assert!(
        response["degraded_reasons"]
            .as_array()
            .expect("degraded reasons")
            .iter()
            .any(|reason| reason == "redb_seed_discovery"),
        "usage-boundary explore should declare the redb seed path"
    );
}

#[test]
fn usage_boundary_reads_fresh_source_evidence_after_redb_index() {
    let tmp = build_usage_boundary_redb_fixture();
    let store_path = tmp.path().join(".aethyme/graph_store.redb");
    let store_modified_before = std::fs::metadata(&store_path)
        .expect("store metadata before query")
        .modified()
        .expect("store mtime before query");

    write(
        tmp.path(),
        "includes/Api/Controller.php",
        b"<?php\nclass Controller {\n    public function handle($store) { $store->externalUsed(); }\n    public function newHandle($store) { $store->unusedMethod(); }\n}\n",
    );
    write(
        tmp.path(),
        "docs/watchlist.md",
        b"# Watchlist\n\nThe docsOnly hook is configured by operations.\nThe unusedMethod callback was wired after indexing.\n",
    );

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
    let excluded = answer["excluded"].as_array().expect("excluded");

    let now_used = dead_code_item_by_name(excluded, "unusedMethod");
    assert_eq!(
        now_used["status"], "Used",
        "fresh external source text should override the stale pre-index unused classification"
    );
    assert!(
        now_used["evidence"]["external_callers"]
            .as_array()
            .expect("external callers")
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|value| value.contains("includes/Api/Controller.php")
                    && value.contains("unusedMethod"))),
        "external caller evidence should be read from the mutated source file"
    );

    let docs_ref = answer["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .chain(excluded.iter())
        .find(|item| item["function"]["name"] == "unusedMethod")
        .expect("unusedMethod item");
    assert!(
        docs_ref["evidence"]["docs_config_references"]
            .as_array()
            .expect("docs refs")
            .iter()
            .any(|item| item.as_str().is_some_and(
                |value| value.contains("docs/watchlist.md") && value.contains("unusedMethod")
            )),
        "docs/config evidence should also be read from fresh text"
    );

    let store_modified_after = std::fs::metadata(&store_path)
        .expect("store metadata after query")
        .modified()
        .expect("store mtime after query");
    assert_eq!(
        store_modified_after, store_modified_before,
        "usage-boundary queries must not mutate the derived redb store"
    );
}

#[test]
fn final_v2_medium_fixture_integrates_graph_task_context_activation_usage_and_explore() {
    let tmp = build_final_v2_medium_redb_fixture();
    let repo = tmp.path();

    let overview = query_json(["query-overview", "--repo", repo.to_str().unwrap()]);
    assert!(overview["areas"].as_array().expect("areas").len() >= 3);

    let symbol_hits = query_json([
        "symbol",
        "--repo",
        repo.to_str().unwrap(),
        "--query",
        "load_token",
    ]);
    assert!(
        !symbol_hits.as_array().expect("symbol hits").is_empty(),
        "symbol query should return redb matches"
    );

    let graph_node = graph_cli_json(repo, "graph-node", "load_token");
    assert_eq!(graph_node["kind"], "function");
    for command in [
        "graph-children",
        "graph-parents",
        "graph-callers",
        "graph-callees",
        "graph-docs",
        "graph-configs",
    ] {
        let relation = graph_cli_json(repo, command, "load_token");
        assert!(
            relation["items"].as_array().is_some(),
            "{command} should render a relation array"
        );
    }
    let callees = graph_cli_json(repo, "graph-callees", "caller");
    assert!(
        !callees["items"].as_array().expect("callees").is_empty(),
        "fixture call edge should be visible through graph-callees"
    );
    let graph_overview = graph_overview_cli_json(repo);
    assert!(graph_overview["signals"].as_object().is_some());
    let expand = graph_cli_json(repo, "graph-expand", "caller");
    assert!(
        !expand["callees"]
            .as_array()
            .expect("expand callees")
            .is_empty(),
        "graph-expand should compose call relations"
    );

    let task = "Trace impact of load_token";
    for command in ["task-anchors", "task-scope", "task-next", "task-localize"] {
        let task_view = task_cli_json(repo, command, task);
        assert!(
            task_view.as_object().is_some(),
            "{command} should render JSON"
        );
    }
    let task_expand = task_expand_cli_json(repo, "caller");
    assert!(task_expand["impact"].as_array().is_some());

    let pack = context_pack_cli_json(repo, "pack", task);
    assert!(pack["snippets"].as_array().is_some());
    assert_eq!(context_pack_cli_json(repo, "task-pack", task), pack);
    let context = context_with_content_cli_json(repo, "context", task);
    assert!(context["snippets"].as_array().is_some());
    assert_eq!(
        context_with_content_cli_json(repo, "task-context", task),
        context
    );

    let explain = run_engine([
        "task-explain",
        "--repo",
        repo.to_str().unwrap(),
        "--task",
        task,
    ]);
    assert_success(&explain);
    assert!(String::from_utf8_lossy(&explain.stdout).contains("Task: Trace impact of load_token"));

    let activation = activation_cli_json(repo, "activate", "--task", task);
    assert!(
        !activation["activations"]
            .as_array()
            .expect("activation array")
            .is_empty(),
        "activate should return redb-backed activations"
    );
    let seed_activation = activate_from_cli_json(repo, "src/calls.py::caller", 3);
    assert!(
        seed_activation["activations"]
            .as_array()
            .expect("seed activation array")
            .iter()
            .any(|item| item["id"].as_str().is_some_and(|id| id.contains("callee"))),
        "activate-from should traverse the fixture call edge"
    );
    let impact = query_json([
        "impact",
        "--repo",
        repo.to_str().unwrap(),
        "--target",
        "load_token",
    ]);
    assert!(impact.as_array().is_some());

    let usage = query_json([
        "analyze-usage-boundary",
        "--repo",
        repo.to_str().unwrap(),
        "--scope",
        "includes/Watchlist",
        "--include-methods",
        "--budget-ms",
        "5000",
    ]);
    assert_eq!(usage["analyzer"], "usage-boundary");
    assert!(usage["candidates"].as_array().is_some());

    let explore = explore_cli_json(
        repo,
        "Find the load_token flow",
        "task_localization_query",
        true,
    );
    assert_eq!(explore["schema_version"], "aethyme-explore-v1");
    assert_eq!(explore["observability"]["graph_store"]["backend"], "redb");
    let usage_explore = usage_boundary_explore_cli_json(
        repo,
        "Find unused watchlist methods",
        "includes/Watchlist",
    );
    assert_eq!(usage_explore["intent"], "usage_boundary_query");
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
    let broad_hit_items = broad_hits["viewing page"]
        .as_array()
        .expect("broad hits array");
    let mediawiki_target = broad_hit_items
        .iter()
        .find(|hit| hit["name"].as_str() == Some("doViewUpdates"))
        .or_else(|| broad_hit_items.first())
        .and_then(|hit| hit["id"].as_str().or_else(|| hit["name"].as_str()))
        .expect("MediaWiki relation target")
        .to_string();

    let (graph_overview_output, graph_overview_ms) =
        run_engine_timed(["graph-overview", "--repo", repo.to_str().unwrap()]);
    assert_success(&graph_overview_output);
    assert_duration_below(
        "MediaWiki graph-overview",
        graph_overview_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_GRAPH_OVERVIEW_MS",
        5_000,
    );

    let (relation_output, relation_ms) = run_engine_timed([
        "graph-callees",
        "--repo",
        repo.to_str().unwrap(),
        "--target",
        mediawiki_target.as_str(),
    ]);
    assert_success(&relation_output);
    assert_duration_below(
        "MediaWiki graph relation query",
        relation_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_RELATION_MS",
        5_000,
    );
    let relation: serde_json::Value =
        serde_json::from_slice(&relation_output.stdout).expect("relation JSON parses");
    assert!(relation["items"].as_array().is_some());

    let (expand_output, expand_ms) = run_engine_timed([
        "graph-expand",
        "--repo",
        repo.to_str().unwrap(),
        "--target",
        mediawiki_target.as_str(),
    ]);
    assert_success(&expand_output);
    assert_duration_below(
        "MediaWiki graph-expand",
        expand_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_GRAPH_EXPAND_MS",
        10_000,
    );
    let expand: serde_json::Value =
        serde_json::from_slice(&expand_output.stdout).expect("graph-expand JSON parses");
    assert!(expand["target"].as_object().is_some());

    let task_text = "Trace impact of doViewUpdates";
    for (command, env_key) in [
        ("task-anchors", "AETHYME_REDB_MEDIAWIKI_MAX_TASK_ANCHORS_MS"),
        ("task-scope", "AETHYME_REDB_MEDIAWIKI_MAX_TASK_SCOPE_MS"),
        ("task-next", "AETHYME_REDB_MEDIAWIKI_MAX_TASK_NEXT_MS"),
    ] {
        let (task_output, task_ms) = run_engine_timed([
            command,
            "--repo",
            repo.to_str().unwrap(),
            "--task",
            task_text,
        ]);
        assert_success(&task_output);
        assert_duration_below(command, task_ms, env_key, 10_000);
        let parsed: serde_json::Value =
            serde_json::from_slice(&task_output.stdout).expect("task JSON parses");
        assert!(parsed.as_object().is_some());
    }

    let (task_output, task_ms) = run_engine_timed([
        "task-localize",
        "--repo",
        repo.to_str().unwrap(),
        "--task",
        task_text,
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

    let (pack_output, pack_ms) = run_engine_timed([
        "pack",
        "--repo",
        repo.to_str().unwrap(),
        "--task",
        task_text,
    ]);
    assert_success(&pack_output);
    assert_duration_below(
        "MediaWiki context pack",
        pack_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_CONTEXT_PACK_MS",
        20_000,
    );
    let pack: serde_json::Value =
        serde_json::from_slice(&pack_output.stdout).expect("context-pack JSON parses");
    assert!(pack["snippets"].as_array().is_some());

    let (explore_output, explore_ms) = run_engine_timed([
        "explore",
        "--repo",
        repo.to_str().unwrap(),
        "--request",
        "Trace impact of doViewUpdates",
        "--format",
        "answer-json",
        "--intent",
        "task_localization_query",
    ]);
    assert_success(&explore_output);
    assert_duration_below(
        "MediaWiki explore",
        explore_ms,
        "AETHYME_REDB_MEDIAWIKI_MAX_EXPLORE_MS",
        20_000,
    );
    let explore: serde_json::Value =
        serde_json::from_slice(&explore_output.stdout).expect("explore JSON parses");
    assert_eq!(explore["schema_version"], "aethyme-explore-v1");
    assert!(explore["answer"].as_array().is_some());
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
    assert!(
        parsed["repo"]["languages"]
            .as_array()
            .expect("languages array")
            .iter()
            .any(|lang| lang == "python")
    );

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
            "impact",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--target",
            "load_token",
        ],
        vec![
            "activate",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--task",
            "Update load_token flow",
        ],
        vec![
            "activate-from",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--seed",
            "src/auth/token.py",
        ],
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
            "explore",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--request",
            "Update load_token flow",
            "--format",
            "answer-json",
            "--intent",
            "task_localization_query",
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
