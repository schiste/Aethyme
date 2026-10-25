//! Shared synthetic surface-flow inputs; expected behavior stays in the tests.

use std::path::Path;

fn write(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

pub fn write_surface_fixture(root: &Path) {
    write(
        root,
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
        root,
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
        root,
        "edge/worker.mjs",
        r#"
export default {
  async fetch(request, env) {
    const headers = new Headers(request.headers)
    const issued = jwt.sign({ sub: "u1" }, env.JWT_SECRET)
    await tokenStore.save(issued)
    headers.set("Authorization", `Bearer ${env.API_TOKEN}`)
    return fetch("https://api.example.com/token", { headers })
  }
}
"#,
    );
    write(
        root,
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
        root,
        "backend/tests/test_auth_flow.py",
        r#"
def test_token_route_requires_bearer(client):
    response = client.get("/api/token/", HTTP_AUTHORIZATION="Bearer test-token")
    assert response.status_code == 200
"#,
    );
}
