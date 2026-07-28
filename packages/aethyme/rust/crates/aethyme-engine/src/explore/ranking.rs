//! Request-sensitive ranking signals shared by Explore evidence passes.
//!
//! These signals are intentionally generic. For broad auth/token requests, many
//! subsystems legitimately mention "token"; ranking should prefer inbound
//! request-path surfaces and credential issue/auth pairs before incidental
//! outbound provider helpers. Non-auth requests receive a zero score and keep
//! the existing ranking order. Scores surface as optional evidence metadata
//! only; they are not required response-schema fields.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SurfaceSignals {
    pub(super) score: i32,
    pub(super) labels: Vec<&'static str>,
}

pub(super) fn auth_token_focus_from_request(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    auth_token_focus_from_lowered_text(&lower)
}

pub(super) fn auth_token_focus_from_terms(terms: &[String]) -> bool {
    let joined = terms
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    auth_token_focus_from_lowered_text(&joined)
}

pub(super) fn auth_token_surface_signals(
    path: &str,
    symbol_names: &[String],
    request: &str,
) -> SurfaceSignals {
    if !auth_token_focus_from_request(request) {
        return SurfaceSignals::default();
    }
    let lower_request = request.to_ascii_lowercase();
    surface_signals(path, symbol_names, Some(lower_request.as_str()))
}

pub(super) fn auth_token_surface_signals_for_terms(
    path: &str,
    symbol_names: &[String],
    terms: &[String],
) -> SurfaceSignals {
    if !auth_token_focus_from_terms(terms) {
        return SurfaceSignals::default();
    }
    let joined_terms = terms
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    surface_signals(path, symbol_names, Some(joined_terms.as_str()))
}

pub(super) fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.")
}

fn auth_token_focus_from_lowered_text(lower: &str) -> bool {
    [
        "api key",
        "api-key",
        "api_key",
        "apikey",
        "auth",
        "authentication",
        "authorization",
        "bearer",
        "credential",
        "jwt",
        "oauth",
        "oidc",
        "session",
        "token",
    ]
    .iter()
    .any(|needle| {
        if *needle == "auth" {
            contains_token(lower, "auth")
        } else {
            lower.contains(needle)
        }
    })
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| token == needle)
}

