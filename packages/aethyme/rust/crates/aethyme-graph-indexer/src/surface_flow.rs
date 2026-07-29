//! Generic Surface/Flow extractors.
//!
//! This module adds behavior-level graph facts that ordinary symbol parsers
//! miss: ingress routes, worker/proxy surfaces, middleware installation,
//! credential operations, and tests tied to live auth behavior. The first pass
//! is intentionally framework-agnostic line scanning with small adapters for
//! common Django, JS/TS worker, deployment config, and test idioms.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_graph_schema::{
    Confidence, Edge, EdgeAttributes, EdgeKind, Node, NodeId, NodeKind, Source, SourceRange,
    SurfaceFlowNode,
};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;
use crate::language::{LanguageIndexError, LanguageIndexResult};

pub fn should_scan(indexed: &IndexedFile) -> bool {
    let path = indexed.source_path.to_ascii_lowercase();
    matches!(
        indexed.language.as_ref(),
        "python" | "javascript" | "typescript" | "json" | "yaml" | "toml" | "dockerfile"
    ) || is_test_path(&path)
        || is_deployment_config_path(&path)
        || path.ends_with("settings.py")
        || path.ends_with("urls.py")
        || path.contains("middleware")
        || path.contains("worker")
        || path.contains("proxy")
}

pub fn index_file(
    ctx: &IndexerContext,
    indexed: &IndexedFile,
    content: &str,
) -> Result<LanguageIndexResult, LanguageIndexError> {
    let mut builder = SurfaceBuilder::new(ctx, indexed);
    let lower_path = indexed.source_path.to_ascii_lowercase();

    if indexed.language.as_ref() == "python" {
        extract_python_django(&mut builder, content)?;
    }
    if matches!(indexed.language.as_ref(), "javascript" | "typescript") {
        extract_js_ts(&mut builder, content)?;
    }
    if is_deployment_config_path(&lower_path)
        || matches!(
            indexed.language.as_ref(),
            "json" | "yaml" | "toml" | "dockerfile"
        )
    {
        extract_config_iac(&mut builder, content)?;
    }
    if is_test_path(&lower_path) {
        extract_behavior_tests(&mut builder, content)?;
    }

    Ok(builder.finish())
}

struct SurfaceBuilder<'a> {
    ctx: &'a IndexerContext,
    indexed: &'a IndexedFile,
    file_id: NodeId,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    seen_nodes: BTreeSet<String>,
    seen_edges: BTreeSet<(String, String, EdgeKind)>,
}

impl<'a> SurfaceBuilder<'a> {
    fn new(ctx: &'a IndexerContext, indexed: &'a IndexedFile) -> Self {
        Self {
            ctx,
            indexed,
            file_id: indexed.top_node.id().clone(),
            nodes: Vec::new(),
            edges: Vec::new(),
            seen_nodes: BTreeSet::new(),
            seen_edges: BTreeSet::new(),
        }
    }

    fn push(
        &mut self,
        kind: NodeKind,
        edge_attrs: EdgeAttributes,
        raw_name: &str,
        detail: &str,
        line: u32,
        metadata: &[(&str, String)],
    ) -> Result<(), LanguageIndexError> {
        self.push_with_edges(kind, &[edge_attrs], raw_name, detail, line, metadata)
    }

    fn push_with_edges(
        &mut self,
        kind: NodeKind,
        edge_attrs: &[EdgeAttributes],
        raw_name: &str,
        detail: &str,
        line: u32,
        metadata: &[(&str, String)],
    ) -> Result<(), LanguageIndexError> {
        let name = compact_name(raw_name);
        if name.is_empty() {
            return Ok(());
        }
        let range = SourceRange::new(line.max(1), line.max(1)).map_err(|e| {
            LanguageIndexError::NodeConstruction {
                message: e.to_string(),
            }
        })?;
        let metadata = metadata
            .iter()
            .map(|(key, value)| ((*key).into(), value.as_str().into()))
            .collect::<BTreeMap<Box<str>, Box<str>>>();
        let surface = SurfaceFlowNode::new(
            kind,
            self.ctx.repo_name(),
            &self.indexed.source_path,
            &name,
            detail,
            range,
            metadata,
        )
        .map_err(|e| LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        })?;
        let id = surface.id().clone();
        if self.seen_nodes.insert(id.as_str().to_string()) {
            self.nodes.push(node_for_kind(kind, surface));
        }
        for attrs in edge_attrs {
            self.push_file_edge(id.clone(), attrs.clone());
        }
        Ok(())
    }

    fn push_file_edge(&mut self, to: NodeId, attrs: EdgeAttributes) {
        let edge_kind = attrs.kind();
        let key = (
            self.file_id.as_str().to_string(),
            to.as_str().to_string(),
            edge_kind,
        );
        if !self.seen_edges.insert(key) {
            return;
        }
        self.edges.push(Edge::new(
            self.file_id.clone(),
            to,
            attrs,
            Source::Derived,
            Confidence::from_milli(850).expect("surface-flow confidence is in range"),
        ));
    }

    fn finish(self) -> LanguageIndexResult {
        LanguageIndexResult {
            additional_nodes: self.nodes,
            additional_edges: self.edges,
        }
    }
}

