//! Graph-store freshness and Surface/Flow coverage observability: which
//! ingress, proxy, middleware, credential and live-test surfaces the
//! source tree suggests versus what the committed graph indexes.

use super::*;

pub(super) const SURFACE_FLOW_COVERAGE_SCHEMA_VERSION: u8 = 1;
pub(super) const SURFACE_FLOW_MAX_FRAGMENT_BYTES: u64 = 8_192;
pub(super) const SURFACE_FLOW_MAX_PATH_HINTS: usize = 8;
pub(super) const SURFACE_FLOW_MAX_SCANNED_PATHS: usize = 20_000;

pub(super) const SURFACE_FLOW_IGNORED_DIRS: &[&str] = &[
    ".aethyme",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "vendor",
];

pub(super) const BACKEND_PATTERNS: &[&str] = &[
    "backend/",
    "backend.",
    "server/",
    "server.",
    "api/",
    "api.",
    "services/",
    "services.",
];
pub(super) const EDGE_PROXY_PATTERNS: &[&str] = &[
    "cloudflare",
    "edge/",
    "functions/_middleware",
    "middleware.ts",
    "middleware.js",
    "netlify/functions",
    "proxy/",
    "proxy.",
    "vercel.json",
    "worker.",
    "workers/",
];
pub(super) const ROUTE_PATTERNS: &[&str] = &[
    "api_views.",
    "app.py",
    "controller",
    "routes/",
    "routes.",
    "router",
    "urls.py",
    "views.py",
];
pub(super) const MIDDLEWARE_PATTERNS: &[&str] = &["middleware", "interceptor", "filter"];
pub(super) const WEBHOOK_PATTERNS: &[&str] = &["webhook", "hooks/"];
pub(super) const CLI_PATTERNS: &[&str] =
    &["/bin/", "cli.", "cmd/", "command", "manage.py", "scripts/"];
pub(super) const JOB_QUEUE_PATTERNS: &[&str] = &[
    "celery",
    "cron",
    "job",
    "queue",
    "scheduler",
    "tasks.py",
    "worker.",
];
pub(super) const CREDENTIAL_PATTERNS: &[&str] = &[
    "api_key",
    "auth",
    "credential",
    "jwt",
    "oauth",
    "oidc",
    "permission",
    "rbac",
    "token",
];
pub(super) const LIVE_TEST_PATTERNS: &[&str] = &[
    ".spec.",
    ".test.",
    "/test/",
    "/tests/",
    "e2e/",
    "fixture",
    "integration",
];
pub(super) const BACKEND_SEMANTIC_TERMS: &[&str] = &[];
pub(super) const EDGE_PROXY_SEMANTIC_TERMS: &[&str] =
    &["worker_surface", "proxy_surface", "forwards_to"];
pub(super) const ROUTE_SEMANTIC_TERMS: &[&str] = &["route_surface"];
pub(super) const MIDDLEWARE_SEMANTIC_TERMS: &[&str] =
    &["middleware_installation", "installs_middleware"];
pub(super) const WEBHOOK_SEMANTIC_TERMS: &[&str] = &["webhook_surface"];
pub(super) const CLI_SEMANTIC_TERMS: &[&str] = &["cli_surface"];
pub(super) const JOB_QUEUE_SEMANTIC_TERMS: &[&str] = &["job_surface", "queue_surface"];
pub(super) const CREDENTIAL_SEMANTIC_TERMS: &[&str] = &[
    "credential_operation",
    "validates_credential",
    "authorizes",
    "issues_credential",
    "rewrites_header",
    "stores_credential",
    "uses_credential",
];
pub(super) const LIVE_TEST_SEMANTIC_TERMS: &[&str] = &["behavior_test_surface", "tested_by"];
pub(super) const SURFACE_FLOW_SEMANTIC_TERMS: &[&str] = &[
    "route_surface",
    "worker_surface",
    "proxy_surface",
    "webhook_surface",
    "cli_surface",
    "job_surface",
    "queue_surface",
    "middleware_installation",
    "credential_operation",
    "behavior_test_surface",
    "forwards_to",
    "installs_middleware",
    "validates_credential",
    "authorizes",
    "issues_credential",
    "rewrites_header",
    "stores_credential",
    "tested_by",
    "uses_credential",
];

#[derive(Clone, Copy)]
pub(super) struct SurfaceCoverageSpec {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) patterns: &'static [&'static str],
    pub(super) semantic_terms: &'static [&'static str],
}

