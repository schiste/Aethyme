use crate::context_pack::{Anchor, AnchorKind};
use crate::graph::search::symbol_search_multi;
use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::file::FileRole;
use crate::model::task::{TaskInput, TaskKind};

const STOP_WORDS: &[&str] = &[
    "change",
    "changes",
    "update",
    "updates",
    "modify",
    "modifies",
    "fix",
    "fixes",
    "the",
    "this",
    "that",
    "repo",
    "repository",
    "component",
    "behavior",
    "flow",
    "find",
    "about",
    "where",
    "managed",
    "manages",
    "manage",
    // Task-instruction verbs already captured as semantic flags in scoring
    "controls",
    "owns",
    "identify",
    "name",
    // Common noise words from bug-fix task descriptions
    "failing",
    "failed",
    "test",
    "tests",
    "pass",
    "passing",
    "does",
    "not",
    "bug",
    "error",
];

pub fn resolve_anchors(map: &RepositoryMap, task: &TaskInput, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();

    match task.kind {
        TaskKind::ExplainRepo => {
            if let Some(readme) = &map.snapshot.readme_path {
                anchors.push(Anchor::new(
                    AnchorKind::File,
                    readme,
                    Some(readme),
                    "repository readme",
                ));
            }
            anchors.extend(explain_repo_doc_anchors(map, 1));
            anchors.extend(explain_repo_area_anchors(map, 2));
            anchors.extend(explain_repo_entrypoint_anchors(map, 1));
            anchors.extend(explain_repo_config_anchors(map, 1));
        }
        TaskKind::NavigateConfigOwnership => {
            let queries = candidate_queries(task);
            anchors.extend(area_anchors(map, &queries, true, 1));
            anchors.extend(navigation_config_anchors(map, task, &queries, 1));
            if anchors.len() < limit {
                for config in
                    config_anchors(map, task, &queries, limit.saturating_sub(anchors.len()))
                {
                    if !anchors.contains(&config) {
                        anchors.push(config);
                    }
                    if anchors.len() == limit {
                        break;
                    }
                }
            }
        }
        TaskKind::ChangeSymbol | TaskKind::TraceImpact => {
            // File references from task text get highest priority
            anchors.extend(file_reference_anchors(map, task, limit));

            let mut queries = candidate_queries(task);
            if queries.is_empty() {
                queries.push(task.normalized.clone());
            }
            // Single multi-token call — same rationale as the Unknown
            // arm at 2026-05-12. Per-token loops short-circuit the
            // compound-name scoring.
            let prefix = if task.kind == TaskKind::TraceImpact {
                "impact symbol via "
            } else {
                "change symbol via "
            };
            for hit in symbol_search_multi(map, &queries, limit) {
                anchors.push(Anchor::new(
                    AnchorKind::Symbol,
                    hit.id,
                    Some(hit.file),
                    format!("{}{}", prefix, hit.reason),
                ));
            }
            if anchors.len() < limit {
                for query in &queries {
                    for anchor in code_file_anchors(map, query, limit.saturating_sub(anchors.len()))
                    {
                        if !anchors.contains(&anchor) {
                            anchors.push(anchor);
                        }
                        if anchors.len() == limit {
                            break;
                        }
                    }
                    if anchors.len() == limit {
                        break;
                    }
                }
            }
            if anchors.len() < limit && wants_area_anchor(task) {
                anchors.extend(area_anchors(
                    map,
                    &queries,
                    true,
                    limit.saturating_sub(anchors.len()),
                ));
            }
        }
        _ => {
            // Unknown (residual) tasks: mirror the ChangeSymbol arm's
            // priority order — specific signals (file refs, symbols,
            // code-file matches, configs) first; broad area anchors as
            // a filler.
            //
            // Pre-2026-05-10, this arm ran `area_anchors` BEFORE
            // `symbol_search`. On narrative queries with many candidate
            // tokens (e.g., a bug description like "Viewing a diff/
            // revision on a watchlisted page marks all revisions as
            // 'seen'..."), the 5 area-name matches saturated `limit`
            // before symbol hits got a chance — even when matching
            // symbols clearly existed (`showDiffPage`, `doViewUpdates`,
            // `mapDiffPrevNext`, etc.). The MediaWiki bug-fix-1
            // recall gap traced directly to this ordering.
            //
            // Areas still appear when no specific anchor fills the
            // slot, preserving the "broad orientation" value of the
            // Unknown arm for genuinely-exploratory queries (e.g.
            // "what's in includes/?" — area-heavy is the right answer).

            // File references from task text get highest priority.
            anchors.extend(file_reference_anchors(map, task, limit));

            let mut queries = candidate_queries(task);
            if queries.is_empty() {
                queries.push(task.normalized.clone());
            }

            // Symbol search: specific identifiers matching task tokens.
            // These are the highest-signal anchors for any query whose
            // terms map to real code names.
            //
            // Single call across ALL tokens (2026-05-12): the previous
            // implementation looped per-token with `SYMBOLS_PER_TOKEN
            // = 1`, calling `symbol_search` independently for each
            // candidate token. With the new multi-signal scorer
            // (`symbol_search_multi`), per-token calls effectively
            // run the scorer in single-token mode — the compound
            // bonus (which fires when a symbol's name matches 2+
            // distinct tokens) never activates. Calling once with
            // the full token set unlocks the compound scoring:
            // `doViewUpdates` matching "viewing" + "viewed" via the
            // `view` stem scores higher than `Page::page()` matching
            // just one token.
            for hit in symbol_search_multi(map, &queries, limit) {
                anchors.push(Anchor::new(
                    AnchorKind::Symbol,
                    hit.id,
                    Some(hit.file),
                    hit.reason,
                ));
            }

            // Code-file matches: source files whose basename matches a
            // query token. Lower priority than symbols but higher than
            // broad areas — e.g. `watchlist.rs` for a query mentioning
            // "watchlist".
            if anchors.len() < limit {
                for query in &queries {
                    for anchor in code_file_anchors(map, query, limit.saturating_sub(anchors.len()))
                    {
                        if !anchors.contains(&anchor) {
                            anchors.push(anchor);
                        }
                        if anchors.len() == limit {
                            break;
                        }
                    }
                    if anchors.len() == limit {
                        break;
                    }
                }
            }

            // Config anchors (manifests, runtime configs) — kept in
            // the Unknown arm because queries like "where is X
            // configured" genuinely benefit. Lower priority than
            // symbols / code files.
            if anchors.len() < limit {
                for config in
                    config_anchors(map, task, &queries, limit.saturating_sub(anchors.len()))
                {
                    if !anchors.contains(&config) {
                        anchors.push(config);
                    }
                    if anchors.len() == limit {
                        break;
                    }
                }
            }

            // Areas LAST as fillers. Pre-fix this was the first call,
            // saturating the limit; now it only adds folder anchors
            // when budget remains. For a query like "Explain
            // includes/Page" with no symbol terms, areas will still
            // fully populate; for narrative queries, areas appear
            // alongside the specific anchors that should lead.
            if anchors.len() < limit {
                let wants_area = wants_area_anchor(task);
                anchors.extend(area_anchors(
                    map,
                    &queries,
                    wants_area,
                    limit.saturating_sub(anchors.len()),
                ));
            }
        }
    }

    let mut deduped = Vec::new();
    for anchor in anchors {
        if !deduped.contains(&anchor) {
            deduped.push(anchor);
        }
        if deduped.len() == limit {
            break;
        }
    }
    if matches!(task.kind, TaskKind::ExplainRepo) {
        deduped
    } else {
        filter_primary_area_anchors(map, deduped, limit)
    }
}