fn node_for_kind(kind: NodeKind, surface: SurfaceFlowNode) -> Node {
    match kind {
        NodeKind::BehaviorTestSurface => Node::BehaviorTestSurface(surface),
        NodeKind::CliSurface => Node::CliSurface(surface),
        NodeKind::CredentialOperation => Node::CredentialOperation(surface),
        NodeKind::JobSurface => Node::JobSurface(surface),
        NodeKind::MiddlewareInstallation => Node::MiddlewareInstallation(surface),
        NodeKind::ProxySurface => Node::ProxySurface(surface),
        NodeKind::QueueSurface => Node::QueueSurface(surface),
        NodeKind::RouteSurface => Node::RouteSurface(surface),
        NodeKind::WebhookSurface => Node::WebhookSurface(surface),
        NodeKind::WorkerSurface => Node::WorkerSurface(surface),
        _ => unreachable!("surface-flow builder called with non-surface kind"),
    }
}

fn extract_python_django(
    builder: &mut SurfaceBuilder<'_>,
    content: &str,
) -> Result<(), LanguageIndexError> {
    let path = builder.indexed.source_path.to_ascii_lowercase();
    let mut in_middleware = false;
    let mut in_drf_auth = false;
    let mut in_drf_permission = false;

    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if path.ends_with("urls.py")
            && (lower.contains("path(") || lower.contains("re_path(") || lower.contains("url("))
        {
            if let Some(pattern) = first_quoted_value(trimmed) {
                builder.push(
                    NodeKind::RouteSurface,
                    EdgeAttributes::Exposes,
                    &format!("route:{pattern}"),
                    "Django URL route",
                    line_no,
                    &[("framework", "django".to_string()), ("pattern", pattern)],
                )?;
            }
        }

        if lower.contains("middleware") && lower.contains('[') {
            in_middleware = true;
        }
        if lower.contains("default_authentication_classes") && lower.contains('[') {
            in_drf_auth = true;
        }
        if lower.contains("default_permission_classes") && lower.contains('[') {
            in_drf_permission = true;
        }

        if in_middleware {
            for value in quoted_values(trimmed) {
                if value.to_ascii_lowercase().contains("middleware") {
                    builder.push(
                        NodeKind::MiddlewareInstallation,
                        EdgeAttributes::InstallsMiddleware,
                        &value,
                        "Django middleware installation",
                        line_no,
                        &[("framework", "django".to_string())],
                    )?;
                }
            }
            if lower.contains(']') {
                in_middleware = false;
            }
        }

        if in_drf_auth || in_drf_permission {
            for value in quoted_values(trimmed) {
                if value.starts_with("DEFAULT_") {
                    continue;
                }
                let (detail, edge, role) = if in_drf_auth {
                    (
                        "DRF authentication class",
                        EdgeAttributes::ValidatesCredential,
                        "drf_authentication_class",
                    )
                } else {
                    (
                        "DRF permission class",
                        EdgeAttributes::Authorizes,
                        "drf_permission_class",
                    )
                };
                builder.push(
                    NodeKind::MiddlewareInstallation,
                    edge,
                    &value,
                    detail,
                    line_no,
                    &[
                        ("framework", "django_rest_framework".to_string()),
                        ("role", role.to_string()),
                    ],
                )?;
            }
            if lower.contains(']') {
                in_drf_auth = false;
                in_drf_permission = false;
            }
        }

        if trimmed.starts_with('@') && is_auth_decorator(&lower) {
            let decorator = trimmed
                .trim_start_matches('@')
                .split(['(', ' '])
                .next()
                .unwrap_or(trimmed);
            builder.push(
                NodeKind::CredentialOperation,
                EdgeAttributes::Authorizes,
                &format!("decorator:{decorator}"),
                "Python auth decorator",
                line_no,
                &[("framework", "python".to_string())],
            )?;
        }

        extract_credential_lifecycle_line(builder, trimmed, &lower, line_no, "python")?;
    }

    Ok(())
}