pub(super) const SURFACE_COVERAGE_SPECS: &[SurfaceCoverageSpec] = &[
    SurfaceCoverageSpec {
        key: "backend",
        label: "backend service/application code",
        patterns: BACKEND_PATTERNS,
        semantic_terms: BACKEND_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "edge_proxy",
        label: "edge worker/proxy/gateway ingress",
        patterns: EDGE_PROXY_PATTERNS,
        semantic_terms: EDGE_PROXY_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "route",
        label: "HTTP route/controller/view surface",
        patterns: ROUTE_PATTERNS,
        semantic_terms: ROUTE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "middleware",
        label: "middleware/filter/interceptor installation",
        patterns: MIDDLEWARE_PATTERNS,
        semantic_terms: MIDDLEWARE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "webhook",
        label: "webhook ingress surface",
        patterns: WEBHOOK_PATTERNS,
        semantic_terms: WEBHOOK_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "cli",
        label: "CLI/command entrypoint surface",
        patterns: CLI_PATTERNS,
        semantic_terms: CLI_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "job_queue",
        label: "job/queue/cron/worker surface",
        patterns: JOB_QUEUE_PATTERNS,
        semantic_terms: JOB_QUEUE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "credential",
        label: "credential issue/store/use/validation surface",
        patterns: CREDENTIAL_PATTERNS,
        semantic_terms: CREDENTIAL_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "live_behavior_test",
        label: "integration/e2e/spec test surface",
        patterns: LIVE_TEST_PATTERNS,
        semantic_terms: LIVE_TEST_SEMANTIC_TERMS,
    },
];

#[derive(Debug)]
pub(super) struct SurfaceSemanticHit {
    pub(super) path: String,
    pub(super) terms: Vec<&'static str>,
}

pub(super) fn graph_store_observability(repo: &Path) -> serde_json::Value {
    let store_path = GraphStore::final_path(repo);
    let fragments_path = repo.join(".aethyme").join("graph");
    let store_modified = modified_unix_secs(&store_path);
    let newest_fragment = newest_fragment_modified_unix_secs(&fragments_path);
    let stale = match (store_modified, newest_fragment) {
        (Some(store), Some(fragment)) => Some(fragment > store),
        _ => None,
    };
    let status = match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None if store_path.is_file() => "unknown",
        None => "missing",
    };

    let graph_store = serde_json::json!({
        "backend": "redb",
        "status": status,
        "exists": store_path.is_file(),
        "fragments_exist": fragments_path.is_dir(),
        "stale": stale,
        "store_modified_unix": store_modified,
        "newest_fragment_modified_unix": newest_fragment,
    });
    let surface_flow_graph = surface_flow_coverage(repo, &fragments_path);
    let completeness = surface_flow_graph
        .get("coverage")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let indexed_languages = surface_flow_graph
        .get("indexed_languages")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let indexed_frameworks = surface_flow_graph
        .get("indexed_frameworks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let missing_expected_surfaces = surface_flow_graph
        .get("missing_expected_surfaces")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    serde_json::json!({
        "graph_store": graph_store,
        "graph_freshness": {
            "backend": "redb",
            "status": status,
            "fresh": status == "fresh",
            "exists": store_path.is_file(),
            "fragments_exist": fragments_path.is_dir(),
            "stale": stale,
            "store_modified_unix": store_modified,
            "newest_fragment_modified_unix": newest_fragment,
            "source_of_truth": "graph_fragments",
            "derived_query_artifact": "redb_graph_store",
        },
        "surface_flow_graph": surface_flow_graph,
        "graph_completeness_by_surface_type": completeness,
        "indexed_languages": indexed_languages,
        "indexed_frameworks": indexed_frameworks,
        "missing_expected_surfaces": missing_expected_surfaces,
    })
}

