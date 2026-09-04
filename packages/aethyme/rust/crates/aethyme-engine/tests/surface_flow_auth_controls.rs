//! End-to-end controls for the Surface/Flow auth milestone.
//!
//! These tests intentionally build tiny throwaway repos instead of using
//! Aethyme itself. They exercise the production CLI path from source
//! fragments through redb into `aethyme explore`.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Instant;

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk};
use aethyme_graph_storage::bootstrap_repo;

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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[derive(Clone, Copy)]
struct AuthFixtureOptions {
    proxy: bool,
    backend_validator: bool,
    behavior_tests: bool,
    decoys: bool,
}

impl AuthFixtureOptions {
    fn positive() -> Self {
        Self {
            proxy: true,
            backend_validator: true,
            behavior_tests: true,
            decoys: true,
        }
    }
}

#[derive(Debug)]
struct ExploreMetrics {
    duration_ms: u128,
    command_output_chars: usize,
    token_estimate: usize,
}

fn build_auth_fixture(options: AuthFixtureOptions) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", b"# Surface Flow Auth Fixture\n");

    if options.proxy {
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
    }

    if options.backend_validator {
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
    } else {
        write(
            tmp.path(),
            "backend/settings.py",
            br#"
MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
]

OIDC_CLIENT_ID = "oidc-decoy-client"
"#,
        );
    }

    if options.decoys {
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
    }

    if options.behavior_tests && options.proxy {
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
    }
    if options.behavior_tests && options.backend_validator {
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
    }

    let root = tmp.path().canonicalize().unwrap();
    bootstrap_repo(&root, "test-engine").expect("bootstrap graph layout");
    let ctx = IndexerContext::new("SurfaceFlowAuthControls", root, "test-engine").unwrap();
    let summary = index_repo_to_disk(&ctx, &WalkOptions::default()).expect("write fragments");
    assert!(summary.total_files > 0);

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

fn explore(repo: &Path, request: &str) -> (serde_json::Value, ExploreMetrics) {
    let start = Instant::now();
    let output = run_aethyme([
        "explore",
        "--repo",
        repo.to_str().unwrap(),
        "--request",
        request,
        "--format",
        "answer-json",
        "--intent",
        "task_localization_query",
        "--show-observability",
    ]);
    let duration_ms = start.elapsed().as_millis();
    assert_success(&output);
    let output_text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("aethyme explore JSON parses");
    (
        value,
        ExploreMetrics {
            duration_ms,
            command_output_chars: output_text.len(),
            token_estimate: (output_text.len() + 3) / 4,
        },
    )
}

fn roles(response: &serde_json::Value) -> Vec<String> {
    response["subsystems"]
        .as_array()
        .expect("subsystems")
        .iter()
        .filter_map(|subsystem| subsystem["role"].as_str().map(str::to_string))
        .collect()
}

fn subsystem_by_role<'a>(
    response: &'a serde_json::Value,
    role: &str,
) -> Option<&'a serde_json::Value> {
    response["subsystems"]
        .as_array()
        .expect("subsystems")
        .iter()
        .find(|subsystem| subsystem["role"] == role)
}