fn filter_primary_area_anchors(
    map: &RepositoryMap,
    anchors: Vec<Anchor>,
    limit: usize,
) -> Vec<Anchor> {
    let primary_areas = anchors
        .iter()
        .filter_map(|anchor| match anchor.kind {
            AnchorKind::Folder => Some(anchor.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if primary_areas.is_empty() {
        return anchors;
    }

    let mut filtered = Vec::new();
    for anchor in anchors {
        let keep = match anchor.kind {
            AnchorKind::Folder => primary_areas.contains(&anchor.id),
            AnchorKind::File | AnchorKind::Symbol => anchor
                .file
                .as_deref()
                .or_else(|| file_for_symbol(map, &anchor.id))
                .and_then(|file| file_area_name(map, file))
                .is_some_and(|area| primary_areas.contains(&area)),
        };
        if keep && !filtered.contains(&anchor) {
            filtered.push(anchor);
        }
        if filtered.len() == limit {
            break;
        }
    }
    filtered
}

/// Extract file-path-like references from task text and resolve them against the graph.
///
/// Scans for tokens containing a `.` followed by a known source extension (ts, js, py, rs, etc.).
/// Matches against file basenames in the graph.  Returns file anchors plus their parent area anchors.
fn file_reference_anchors(map: &RepositoryMap, task: &TaskInput, limit: usize) -> Vec<Anchor> {
    const EXTENSIONS: &[&str] = &[
        ".ts", ".tsx", ".js", ".jsx", ".py", ".rs", ".go", ".java", ".rb", ".gd", ".cs", ".cpp",
        ".c", ".h", ".hpp", ".swift", ".kt",
    ];

    // Extract tokens that look like file references (contain a dot + known extension).
    // Split on whitespace first to preserve path separators within tokens.
    let mut file_refs: Vec<String> = Vec::new();
    for token in task.raw.split_whitespace() {
        let lowered = token.to_ascii_lowercase();
        // Strip trailing punctuation (commas, colons, etc.)
        let cleaned = lowered.trim_end_matches(|c: char| {
            c.is_ascii_punctuation() && c != '.' && c != '/' && c != '-' && c != '_'
        });
        if EXTENSIONS.iter().any(|ext| cleaned.ends_with(ext)) {
            if !file_refs.contains(&cleaned.to_string()) {
                file_refs.push(cleaned.to_string());
            }
        }
    }

    if file_refs.is_empty() {
        return Vec::new();
    }

    let mut anchors = Vec::new();
    let mut seen_areas: Vec<String> = Vec::new();

    for file_ref in &file_refs {
        let ref_basename = file_ref.rsplit('/').next().unwrap_or(file_ref);

        for file in &map.files {
            let file_lower = file.path.to_ascii_lowercase();
            let file_basename = file_lower.rsplit('/').next().unwrap_or(&file_lower);

            // Score: exact basename match > path-contains match
            let score = if file_basename == ref_basename {
                300 // highest priority — exact filename match
            } else if file_lower.ends_with(file_ref.as_str()) {
                280 // full relative path match
            } else if file_lower.contains(ref_basename) {
                200 // basename appears somewhere in path
            } else {
                continue;
            };

            anchors.push((
                score,
                Anchor::new(
                    AnchorKind::File,
                    &file.path,
                    Some(&file.path),
                    format!("file reference from task text ({})", file_ref),
                ),
            ));

            // Also anchor on the file's parent area
            if let Some(area_id) = &file.area_id {
                if !seen_areas.contains(area_id) {
                    if let Some(area) = map.areas.iter().find(|a| &a.id == area_id) {
                        seen_areas.push(area_id.clone());
                        anchors.push((
                            score - 10, // slightly lower than the file itself
                            Anchor::new(
                                AnchorKind::Folder,
                                &area.name,
                                None::<String>,
                                format!("area containing referenced file ({})", file_ref),
                            ),
                        ));
                    }
                }
            }
        }
    }

    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn wants_area_anchor(task: &TaskInput) -> bool {
    ["area", "component", "module", "folder", "directory"]
        .iter()
        .any(|needle| task.normalized.contains(needle))
}

fn candidate_queries(task: &TaskInput) -> Vec<String> {
    let mut queries = Vec::new();
    for token in task
        .normalized
        .split(|character: char| !character.is_alphanumeric() && character != '_')
    {
        let cleaned = token.trim().to_ascii_lowercase();
        if cleaned.len() < 3 {
            continue;
        }
        if STOP_WORDS.contains(&cleaned.as_str()) {
            continue;
        }
        if !queries.contains(&cleaned) {
            queries.push(cleaned);
        }
    }
    queries
}

fn navigation_config_anchors(
    map: &RepositoryMap,
    task: &TaskInput,
    queries: &[String],
    limit: usize,
) -> Vec<Anchor> {
    let mut anchors = config_anchors(map, task, queries, limit.max(3));
    anchors.retain(|anchor| anchor.reason.contains("manifest"));
    anchors.truncate(limit);
    anchors
}

fn config_anchors(
    map: &RepositoryMap,
    task: &TaskInput,
    queries: &[String],
    limit: usize,
) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    let wants_manifest = task.normalized.contains("manifest");
    let wants_project = task.normalized.contains("project");
    let wants_entrypoint =
        task.normalized.contains("entrypoint") || task.normalized.contains("main code");
    let wants_ownership = task.normalized.contains("manage")
        || task.normalized.contains("managed")
        || task.normalized.contains("controls")
        || task.normalized.contains("owner")
        || task.normalized.contains("owns");
    let matched_area_ids = map
        .areas
        .iter()
        .filter(|area| {
            let area_lower = area.name.to_ascii_lowercase();
            queries.iter().any(|query| area_lower.contains(query))
        })
        .map(|area| area.id.clone())
        .collect::<Vec<_>>();
    for config in &map.configs {
        let path_lower = config.path.to_ascii_lowercase();
        let area_name = config
            .area_id
            .as_deref()
            .and_then(|area_id| map.areas.iter().find(|area| area.id == area_id))
            .map(|area| area.name.to_ascii_lowercase());
        let area_matches = config
            .area_id
            .as_ref()
            .is_some_and(|area_id| matched_area_ids.iter().any(|matched| matched == area_id));

        let mut score = 0;
        score += 8;
        if queries.iter().any(|query| path_lower.contains(query)) {
            score += 6;
        }
        if area_name
            .as_deref()
            .is_some_and(|name| queries.iter().any(|query| name.contains(query)))
        {
            score += 5;
        }
        if !matched_area_ids.is_empty() && !area_matches {
            score = 0;
        }
        let config_area = config.area_id.as_deref();
        let mut direct_entrypoint_edges = 0i32;
        let mut transitive_entrypoint_edges = 0i32;
        let mut cross_package_bonus = 0i32;
        for edge in map
            .edges
            .iter()
            .filter(|edge| edge.from == config.id && matches!(edge.kind, EdgeKind::EntrypointFor))
            .filter(|edge| {
                map.files.iter().any(|file| {
                    file.id == edge.to
                        && (matched_area_ids.is_empty()
                            || file.area_id.as_ref().is_some_and(|area| {
                                matched_area_ids.iter().any(|a| a == area.as_str())
                            }))
                }) || map.functions.iter().any(|function| {
                    function.id == edge.to
                        && (matched_area_ids.is_empty()
                            || function.area_id.as_ref().is_some_and(|area| {
                                matched_area_ids.iter().any(|a| a == area.as_str())
                            }))
                })
            })
        {
            if edge.confidence >= 900 {
                direct_entrypoint_edges += 1;
                let target_area = map
                    .files
                    .iter()
                    .find(|f| f.id == edge.to)
                    .and_then(|f| f.area_id.as_deref());
                if config_area.is_some() && target_area.is_some() && config_area != target_area {
                    cross_package_bonus += 8;
                }
            } else {
                transitive_entrypoint_edges += 1;
            }
        }
        let area_configures = map
            .edges
            .iter()
            .filter(|edge| edge.from == config.id && matches!(edge.kind, EdgeKind::Configures))
            .filter(|edge| {
                matched_area_ids.is_empty()
                    || matched_area_ids.iter().any(|matched| matched == &edge.to)
            })
            .count() as i32;

        if wants_manifest {
            score += if config.config_type == "manifest" {
                12
            } else {
                -4
            };
        }
        if wants_project {
            score += if config.config_type == "project" {
                8
            } else {
                0
            };
        }
        if wants_entrypoint {
            score += direct_entrypoint_edges * 20;
            score += transitive_entrypoint_edges.min(3) * 2;
            score += cross_package_bonus;
        } else if direct_entrypoint_edges > 0 || transitive_entrypoint_edges > 0 {
            score += 3;
        }
        if wants_ownership {
            score += area_configures * 4;
        } else if area_configures > 0 {
            score += 2;
        }
        if matches!(
            config.config_type.as_str(),
            "manifest" | "project" | "runtime"
        ) {
            score += 2;
        }
        if score == 0 {
            continue;
        }

        anchors.push((
            score,
            Anchor::new(
                AnchorKind::File,
                &config.path,
                Some(&config.path),
                format!("{} config anchor (score {})", config.config_type, score),
            ),
        ));
    }
    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn file_for_symbol<'a>(map: &'a RepositoryMap, symbol_id: &str) -> Option<&'a str> {
    map.functions
        .iter()
        .find(|function| function.id == symbol_id)
        .map(|function| function.file_path.as_str())
        .or_else(|| {
            map.classes
                .iter()
                .find(|class| class.id == symbol_id)
                .map(|class| class.file_path.as_str())
        })
}

fn file_area_name(map: &RepositoryMap, file_path: &str) -> Option<String> {
    map.files
        .iter()
        .find(|file| file.path == file_path)
        .and_then(|file| file.area_id.as_deref())
        .and_then(|area_id| map.areas.iter().find(|area| area.id == area_id))
        .map(|area| area.name.clone())
}

fn area_anchors(
    map: &RepositoryMap,
    queries: &[String],
    wants_area: bool,
    limit: usize,
) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for area in &map.areas {
        let area_lower = area.name.to_ascii_lowercase();
        let matching_queries = queries
            .iter()
            .filter(|query| area_lower.contains(query.as_str()))
            .count() as i32;
        let mut score = matching_queries * 8;
        if wants_area && matching_queries > 0 {
            score += 4;
        }
        // Prefer exact name matches over substring-in-path matches.
        // e.g. area "packages" exactly matching query "packages" should beat
        // area "backend/controls" matching query "controls" via substring.
        let leaf_name = area_lower.rsplit('/').next().unwrap_or(&area_lower);
        if queries.iter().any(|query| leaf_name == query.as_str()) {
            score += 10;
        }
        if score == 0 {
            continue;
        }
        anchors.push((
            score,
            Anchor::new(AnchorKind::Folder, &area.name, None::<String>, "area match"),
        ));
    }
    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn code_file_anchors(map: &RepositoryMap, query: &str, limit: usize) -> Vec<Anchor> {
    let lowered_query = query.to_ascii_lowercase();
    let mut anchors = map
        .files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Source | FileRole::Test))
        .filter_map(|file| {
            let lowered_path = file.path.to_ascii_lowercase();
            let basename = lowered_path
                .rsplit('/')
                .next()
                .unwrap_or(lowered_path.as_str());
            let score = if basename == format!("{lowered_query}.py")
                || basename == format!("{lowered_query}.rs")
                || basename == format!("{lowered_query}.ts")
                || basename == format!("{lowered_query}.js")
                || basename == format!("{lowered_query}.gd")
            {
                220
            } else if basename.starts_with(&lowered_query) {
                170
            } else if basename.contains(&lowered_query) {
                140
            } else if lowered_path.contains(&lowered_query) {
                100
            } else {
                return None;
            };
            Some((
                score,
                Anchor::new(
                    AnchorKind::File,
                    &file.path,
                    Some(&file.path),
                    format!("code file path match ({query})"),
                ),
            ))
        })
        .collect::<Vec<_>>();

    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_doc_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut docs: Vec<(i32, Anchor)> = map
        .docs
        .iter()
        .filter(|doc| !doc.path.eq_ignore_ascii_case("README.md") && doc.doc_type != "readme")
        .map(|doc| {
            let lower = doc.path.to_ascii_lowercase();
            let mut score = match doc.doc_type.as_str() {
                "architecture" => 10,
                "guide" => 6,
                _ => 4,
            };
            if lower.contains("documentation/") || lower.contains("docs/") {
                score += 2;
            }
            (
                score,
                Anchor::new(
                    AnchorKind::File,
                    &doc.path,
                    Some(&doc.path),
                    format!("{} document", doc.doc_type),
                ),
            )
        })
        .collect();
    docs.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    docs.truncate(limit);
    docs.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_entrypoint_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for function in &map.functions {
        let lower = function.file_path.to_ascii_lowercase();
        let score = if function.name == "main" {
            10
        } else if lower.ends_with("lib.rs")
            || lower.ends_with("main.rs")
            || lower.ends_with("main.py")
            || lower.ends_with("app.py")
            || lower.ends_with("cli.py")
            || lower.ends_with("index.ts")
            || lower.ends_with("main.ts")
        {
            7
        } else {
            continue;
        };
        anchors.push((
            score,
            Anchor::new(
                AnchorKind::File,
                &function.file_path,
                Some(&function.file_path),
                "likely entrypoint",
            ),
        ));
    }
    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_area_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for area in &map.areas {
        let mut score = 2;
        let lower = area.name.to_ascii_lowercase();
        if lower == "src"
            || lower == "app"
            || lower == "packages"
            || lower == "services"
            || lower == "tools"
            || lower.contains("engine")
        {
            score += 5;
        }
        let files = map
            .files
            .iter()
            .filter(|file| file.area_id.as_deref() == Some(area.id.as_str()))
            .count() as i32;
        let code = map
            .functions
            .iter()
            .filter(|function| function.area_id.as_deref() == Some(area.id.as_str()))
            .count() as i32;
        score += files.min(3) + code.min(3);
        anchors.push((
            score,
            Anchor::new(
                AnchorKind::Folder,
                &area.name,
                None::<String>,
                "top-level area",
            ),
        ));
    }
    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_config_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for config in &map.configs {
        let score = match config.config_type.as_str() {
            "manifest" | "project" => 6,
            "runtime" => 5,
            _ => 3,
        };
        anchors.push((
            score,
            Anchor::new(
                AnchorKind::File,
                &config.path,
                Some(&config.path),
                format!("{} config", config.config_type),
            ),
        ));
    }
    anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::context_pack::AnchorKind;

    use super::resolve_anchors;
    use crate::map::RepositoryMap;
    use crate::model::task::TaskInput;

    #[test]
    fn fix_task_extracts_file_reference_from_task_text() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_fileref_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("packages/auth/src/__tests__")).expect("create test dir");
        fs::create_dir_all(root.join("packages/auth/src")).expect("create src dir");
        fs::write(
            root.join("packages/auth/src/__tests__/ability-implications.test.ts"),
            "import { describe } from 'vitest'\ndescribe('test', () => {})\n",
        )
        .expect("write test file");
        fs::write(
            root.join("packages/auth/src/rbac-canonical.ts"),
            "export const PERMISSION_IMPLICATIONS = {}\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text(
            "Fix failing test: manage permission does not imply share in ability-implications.test.ts",
        );
        let anchors = resolve_anchors(&map, &task, 5);

        // Should find the referenced test file
        assert!(
            anchors
                .iter()
                .any(|a| a.id.contains("ability-implications.test.ts")),
            "expected anchor for ability-implications.test.ts, got: {:?}",
            anchors.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
        // Should anchor on the parent area (packages in this minimal repo)
        assert!(
            anchors.iter().any(|a| a.kind == AnchorKind::Folder),
            "expected at least one area anchor, got: {:?}",
            anchors.iter().map(|a| (&a.kind, &a.id)).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn change_symbol_task_extracts_useful_symbol_token() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/auth.py"),
            "def validate_token():\n    return True\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text("Update validate_token flow");
        let anchors = resolve_anchors(&map, &task, 3);

        assert!(
            anchors
                .iter()
                .any(|anchor| anchor.id.contains("validate_token"))
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explain_repo_prefers_structural_folder_anchors() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_repo_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("documentation")).expect("create docs dir");
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::write(root.join("README.md"), "# Demo Repo\n").expect("write readme");
        fs::write(
            root.join("documentation/technical-architecture.md"),
            "# Architecture\n",
        )
        .expect("write architecture doc");
        fs::write(root.join("GameEngine/src/main.rs"), "fn main() {}\n").expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text("Explain this repo");
        let anchors = resolve_anchors(&map, &task, 5);

        assert!(anchors.iter().any(|anchor| anchor.id == "README.md"));
        assert!(anchors.iter().any(|anchor| anchor.id == "documentation"));
        assert!(anchors.iter().any(|anchor| anchor.id == "GameEngine"));
        assert!(
            anchors
                .iter()
                .any(|anchor| anchor.id.ends_with("technical-architecture.md"))
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_arm_narrative_query_surfaces_symbols_not_only_folders() {
        // Regression test for the MediaWiki bug-fix-1 anchor gap.
        //
        // Pre-2026-05-10: a narrative query with many tokens that
        // happen to match top-level folder names would fill the
        // anchor limit (default 5) with folder anchors, leaving no
        // budget for symbol matches. Symbols were appended AFTER
        // areas in the Unknown arm — too late.
        //
        // Post-2026-05-10: the Unknown arm runs `symbol_search`
        // before `area_anchors`, with areas as fillers. A query
        // that mentions a symbol present in the graph MUST surface
        // at least one Symbol anchor.
        let root = std::env::temp_dir().join("aethyme_engine_anchor_narrative_test");
        let _ = fs::remove_dir_all(&root);
        // Set up a minimal repo with one symbol whose name matches a
        // query token, plus several folder names that match other
        // query tokens (to simulate the "areas saturate limit" case).
        fs::create_dir_all(root.join("Page/sub")).expect("create dir");
        fs::create_dir_all(root.join("Diff/sub")).expect("create dir");
        fs::create_dir_all(root.join("Revision/sub")).expect("create dir");
        fs::create_dir_all(root.join("Watchlist/sub")).expect("create dir");
        // Stub file in each folder so they're registered as areas.
        for name in &["Page", "Diff", "Revision", "Watchlist"] {
            fs::write(
                root.join(name).join("sub").join("Stub.py"),
                "def stub():\n    pass\n",
            )
            .expect("write stub");
        }
        // The producer symbol — name matches a query token.
        // Use `showDiffPage` because:
        //   - it doesn't contain change/update/modify/fix substrings,
        //     so the task stays classified as Unknown;
        //   - `Page` in its name also matches an area, exposing the
        //     symbols-vs-folders competition.
        fs::write(
            root.join("Page/sub/handler.py"),
            "def showDiffPage(self):\n    return 42\n",
        )
        .expect("write handler.py");

        let map = RepositoryMap::build(&root).expect("build repository map");

        // Descriptive query mimicking T419918: no fix/change/update/
        // modify verb, so classifies as Unknown. The tokens "page",
        // "diff", "revision", "watchlist" all match area names; the
        // token "showDiffPage" matches our planted symbol.
        let task = TaskInput::from_task_text(
            "Viewing a diff on a watchlisted page calls showDiffPage \
             and marks revisions as seen instead of only the one viewed",
        );
        assert_eq!(
            task.kind,
            crate::model::task::TaskKind::Unknown,
            "test setup precondition: query must classify as Unknown; \
             got {:?}",
            task.kind,
        );

        let anchors = resolve_anchors(&map, &task, 5);

        // Post-fix: at least one Symbol anchor must surface even
        // though four folder names (Page, Diff, Revision, Watchlist)
        // match query tokens and would have saturated the limit
        // pre-fix.
        let symbol_count = anchors
            .iter()
            .filter(|a| a.kind == AnchorKind::Symbol)
            .count();
        assert!(
            symbol_count >= 1,
            "expected at least 1 Symbol anchor; got {}: {:?}",
            symbol_count,
            anchors.iter().map(|a| (&a.kind, &a.id)).collect::<Vec<_>>()
        );
        // And the showDiffPage symbol specifically should be among
        // the anchors — that's the canonical recall test.
        assert!(
            anchors.iter().any(|a| a.id.contains("showDiffPage")),
            "expected an anchor naming `showDiffPage`; got: {:?}",
            anchors.iter().map(|a| &a.id).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_arm_orientation_query_still_gets_folder_anchors() {
        // Companion to the test above: a query that has NO matching
        // symbols (purely a "what's in here?" exploration) must
        // still get folder anchors as fillers. This pins the
        // "areas-as-filler" semantics — they appear when nothing
        // more specific does, preserving the orientation value of
        // the Unknown arm.
        let root = std::env::temp_dir().join("aethyme_engine_anchor_orientation_test");
        let _ = fs::remove_dir_all(&root);
        // Mimic the structure of `explain_repo_prefers_structural_folder_anchors`
        // (which we know produces area anchors): top-level dirs with
        // content. This guarantees the test environment HAS areas to
        // surface.
        fs::create_dir_all(root.join("Inventory/src")).expect("create dir");
        fs::create_dir_all(root.join("Auth/src")).expect("create dir");
        fs::write(root.join("Inventory/src/lib.py"), "def stub():\n    pass\n")
            .expect("write inventory file");
        fs::write(root.join("Auth/src/lib.py"), "def stub():\n    pass\n")
            .expect("write auth file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        // Query with no token matching any planted symbol name.
        // "look around in the Inventory area" → token "inventory"
        // matches the Inventory folder; no symbol named "inventory"
        // exists. We expect a Folder anchor for Inventory.
        let task = TaskInput::from_task_text("Look around in the Inventory area");
        assert_eq!(
            task.kind,
            crate::model::task::TaskKind::Unknown,
            "test setup precondition: query must classify as Unknown; \
             got {:?}",
            task.kind,
        );
        let anchors = resolve_anchors(&map, &task, 5);

        assert!(
            anchors.iter().any(|a| a.kind == AnchorKind::Folder),
            "orientation queries must still receive folder anchors; \
             got: {:?}",
            anchors.iter().map(|a| (&a.kind, &a.id)).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_navigation_task_prefers_config_and_area_anchors() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_manifest_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::create_dir_all(root.join("Other")).expect("create other dir");
        fs::write(root.join("GameEngine/src/main.rs"), "fn main() {}\n").expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");
        fs::write(root.join("Other/project.godot"), "[application]\n")
            .expect("write off-area config");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text(
            "Find the manifest that manages the main code entrypoint in the GameEngine area",
        );
        let anchors = resolve_anchors(&map, &task, 5);

        assert_eq!(
            task.kind,
            crate::model::task::TaskKind::NavigateConfigOwnership
        );
        assert!(
            anchors
                .iter()
                .any(|anchor| anchor.id.ends_with("Cargo.toml"))
        );
        assert!(anchors.iter().any(|anchor| anchor.id == "GameEngine"));
        assert!(
            !anchors
                .iter()
                .any(|anchor| anchor.id.contains("Other/project.godot"))
        );
        assert_eq!(
            anchors
                .iter()
                .filter(|anchor| anchor.kind == AnchorKind::File)
                .count(),
            1
        );

        let _ = fs::remove_dir_all(&root);
    }
}