pub(super) fn surface_flow_coverage(repo: &Path, fragments_path: &Path) -> serde_json::Value {
    let source_paths = collect_surface_flow_paths(repo, false);
    let indexed_paths = collect_surface_flow_paths(fragments_path, true);
    let semantic_hits = collect_surface_flow_semantic_hits(fragments_path, &indexed_paths);
    let indexed_languages = indexed_languages_from_graph_paths(&indexed_paths);
    let indexed_frameworks = indexed_frameworks_from_graph_paths(&indexed_paths, &semantic_hits);
    let source_truncated = source_paths.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS;
    let indexed_truncated = indexed_paths.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS;
    let mut coverage = serde_json::Map::new();
    let mut missing_expected_surfaces = Vec::new();
    let mut covered_count = 0usize;
    let mut source_present_count = 0usize;

    for spec in SURFACE_COVERAGE_SPECS {
        let source_matches = matching_paths(&source_paths, spec.patterns);
        let path_indexed_matches = matching_paths(&indexed_paths, spec.patterns);
        let semantic_matches = semantic_path_matches(&semantic_hits, spec.semantic_terms);
        let source_present = !source_matches.is_empty();
        let path_indexed = !path_indexed_matches.is_empty();
        let semantic_required = !spec.semantic_terms.is_empty();
        let semantic_indexed = !semantic_matches.is_empty();
        let indexed = if semantic_required {
            semantic_indexed
        } else {
            path_indexed
        };
        let indexed_matches = if semantic_required {
            &semantic_matches
        } else {
            &path_indexed_matches
        };
        let unindexed_source_matches = if source_present && semantic_required && !semantic_indexed {
            source_matches.clone()
        } else {
            unindexed_source_paths(&source_matches, indexed_matches)
        };
        let source_hints = capped_path_hints(&source_matches);
        let indexed_hints = capped_path_hints(indexed_matches);
        let path_indexed_hints = capped_path_hints(&path_indexed_matches);
        let semantic_hints = capped_path_hints(&semantic_matches);
        let unindexed_source_hints = capped_path_hints(&unindexed_source_matches);
        let status = match (source_present, indexed) {
            (true, true) if unindexed_source_hints.is_empty() => {
                covered_count += 1;
                "covered"
            }
            (true, true) => {
                missing_expected_surfaces.push(serde_json::json!({
                    "surface_type": spec.key,
                    "label": spec.label,
                    "reason": if semantic_required {
                        "some source paths suggest this Surface/Flow family and some semantic graph evidence exists, but other source paths have no matching semantic Surface/Flow node/edge evidence"
                    } else {
                        "some source paths suggest this family, but matching graph fragments/index shards were not found for those source paths"
                    },
                    "source_path_hints": unindexed_source_hints.clone(),
                }));
                "partially_indexed"
            }
            (true, false) => {
                missing_expected_surfaces.push(serde_json::json!({
                    "surface_type": spec.key,
                    "label": spec.label,
                    "reason": if semantic_required && path_indexed {
                        "source paths and path fragments exist for this Surface/Flow family, but explicit semantic Surface/Flow node/edge evidence is missing"
                    } else if semantic_required {
                        "source paths suggest this Surface/Flow family, but graph fragments/index shards do not contain explicit semantic Surface/Flow node/edge evidence"
                    } else {
                        "source paths suggest this family, but graph fragments/index shards do not contain matching paths"
                    },
                    "source_path_hints": source_hints.clone(),
                }));
                "source_present_not_indexed"
            }
            (false, true) => "indexed_without_source_hint",
            (false, false) => "not_detected",
        };
        if source_present {
            source_present_count += 1;
        }
        coverage.insert(
            spec.key.to_string(),
            serde_json::json!({
                "label": spec.label,
                "source_present": source_present,
                "indexed": indexed,
                "path_indexed": path_indexed,
                "semantic_required": semantic_required,
                "semantic_indexed": semantic_indexed,
                "status": status,
                "source_path_hints": source_hints,
                "indexed_path_hints": indexed_hints,
                "path_indexed_hints": path_indexed_hints,
                "semantic_path_hints": semantic_hints,
                "unindexed_source_path_hints": unindexed_source_hints,
            }),
        );
    }

    let status = if !fragments_path.is_dir() {
        "unknown"
    } else if !missing_expected_surfaces.is_empty() {
        "partial"
    } else if covered_count > 0 {
        "covered"
    } else {
        "no_surface_signals"
    };

    serde_json::json!({
        "schema_version": SURFACE_FLOW_COVERAGE_SCHEMA_VERSION,
        "status": status,
        "source_of_truth": "graph_fragments",
        "derived_query_artifact": "redb_graph_store",
        "source_path_count_scanned": source_paths.len(),
        "indexed_path_count_scanned": indexed_paths.len(),
        "semantic_fragment_hit_count": semantic_hits.len(),
        "source_scan_truncated": source_truncated,
        "indexed_scan_truncated": indexed_truncated,
        "indexed_languages": indexed_languages,
        "indexed_frameworks": indexed_frameworks,
        "surface_type_count": SURFACE_COVERAGE_SPECS.len(),
        "source_present_surface_count": source_present_count,
        "covered_surface_count": covered_count,
        "coverage": coverage,
        "missing_expected_surfaces": missing_expected_surfaces,
    })
}