fn extract_js_ts(
    builder: &mut SurfaceBuilder<'_>,
    content: &str,
) -> Result<(), LanguageIndexError> {
    let path = builder.indexed.source_path.to_ascii_lowercase();
    let lower_content = content.to_ascii_lowercase();

    if lower_content.contains("addEventListener(\"fetch\"")
        || lower_content.contains("addeventlistener('fetch'")
        || lower_content.contains("export default")
            && (lower_content.contains(" fetch(") || lower_content.contains("async fetch("))
        || lower_content.contains("export function onrequest")
        || path.contains("functions/_middleware.")
    {
        let line = find_line(content, "fetch").unwrap_or(1);
        builder.push(
            NodeKind::WorkerSurface,
            EdgeAttributes::Exposes,
            worker_name_for_path(&path),
            "JS/TS fetch worker surface",
            line,
            &[("runtime", "javascript_worker".to_string())],
        )?;
    }

    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if let Some((method, route)) = js_route_call(trimmed) {
            builder.push(
                NodeKind::RouteSurface,
                EdgeAttributes::Exposes,
                &format!("{method} {route}"),
                "JS/TS route handler",
                line_no,
                &[("framework", "javascript".to_string()), ("method", method)],
            )?;
        }

        if lower.contains(".use(") || lower.contains("middleware") {
            if let Some(name) = first_quoted_value(trimmed).or_else(|| callable_name(trimmed)) {
                builder.push(
                    NodeKind::MiddlewareInstallation,
                    EdgeAttributes::InstallsMiddleware,
                    &name,
                    "JS/TS middleware installation",
                    line_no,
                    &[("framework", "javascript".to_string())],
                )?;
            }
        }

        if lower.contains("fetch(")
            && (lower.contains("http://")
                || lower.contains("https://")
                || lower.contains("upstream")
                || lower.contains("proxy")
                || path.contains("proxy"))
        {
            let target = first_quoted_value(trimmed).unwrap_or_else(|| "fetch_proxy".to_string());
            builder.push(
                NodeKind::ProxySurface,
                EdgeAttributes::ForwardsTo,
                &format!("proxy:{target}"),
                "JS/TS fetch proxy target",
                line_no,
                &[("runtime", "javascript".to_string())],
            )?;
        }

        if lower.contains("headers.set(")
            || lower.contains("headers.append(")
            || lower.contains("new headers")
        {
            let credentialish = contains_credential_term(&lower);
            if credentialish {
                builder.push_with_edges(
                    NodeKind::CredentialOperation,
                    &[
                        EdgeAttributes::RewritesHeader,
                        EdgeAttributes::UsesCredential,
                    ],
                    header_mutation_name(trimmed),
                    "JS/TS request or response header mutation",
                    line_no,
                    &[("runtime", "javascript".to_string())],
                )?;
            } else {
                builder.push(
                    NodeKind::MiddlewareInstallation,
                    EdgeAttributes::RewritesHeader,
                    header_mutation_name(trimmed),
                    "JS/TS request or response header mutation",
                    line_no,
                    &[("runtime", "javascript".to_string())],
                )?;
            }
        }

        extract_credential_lifecycle_line(builder, trimmed, &lower, line_no, "javascript")?;
    }

    Ok(())
}