fn surface_signals(
    path: &str,
    symbol_names: &[String],
    request_lower: Option<&str>,
) -> SurfaceSignals {
    let lower_path = path.to_ascii_lowercase();
    let basename = lower_path.rsplit('/').next().unwrap_or(&lower_path);
    let runtime_surface = is_runtime_surface_path(&lower_path);
    let symbols = symbol_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let combined = format!("{lower_path} {symbols}");
    let mut signals = SurfaceSignals::default();
    let auth_path = lower_path.contains("/auth")
        || lower_path.contains("/accounts/")
        || lower_path.contains("api_keys")
        || lower_path.contains("api-key")
        || lower_path.contains("api_key")
        || lower_path.contains("session")
        || lower_path.contains("oidc")
        || lower_path.contains("oauth")
        || lower_path.contains("jwt")
        || lower_path.contains("token")
        || lower_path.contains("credential");
    let auth_symbol = auth_token_focus_from_lowered_text(&symbols)
        || contains_any(&symbols, &["validate", "validation", "verify"]);
    let auth_locality = auth_path || auth_symbol;

    let is_test = is_test_path(path);
    if is_test && auth_locality {
        signals.score -= 35;
    } else if runtime_surface && auth_locality {
        add(&mut signals, 10, "production_code");
    }

    if runtime_surface
        && auth_locality
        && (lower_path.contains("middleware")
            || symbols.contains("middleware")
            || symbols.contains("authentication"))
    {
        add(&mut signals, 90, "middleware_request_path");
    }

    if runtime_surface
        && auth_locality
        && (basename == "urls.py"
            || basename.contains("routes")
            || basename.contains("router")
            || lower_path.contains("/routes/")
            || lower_path.contains("/functions/api/")
            || lower_path.contains("/gcp-run-proxy/")
            || lower_path.contains("worker."))
    {
        add(&mut signals, 70, "externally_exposed_route");
    }

    if runtime_surface
        && auth_locality
        && (lower_path.contains("/api/")
            || lower_path.contains("api_keys")
            || lower_path.contains("api-key")
            || basename.contains("api"))
    {
        add(&mut signals, 55, "api_boundary");
    }

    if runtime_surface
        && (lower_path.contains("/auth")
            || lower_path.contains("/accounts/")
            || lower_path.contains("api_keys")
            || lower_path.contains("session")
            || lower_path.contains("oidc")
            || lower_path.contains("oauth")
            || lower_path.contains("jwt"))
    {
        add(&mut signals, 25, "auth_namespace");
    }

    if runtime_surface && auth_locality && (basename.contains("model") || basename == "models.py") {
        add(&mut signals, 35, "credential_model");
    }
    if runtime_surface
        && auth_locality
        && (basename.contains("view")
            || basename == "views.py"
            || basename.contains("controller")
            || basename.contains("handler"))
    {
        add(&mut signals, 30, "request_handler");
    }

    let issues_credentials = contains_any(
        &symbols,
        &[
            "create_key",
            "generate_api_key",
            "issue",
            "token_issued",
            "create_token",
            "generate_token",
            "mint",
        ],
    );
    let authenticates_credentials = contains_any(
        &symbols,
        &[
            "authenticate",
            "authentication",
            "validate",
            "verify",
            "check_token",
            "verify_id_token",
        ],
    );
    let credential_path = lower_path.contains("api_keys")
        || lower_path.contains("token")
        || lower_path.contains("credential")
        || lower_path.contains("session");
    if runtime_surface
        && ((issues_credentials && authenticates_credentials)
            || (credential_path && (basename == "models.py" || basename.contains("middleware"))))
    {
        add(&mut signals, 80, "issue_auth_credential_pair");
    } else if runtime_surface && (issues_credentials || authenticates_credentials) {
        add(&mut signals, 35, "credential_operation_symbol");
    }

    if runtime_surface
        && is_test
        && contains_any(
            &lower_path,
            &[
                "auth",
                "api_key",
                "apikey",
                "bearer",
                "integration",
                "security",
                "session",
                "token",
            ],
        )
    {
        add(&mut signals, 30, "live_auth_behavior_test");
    }

    let outbound_provider_helper =
        contains_any(
            &lower_path,
            &[
                "/adapters/",
                "/integrations/",
                "/sdk",
                "auth0_management",
                "client.",
                "management",
                "provider",
            ],
        ) && !contains_any(&lower_path, &["middleware", "urls.py", "api_keys"]);
    if outbound_provider_helper {
        if request_targets_outbound_provider_helper(request_lower, &combined) {
            add(&mut signals, 280, "named_provider_management_helper");
        } else {
            add(&mut signals, -35, "outbound_provider_helper");
        }
    }

    signals
}

fn request_targets_outbound_provider_helper(
    request_lower: Option<&str>,
    candidate_lower: &str,
) -> bool {
    let Some(request_lower) = request_lower else {
        return false;
    };
    if request_lower.contains("auth0") {
        return candidate_lower.contains("auth0");
    }
    if request_lower.contains("provider management")
        || request_lower.contains("provider-management")
        || request_lower.contains("external provider")
    {
        return contains_any(
            candidate_lower,
            &["provider", "management", "/integrations/", "/adapters/"],
        );
    }
    if request_lower.contains("management token")
        || request_lower.contains("management api")
        || request_lower.contains("oauth management")
    {
        return contains_any(candidate_lower, &["management", "provider", "oauth"]);
    }
    false
}

fn is_runtime_surface_path(lower_path: &str) -> bool {
    let suffix = lower_path.rsplit('.').next().unwrap_or("");
    matches!(
        suffix,
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "mjs"
            | "cjs"
            | "vue"
            | "svelte"
            | "yml"
            | "yaml"
            | "toml"
            | "ini"
            | "conf"
            | "config"
    )
}

