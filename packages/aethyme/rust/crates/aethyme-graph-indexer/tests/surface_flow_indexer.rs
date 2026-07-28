//! Surface/Flow extractor fixture coverage.

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk};
use aethyme_graph_schema::{EdgeKind, NodeKind};
use aethyme_graph_storage::read_fragment;

fn write(root: &std::path::Path, rel: &str, content: &str) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn ctx(repo_root: &std::path::Path) -> IndexerContext {
    IndexerContext::new("surfacefixture", repo_root.to_path_buf(), "0.1.0").unwrap()
}

fn node_kinds(root: &std::path::Path, rel: &str) -> Vec<NodeKind> {
    read_fragment(root, rel)
        .unwrap()
        .nodes()
        .iter()
        .map(|node| node.kind())
        .collect()
}

fn edge_kinds(root: &std::path::Path, rel: &str) -> Vec<EdgeKind> {
    read_fragment(root, rel)
        .unwrap()
        .edges()
        .iter()
        .map(|edge| edge.kind())
        .collect()
}

#[test]
fn persists_expected_routes_proxy_middleware_workers_and_auth_tests() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "backend/urls.py",
        r#"
from django.urls import path, re_path

urlpatterns = [
    path("api/token/", token_view),
    re_path(r"^api/profile/$", profile_view),
]
"#,
    );
    write(
        tmp.path(),
        "backend/settings.py",
        r#"
MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
    "core.auth.TokenAuditMiddleware",
]

REST_FRAMEWORK = {
    "DEFAULT_AUTHENTICATION_CLASSES": [
        "rest_framework.authentication.TokenAuthentication",
    ],
    "DEFAULT_PERMISSION_CLASSES": [
        "rest_framework.permissions.IsAuthenticated",
    ],
}
"#,
    );
    write(
        tmp.path(),
        "edge/worker.mjs",
        r#"
export default {
  async fetch(request, env) {
    const headers = new Headers(request.headers)
    headers.set("Authorization", `Bearer ${env.API_TOKEN}`)
    return fetch("https://api.example.com/token", { headers })
  }
}
"#,
    );
    write(
        tmp.path(),
        "netlify.toml",
        r#"
[[redirects]]
from = "/api/*"
to = "https://backend.example.com/:splat"
status = 200
force = true

[build.environment]
JWT_AUDIENCE = "internal-api"
"#,
    );
    write(
        tmp.path(),
        "backend/tests/test_auth_flow.py",
        r#"
def test_token_route_requires_bearer(client):
    response = client.get("/api/token/", HTTP_AUTHORIZATION="Bearer test-token")
    assert response.status_code == 200
"#,
    );

    let summary = index_repo_to_disk(&ctx(tmp.path()), &WalkOptions::default()).unwrap();
    assert_eq!(
        summary.counts_by_kind.get(&NodeKind::RouteSurface),
        Some(&2)
    );
    assert!(
        summary
            .counts_by_kind
            .get(&NodeKind::MiddlewareInstallation)
            .copied()
            .unwrap_or(0)
            >= 4
    );
    assert_eq!(
        summary.counts_by_kind.get(&NodeKind::WorkerSurface),
        Some(&1)
    );
    assert_eq!(
        summary.counts_by_kind.get(&NodeKind::BehaviorTestSurface),
        Some(&1)
    );
    assert!(
        summary
            .counts_by_kind
            .get(&NodeKind::ProxySurface)
            .copied()
            .unwrap_or(0)
            >= 2
    );
    assert!(
        summary
            .counts_by_kind
            .get(&NodeKind::CredentialOperation)
            .copied()
            .unwrap_or(0)
            >= 2
    );

    let url_nodes = node_kinds(tmp.path(), "backend/urls.py");
    assert_eq!(
        url_nodes
            .iter()
            .filter(|kind| **kind == NodeKind::RouteSurface)
            .count(),
        2
    );
    assert!(
        edge_kinds(tmp.path(), "backend/urls.py")
            .iter()
            .any(|kind| *kind == EdgeKind::Exposes)
    );

    let settings_nodes = node_kinds(tmp.path(), "backend/settings.py");
    assert_eq!(
        settings_nodes
            .iter()
            .filter(|kind| **kind == NodeKind::MiddlewareInstallation)
            .count(),
        4
    );
    assert!(
        edge_kinds(tmp.path(), "backend/settings.py")
            .iter()
            .any(|kind| *kind == EdgeKind::InstallsMiddleware)
    );

    let worker_nodes = node_kinds(tmp.path(), "edge/worker.mjs");
    assert!(worker_nodes.contains(&NodeKind::WorkerSurface));
    assert!(worker_nodes.contains(&NodeKind::ProxySurface));
    assert!(worker_nodes.contains(&NodeKind::CredentialOperation));

    let config_nodes = node_kinds(tmp.path(), "netlify.toml");
    assert!(config_nodes.contains(&NodeKind::ProxySurface));
    assert!(config_nodes.contains(&NodeKind::CredentialOperation));

    let test_nodes = node_kinds(tmp.path(), "backend/tests/test_auth_flow.py");
    assert!(test_nodes.contains(&NodeKind::BehaviorTestSurface));
}