fn extract_config_iac(
    builder: &mut SurfaceBuilder<'_>,
    content: &str,
) -> Result<(), LanguageIndexError> {
    let path = builder.indexed.source_path.to_ascii_lowercase();
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        if looks_like_proxy_config_line(&lower) {
            let name = first_quoted_value(trimmed)
                .or_else(|| config_value_after_separator(trimmed))
                .unwrap_or_else(|| format!("proxy:{}", compact_name(trimmed)));
            builder.push(
                NodeKind::ProxySurface,
                EdgeAttributes::ForwardsTo,
                &name,
                "Deployment proxy or route configuration",
                line_no,
                &[("config_path", path.clone())],
            )?;
        }

        if looks_like_env_auth_setting(trimmed) {
            let key = trimmed
                .split(['=', ':'])
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("auth_setting");
            builder.push(
                NodeKind::CredentialOperation,
                EdgeAttributes::UsesCredential,
                &format!("env:{key}"),
                "Environment-driven auth or credential setting",
                line_no,
                &[("config_path", path.clone())],
            )?;
        }
    }
    Ok(())
}

fn extract_behavior_tests(
    builder: &mut SurfaceBuilder<'_>,
    content: &str,
) -> Result<(), LanguageIndexError> {
    let lower_content = content.to_ascii_lowercase();
    if !contains_credential_term(&lower_content)
        && !lower_content.contains("auth")
        && !lower_content.contains("route")
        && !lower_content.contains("login")
    {
        return Ok(());
    }

    let mut emitted = false;
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("def test_")
            || lower.starts_with("async def test_")
            || lower.starts_with("it(")
            || lower.starts_with("test(")
            || lower.starts_with("describe(")
        {
            builder.push_with_edges(
                NodeKind::BehaviorTestSurface,
                &[
                    EdgeAttributes::TestedBy,
                    EdgeAttributes::ValidatesCredential,
                ],
                &test_name(trimmed),
                "Route/auth behavior test",
                line_no,
                &[("test_path", builder.indexed.source_path.to_string())],
            )?;
            emitted = true;
        }
    }

    if !emitted {
        builder.push_with_edges(
            NodeKind::BehaviorTestSurface,
            &[
                EdgeAttributes::TestedBy,
                EdgeAttributes::ValidatesCredential,
            ],
            "auth behavior coverage",
            "Route/auth behavior test",
            1,
            &[("test_path", builder.indexed.source_path.to_string())],
        )?;
    }
    Ok(())
}

fn extract_credential_lifecycle_line(
    builder: &mut SurfaceBuilder<'_>,
    trimmed: &str,
    lower: &str,
    line_no: u32,
    runtime: &str,
) -> Result<(), LanguageIndexError> {
    if !contains_credential_term(lower) {
        return Ok(());
    }
    if looks_like_credential_validation(lower) {
        builder.push(
            NodeKind::CredentialOperation,
            EdgeAttributes::ValidatesCredential,
            &credential_lifecycle_name("validate", trimmed),
            "Credential validation operation",
            line_no,
            &[("runtime", runtime.to_string())],
        )?;
    }
    if looks_like_credential_issue(lower) {
        builder.push(
            NodeKind::CredentialOperation,
            EdgeAttributes::IssuesCredential,
            &credential_lifecycle_name("issue", trimmed),
            "Credential issue operation",
            line_no,
            &[("runtime", runtime.to_string())],
        )?;
    }
    if looks_like_credential_store(lower) {
        builder.push(
            NodeKind::CredentialOperation,
            EdgeAttributes::StoresCredential,
            &credential_lifecycle_name("store", trimmed),
            "Credential store operation",
            line_no,
            &[("runtime", runtime.to_string())],
        )?;
    }
    Ok(())
}

fn is_test_path(path: &str) -> bool {
    path.contains("/test")
        || path.contains("/tests")
        || path.contains(".test.")
        || path.contains(".spec.")
        || path.ends_with("_test.py")
        || path.starts_with("test_")
}

fn is_deployment_config_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file == "vercel.json"
        || file == "wrangler.json"
        || file == "wrangler.jsonc"
        || file == "netlify.toml"
        || file == "render.yaml"
        || file == "render.yml"
        || path.contains("/deploy/")
        || path.contains("/deployment/")
        || path.contains("/infra/")
        || path.contains("/k8s/")
        || path.contains("/helm/")
}

fn is_auth_decorator(lower: &str) -> bool {
    lower.contains("login_required")
        || lower.contains("permission_required")
        || lower.contains("authentication_classes")
        || lower.contains("permission_classes")
        || lower.contains("require_scope")
        || lower.contains("csrf")
        || lower.contains("api_view")
}

fn contains_credential_term(lower: &str) -> bool {
    lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("jwt")
        || lower.contains("jws")
        || lower.contains("oidc")
        || lower.contains("cookie")
        || lower.contains("credential")
        || lower.contains("secret")
}