pub(super) fn collect_surface_flow_paths(root: &Path, include_hidden: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if out.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if include_hidden || !SURFACE_FLOW_IGNORED_DIRS.contains(&file_name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(normalized_path(relative));
            }
            if out.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS {
                break;
            }
        }
    }
    out.sort();
    out
}

pub(super) fn collect_surface_flow_semantic_hits(
    root: &Path,
    relative_paths: &[String],
) -> Vec<SurfaceSemanticHit> {
    let mut hits = Vec::new();
    if !root.is_dir() {
        return hits;
    }
    for relative in relative_paths {
        let path = root.join(relative);
        let Some(bytes) = read_file_prefix(&path, SURFACE_FLOW_MAX_FRAGMENT_BYTES) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        let terms: Vec<&'static str> = SURFACE_FLOW_SEMANTIC_TERMS
            .iter()
            .copied()
            .filter(|term| text.contains(term))
            .collect();
        if !terms.is_empty() {
            hits.push(SurfaceSemanticHit {
                path: relative.clone(),
                terms,
            });
        }
    }
    hits.sort_by(|a, b| a.path.cmp(&b.path));
    hits
}

pub(super) fn read_file_prefix(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut limited = file.by_ref().take(max_bytes);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

pub(super) fn semantic_path_matches(
    hits: &[SurfaceSemanticHit],
    semantic_terms: &[&'static str],
) -> Vec<String> {
    if semantic_terms.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<String> = hits
        .iter()
        .filter(|hit| hit.terms.iter().any(|term| semantic_terms.contains(term)))
        .map(|hit| hit.path.clone())
        .collect();
    matches.sort_by(
        |a, b| match surface_hint_priority(a).cmp(&surface_hint_priority(b)) {
            std::cmp::Ordering::Equal => a.cmp(b),
            other => other,
        },
    );
    matches
}

pub(super) fn matching_paths(paths: &[String], patterns: &[&str]) -> Vec<String> {
    let mut matches: Vec<String> = paths
        .iter()
        .filter(|path| {
            let match_text = surface_path_match_text(path);
            patterns.iter().any(|pattern| match_text.contains(pattern))
        })
        .cloned()
        .collect();
    matches.sort_by(
        |a, b| match surface_hint_priority(a).cmp(&surface_hint_priority(b)) {
            std::cmp::Ordering::Equal => a.cmp(b),
            other => other,
        },
    );
    matches
}

pub(super) fn capped_path_hints(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .take(SURFACE_FLOW_MAX_PATH_HINTS)
        .cloned()
        .collect()
}

pub(super) fn surface_path_match_text(path: &str) -> String {
    let lowered = path.to_ascii_lowercase();
    let logical = surface_logical_path(path);
    if logical != lowered {
        return format!("{lowered}\n{logical}");
    }
    lowered
}

pub(super) fn unindexed_source_paths(
    source_paths: &[String],
    indexed_paths: &[String],
) -> Vec<String> {
    let indexed_keys: std::collections::BTreeSet<String> = indexed_paths
        .iter()
        .flat_map(|path| surface_path_coverage_keys(path))
        .collect();
    source_paths
        .iter()
        .filter(|path| {
            let source_keys = surface_path_coverage_keys(path);
            !source_keys.iter().any(|key| indexed_keys.contains(key))
        })
        .cloned()
        .collect()
}

pub(super) fn surface_path_coverage_keys(path: &str) -> Vec<String> {
    let logical = surface_logical_path(path);
    let mut keys = vec![logical.clone()];
    if let Some(stem) = strip_known_source_extension(&logical) {
        keys.push(stem);
    }
    keys.sort();
    keys.dedup();
    keys
}

pub(super) fn surface_logical_path(path: &str) -> String {
    let lowered = path.to_ascii_lowercase();
    if let Some(indexed_module) = lowered.strip_prefix("_index/") {
        indexed_module.trim_end_matches(".ndjson").replace('.', "/")
    } else {
        lowered.trim_end_matches(".bin").to_string()
    }
}

pub(super) fn strip_known_source_extension(path: &str) -> Option<String> {
    const SOURCE_EXTENSIONS: &[&str] = &[
        ".cs", ".go", ".java", ".js", ".jsx", ".mjs", ".php", ".py", ".rb", ".rs", ".swift", ".ts",
        ".tsx",
    ];
    SOURCE_EXTENSIONS
        .iter()
        .find_map(|extension| path.strip_suffix(extension).map(str::to_string))
}

pub(super) fn indexed_languages_from_graph_paths(paths: &[String]) -> Vec<String> {
    let mut languages = std::collections::BTreeSet::new();
    for path in paths {
        let logical = surface_logical_path(path);
        let extension = Path::new(&logical)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        if let Some(language) = language_for_extension(extension) {
            languages.insert(language);
        }
    }
    languages.into_iter().map(str::to_string).collect()
}

pub(super) fn language_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "cs" => Some("csharp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "js" | "jsx" | "mjs" => Some("javascript"),
        "php" => Some("php"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "rs" => Some("rust"),
        "swift" => Some("swift"),
        "ts" | "tsx" => Some("typescript"),
        _ => None,
    }
}

pub(super) fn indexed_frameworks_from_graph_paths(
    paths: &[String],
    semantic_hits: &[SurfaceSemanticHit],
) -> Vec<String> {
    let mut match_text = paths
        .iter()
        .map(|path| surface_path_match_text(path))
        .collect::<Vec<_>>()
        .join("\n");
    for hit in semantic_hits {
        match_text.push('\n');
        match_text.push_str(&surface_path_match_text(&hit.path));
        match_text.push('\n');
        match_text.push_str(&hit.terms.join("\n"));
    }

    let mut frameworks = std::collections::BTreeSet::new();
    if contains_any_text(
        &match_text,
        &["manage.py", "settings.py", "urls.py", "django"],
    ) {
        frameworks.insert("django");
    }
    if contains_any_text(
        &match_text,
        &[
            "rest_framework",
            "viewset",
            "serializers.py",
            "api_views.py",
        ],
    ) {
        frameworks.insert("django-rest-framework");
    }
    if contains_any_text(&match_text, &["fastapi", "apirouter"]) {
        frameworks.insert("fastapi");
    }
    if contains_any_text(&match_text, &["flask", "blueprint"]) {
        frameworks.insert("flask");
    }
    if contains_any_text(&match_text, &["next.config", "app/api/", "pages/api/"]) {
        frameworks.insert("nextjs");
    }
    if contains_any_text(
        &match_text,
        &["functions/_middleware", "middleware.ts", "middleware.js"],
    ) {
        frameworks.insert("edge-middleware");
    }
    if contains_any_text(
        &match_text,
        &[
            "cloudflare",
            "wrangler",
            "worker.js",
            "worker.mjs",
            "worker.ts",
        ],
    ) {
        frameworks.insert("cloudflare-workers");
    }
    if match_text.contains("proxy_surface") {
        frameworks.insert("edge-proxy");
    }
    if contains_any_text(&match_text, &["vercel.json", "vercel/"]) {
        frameworks.insert("vercel");
    }
    if contains_any_text(&match_text, &["netlify.toml", "netlify/functions"]) {
        frameworks.insert("netlify");
    }
    if contains_any_text(&match_text, &["config/routes.rb", "rails"]) {
        frameworks.insert("rails");
    }
    if contains_any_text(&match_text, &["axum", "actix_web", "rocket"]) {
        frameworks.insert("rust-web");
    }
    frameworks.into_iter().map(str::to_string).collect()
}

pub(super) fn surface_hint_priority(path: &str) -> u8 {
    let logical = surface_logical_path(path);
    let hidden = logical.split('/').any(|part| part.starts_with('.'));
    let implementation_file = strip_known_source_extension(&logical).is_some();
    match (hidden, implementation_file) {
        (false, true) => 0,
        (true, true) => 1,
        (false, false) => 2,
        (true, false) => 3,
    }
}

pub(super) fn normalized_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn modified_unix_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(system_time_to_unix_secs)
}

pub(super) fn newest_fragment_modified_unix_secs(root: &Path) -> Option<u64> {
    let mut newest = modified_unix_secs(root);
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                newest = newest.max(modified_unix_secs(&path));
            }
        }
    }
    newest
}

pub(super) fn system_time_to_unix_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}