fn verification_steps_text(response: &serde_json::Value) -> String {
    response["verification_steps"]
        .as_array()
        .expect("verification steps")
        .iter()
        .filter_map(|step| step["step"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn top_target_text(subsystem: &serde_json::Value) -> String {
    subsystem["top_verification_targets"]
        .as_array()
        .expect("top verification targets")
        .first()
        .map(serde_json::Value::to_string)
        .unwrap_or_default()
}

fn has_behavior_test_warning(response: &serde_json::Value) -> bool {
    response["subsystems"]
        .as_array()
        .expect("subsystems")
        .iter()
        .any(|subsystem| {
            subsystem["missing_coverage_warnings"]
                .as_array()
                .expect("missing coverage warnings")
                .iter()
                .any(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|text| text.contains("No linked live behavior test"))
                })
        })
}

fn without_runtime_performance(mut response: serde_json::Value) -> serde_json::Value {
    response["observability"]
        .as_object_mut()
        .expect("observability object")
        .remove("performance");
    response
}

#[test]
fn auth_surface_positive_contract_is_ranked_bounded_and_deterministic() {
    let tmp = build_auth_fixture(AuthFixtureOptions::positive());

    let (first, first_metrics) = explore(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
    );
    let (second, second_metrics) = explore(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
    );
    eprintln!("positive first metrics: {first_metrics:?}");
    eprintln!("positive second metrics: {second_metrics:?}");

    assert_eq!(
        without_runtime_performance(first.clone()),
        without_runtime_performance(second),
        "repeated explore output should be deterministic"
    );
    assert_eq!(first["schema_version"], "aethyme-explore-v1");
    assert_eq!(first["observability"]["graph_store"]["backend"], "redb");
    assert_eq!(first["observability"]["graph_store"]["status"], "fresh");
    assert_eq!(
        first["observability"]["output_profile"], "agent_compact",
        "default show-observability should use the compact agent profile"
    );
    assert!(
        first["output_chars_estimate"].as_u64().unwrap_or(0) > 0,
        "output size estimate should be populated"
    );
    assert!(
        first_metrics.duration_ms > 0 && second_metrics.duration_ms > 0,
        "wall-time capture should be populated: first={first_metrics:?}, second={second_metrics:?}"
    );
    assert!(
        first_metrics.command_output_chars < 20_000,
        "output should stay bounded: {first_metrics:?}"
    );
    assert!(
        first_metrics.token_estimate < 5_000,
        "token estimate should stay bounded: {first_metrics:?}"
    );
    assert!(
        !first.to_string().contains(".aethyme"),
        "generated graph artifacts must not leak into Explore output"
    );

    let roles = roles(&first);
    assert_eq!(
        roles.iter().take(2).cloned().collect::<Vec<_>>(),
        vec!["ingress_proxy".to_string(), "backend_validator".to_string()]
    );
    let ingress = subsystem_by_role(&first, "ingress_proxy").expect("ingress/proxy subsystem");
    let ingress_top = top_target_text(ingress);
    assert!(
        ingress_top.contains("gcp-run-proxy"),
        "ingress/proxy top target should point at gcp-run-proxy: {ingress_top}"
    );
    let backend = subsystem_by_role(&first, "backend_validator").expect("backend subsystem");
    let backend_top = top_target_text(backend);
    assert!(
        backend_top.contains("backend/api_keys"),
        "backend validator top target should point at backend/api_keys: {backend_top}"
    );

    let ambiguity = first["ambiguous"].to_string();
    assert!(ambiguity.contains("OIDC"));
    assert!(ambiguity.contains("audit JWS"));
    assert!(ambiguity.contains("Auth0 management"));
    assert!(ambiguity.contains("webhook tokens"));

    let steps = verification_steps_text(&first);
    assert!(steps.contains("There are multiple token systems"));
    assert!(steps.contains("Verify proxy classification first, then backend validation"));
    assert!(
        !has_behavior_test_warning(&first),
        "positive fixture has proxy/backend auth behavior tests"
    );
}

#[test]
fn auth_surface_without_proxy_does_not_emit_proxy_crossing_hint() {
    let tmp = build_auth_fixture(AuthFixtureOptions {
        proxy: false,
        ..AuthFixtureOptions::positive()
    });

    let (response, _) = explore(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
    );
    let output = response.to_string();
    assert!(!output.contains("gcp-run-proxy"));
    if let Some(ingress) = subsystem_by_role(&response, "ingress_proxy") {
        let confidence = ingress["confidence"].as_f64().unwrap_or(1.0);
        assert!(
            confidence <= 0.68,
            "route-only ingress must have reduced confidence, got {confidence}: {ingress}"
        );
        assert!(
            ingress["missing_coverage_warnings"]
                .to_string()
                .contains("backend route-only"),
            "route-only ingress should warn that no edge/proxy surface matched: {ingress}"
        );
    }
    assert!(
        !verification_steps_text(&response).contains("Verify proxy classification first"),
        "proxy-specific verification should require actual proxy evidence"
    );
}

#[test]
fn auth_surface_without_backend_validator_does_not_invent_backend_lane() {
    let tmp = build_auth_fixture(AuthFixtureOptions {
        backend_validator: false,
        behavior_tests: false,
        ..AuthFixtureOptions::positive()
    });

    let (response, _) = explore(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
    );
    let roles = roles(&response);
    assert!(
        !roles.iter().any(|role| role == "backend_validator"),
        "proxy-only API-key evidence must not invent a backend validator lane: roles={roles:?}, subsystems={}",
        response["subsystems"]
    );
    assert!(
        !response["subsystems"]
            .to_string()
            .contains("backend/api_keys")
    );
}

#[test]
fn auth_surface_specific_oidc_request_does_not_force_api_key_proxy_path() {
    let tmp = build_auth_fixture(AuthFixtureOptions::positive());

    let (response, _) = explore(tmp.path(), "Trace OIDC id token validation");
    assert!(
        response.to_string().contains("OIDC"),
        "specific OIDC request should still surface OIDC evidence"
    );
    assert!(
        !verification_steps_text(&response).contains("Verify proxy classification first"),
        "specific provider-token requests should not force API-key proxy verification"
    );
}

#[test]
fn auth_surface_without_tests_reports_missing_live_behavior_coverage() {
    let tmp = build_auth_fixture(AuthFixtureOptions {
        behavior_tests: false,
        ..AuthFixtureOptions::positive()
    });

    let (response, _) = explore(
        tmp.path(),
        "Trace token authentication behavior for publishable API keys",
    );
    assert!(
        has_behavior_test_warning(&response),
        "missing proxy/backend behavior tests should be visible"
    );
}

#[test]
fn auth_surface_decoys_only_surfaces_uncertainty_without_ingress_or_backend() {
    let tmp = build_auth_fixture(AuthFixtureOptions {
        proxy: false,
        backend_validator: false,
        behavior_tests: false,
        decoys: true,
    });

    let (response, _) = explore(tmp.path(), "Trace token authentication behavior");
    let roles = roles(&response);
    assert!(
        !roles.iter().any(|role| role == "ingress_proxy"),
        "decoy-only fixture must not invent ingress: roles={roles:?}, subsystems={}",
        response["subsystems"]
    );
    assert!(
        !roles.iter().any(|role| role == "backend_validator"),
        "decoy-only fixture must not invent backend validation: roles={roles:?}, subsystems={}",
        response["subsystems"]
    );
    let provider =
        subsystem_by_role(&response, "provider_or_secondary_token").expect("provider subsystem");
    let token_subsystems = provider["token_subsystems"].to_string();
    assert!(token_subsystems.contains("OIDC"));
    assert!(token_subsystems.contains("audit JWS"));
    assert!(token_subsystems.contains("Auth0 management"));
    assert!(token_subsystems.contains("webhook tokens"));
}