fn add(signals: &mut SurfaceSignals, score: i32, label: &'static str) {
    signals.score += score;
    if !signals.labels.contains(&label) {
        signals.labels.push(label);
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_token_focus_is_request_sensitive() {
        assert!(auth_token_focus_from_request(
            "trace API key token validation"
        ));
        assert!(auth_token_focus_from_request("trace api_key validation"));
        assert!(auth_token_focus_from_request("trace auth middleware"));
        assert!(auth_token_focus_from_request(
            "describe authentication behavior"
        ));
        assert!(!auth_token_focus_from_request(
            "explain author profile behavior"
        ));
        assert!(!auth_token_focus_from_request(
            "explain repository architecture"
        ));
    }

    #[test]
    fn auth_surface_prefers_inbound_request_paths_over_outbound_helpers() {
        let request = "trace token validation behavior";
        let middleware = auth_token_surface_signals(
            "backend/api_keys/middleware.py",
            &[
                "APIKeyAuthenticationMiddleware".into(),
                "authenticate".into(),
            ],
            request,
        );
        let outbound = auth_token_surface_signals(
            "backend/accounts/auth0_management.py",
            &["get_management_token".into()],
            request,
        );

        assert!(middleware.score > outbound.score);
        assert!(middleware.labels.contains(&"middleware_request_path"));
        assert!(middleware.labels.contains(&"api_boundary"));
        assert!(outbound.labels.contains(&"outbound_provider_helper"));
    }

    #[test]
    fn outbound_provider_helper_not_penalized_when_request_names_it() {
        let signals = auth_token_surface_signals(
            "backend/accounts/auth0_management.py",
            &["get_management_token".into()],
            "trace Auth0 management token behavior",
        );

        assert!(
            !signals.labels.contains(&"outbound_provider_helper"),
            "explicit provider-management tasks should not suppress the named helper"
        );
        assert!(
            signals.labels.contains(&"named_provider_management_helper"),
            "explicit provider-management tasks should promote the named helper"
        );
    }

    #[test]
    fn auth0_request_does_not_promote_unmatched_integration_helpers() {
        let signals = auth_token_surface_signals(
            "backend/integrations/api_views.py",
            &["decrypt_token".into()],
            "trace Auth0 management token behavior",
        );

        assert!(
            !signals.labels.contains(&"named_provider_management_helper"),
            "Auth0-specific requests should not promote unrelated integration helpers"
        );
        assert!(
            signals.labels.contains(&"outbound_provider_helper"),
            "unmatched integration helpers remain secondary for Auth0-specific tasks"
        );
    }

    #[test]
    fn credential_model_pair_beats_incidental_profile_token_view() {
        let request = "find token issuing and validation behavior";
        let model = auth_token_surface_signals(
            "backend/api_keys/models.py",
            &["generate_api_key".into(), "authenticate".into()],
            request,
        );
        let profile_view = auth_token_surface_signals(
            "backend/accounts/platform_users_views.py",
            &["_issue_profile_integrity_token".into()],
            request,
        );

        assert!(model.score > profile_view.score);
        assert!(model.labels.contains(&"issue_auth_credential_pair"));
    }

    #[test]
    fn generic_api_route_without_auth_locality_gets_no_surface_bonus() {
        let signals = auth_token_surface_signals(
            "packages/app-shared/src/components/admin/AdminApiRoutesDiagnostics.tsx",
            &["onChange".into()],
            "trace token validation behavior",
        );

        assert_eq!(signals.score, 0);
        assert!(signals.labels.is_empty());
    }

    #[test]
    fn markdown_reference_does_not_count_as_runtime_middleware() {
        let signals = auth_token_surface_signals(
            "Agents/skills/api/references/advanced-middleware.md",
            &["token validation".into()],
            "trace token validation behavior",
        );

        assert_eq!(signals.score, 0);
        assert!(signals.labels.is_empty());
    }

    #[test]
    fn live_auth_tests_get_signal_but_stay_below_production() {
        let request = "assess auth token behavior";
        let production = auth_token_surface_signals(
            "backend/api_keys/models.py",
            &["authenticate".into()],
            request,
        );
        let test = auth_token_surface_signals(
            "backend/tests/integration/test_api_key_rate_limit_policy.py",
            &["test_api_key_rate_limit".into()],
            request,
        );

        assert!(test.labels.contains(&"live_auth_behavior_test"));
        assert!(production.score > test.score);
    }
}