fn looks_like_credential_validation(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "authenticate",
            "authorize",
            "permission",
            "require_scope",
            "scope",
            "validate",
            "verify",
        ],
    )
}

fn looks_like_credential_issue(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "issue",
            "mint",
            "generate",
            "create",
            "sign",
            "jwt.encode",
            "encode_jwt",
            "new_api_key",
        ],
    )
}

fn looks_like_credential_store(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "store",
            "save",
            "persist",
            "set_cookie",
            "cookies.set",
            "session[",
            "localstorage.setitem",
            "securestorage",
        ],
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn credential_lifecycle_name(action: &str, line: &str) -> String {
    if let Some(value) = first_quoted_value(line) {
        format!("{action}:{value}")
    } else {
        format!("{action}:{}", compact_name(line))
    }
}

fn looks_like_proxy_config_line(lower: &str) -> bool {
    (lower.contains("route") || lower.contains("routes") || lower.contains("rewrite"))
        && (lower.contains('/') || lower.contains("http"))
        || lower.contains("proxy")
        || lower.contains("target")
        || lower.contains("destination")
        || lower.contains("upstream")
        || lower.starts_with("to =")
        || lower.starts_with("to:")
        || lower.starts_with("from =")
        || lower.starts_with("from:")
}

fn looks_like_env_auth_setting(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        return false;
    }
    let key = trimmed
        .split(['=', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    !key.is_empty()
        && (key.contains("auth")
            || key.contains("token")
            || key.contains("jwt")
            || key.contains("oidc")
            || key.contains("secret")
            || key.contains("credential"))
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((start, quote)) = chars.next() {
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value_start = start + quote.len_utf8();
        let mut escaped = false;
        for (end, ch) in chars.by_ref() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote {
                values.push(line[value_start..end].to_string());
                break;
            }
        }
    }
    values
}

fn first_quoted_value(line: &str) -> Option<String> {
    quoted_values(line).into_iter().next()
}

fn config_value_after_separator(line: &str) -> Option<String> {
    line.split(['=', ':'])
        .nth(1)
        .map(|value| {
            value
                .trim()
                .trim_matches(',')
                .trim_matches('"')
                .trim_matches('\'')
        })
        .filter(|value| !value.is_empty())
        .map(compact_name)
}

fn js_route_call(line: &str) -> Option<(String, String)> {
    let lower = line.to_ascii_lowercase();
    for method in ["get", "post", "put", "patch", "delete", "all", "use"] {
        let app = format!(".{method}(");
        if lower.contains(&app) {
            let route = first_quoted_value(line)?;
            if route.starts_with('/') {
                return Some((method.to_ascii_uppercase(), route));
            }
        }
    }
    None
}

fn callable_name(line: &str) -> Option<String> {
    line.split(['(', ',', ')'])
        .find(|part| {
            let trimmed = part.trim();
            !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
        })
        .map(|part| compact_name(part.trim()))
}

fn header_mutation_name(line: &str) -> &str {
    if line.to_ascii_lowercase().contains("authorization") {
        "header:authorization"
    } else if line.to_ascii_lowercase().contains("cookie") {
        "header:cookie"
    } else {
        "header:mutation"
    }
}

fn worker_name_for_path(path: &str) -> &str {
    if path.contains("functions/_middleware.") {
        "functions/_middleware"
    } else {
        "fetch"
    }
}

fn test_name(line: &str) -> String {
    if let Some(value) = first_quoted_value(line) {
        return compact_name(&value);
    }
    line.split(['(', ':'])
        .next()
        .map(|value| {
            compact_name(
                value
                    .trim_start_matches("async ")
                    .trim_start_matches("def ")
                    .trim(),
            )
        })
        .unwrap_or_else(|| "auth behavior test".to_string())
}

fn find_line(content: &str, needle: &str) -> Option<u32> {
    let needle = needle.to_ascii_lowercase();
    content
        .lines()
        .enumerate()
        .find(|(_, line)| line.to_ascii_lowercase().contains(&needle))
        .map(|(idx, _)| (idx + 1) as u32)
}

fn compact_name(value: &str) -> String {
    let mut out = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.len() > 120 {
        out.truncate(120);
    }
    out
}
