use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use crate::context_pack::{
    ActivationSummary, Anchor, AnchorKind, Budget, ContextPack, DependencyEdge, FileContent,
    ImpactItem, RepoOverview, Snippet,
};
use crate::graph::activation::{hormone_profile, spread_activation, spread_activation_redb};
use crate::graph::anchors::{resolve_anchors, resolve_anchors_redb};
use crate::graph::guidance::{build_in_scope, build_out_of_scope_activated, navigation_order};
use crate::graph::navigation::{
    anchor_files_redb, build_in_scope_redb, graph_overview_view_redb, primary_area_names_redb,
    task_navigation_order_redb,
};
use crate::graph::neighborhood::{
    dependency_frontier, dependency_frontier_redb, display_for_redb_id, impact_frontier,
    impact_frontier_redb, matching_target_ids_redb,
};
use crate::graph::overview::{
    build_repo_overview, overview_dependencies, overview_impact, repo_overview_seed,
};
use crate::graph::signals::{evaluate_graph_signals, evaluate_graph_signals_redb};
use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::risk::{RiskFlag, RiskLevel};
use crate::model::scope::ScopeBoundary;
use crate::model::task::TaskInput;
use crate::snippets::{select_snippets, select_snippets_redb};
use crate::store::redb::graph_store::{
    GraphStoreError, NeighborDirection, OverviewV2, OverviewV2Limits, ReadOnlyGraphStore,
};

pub fn build_context_pack(root: &Path, map: &RepositoryMap, task: TaskInput) -> ContextPack {
    let anchor_limit = if task.kind.is_explain_repo() { 5 } else { 3 };
    let scope_limit = if task.kind.is_explain_repo() { 8 } else { 5 };
    let anchors = resolve_anchors(map, &task, anchor_limit);
    let anchor_targets: Vec<String> = anchors.iter().map(|anchor| anchor.id.clone()).collect();
    let navigation = navigation_order(&anchors);
    let overview = if task.kind.is_explain_repo() {
        build_repo_overview(map, &repo_overview_seed(map))
    } else {
        Default::default()
    };
    let signals = evaluate_graph_signals(map);
    let (mut dependencies, mut impact) = if task.kind.is_explain_repo() {
        (
            overview_dependencies(map, &overview),
            overview_impact(map, &overview),
        )
    } else {
        let primary_areas = primary_area_names(map, &anchors);
        (
            focused_dependencies(map, &anchor_targets, &primary_areas),
            focused_impact(map, &anchor_targets, &primary_areas),
        )
    };

    dependencies.sort();
    dependencies.dedup();
    impact.sort();
    impact.dedup();

    // Spreading activation: seed from anchors, propagate with task-specific hormone profile
    let profile = hormone_profile(&task.kind);
    let activation_map = spread_activation(map, &anchors, &profile);
    let activated_set = activation_map.activated_set(0.1);

    let in_scope = build_in_scope(map, &anchors, scope_limit);
    let out_of_scope = build_out_of_scope_activated(map, &anchors, &task.kind, &activated_set);

    // Filter risk_flags to only those whose scope matches an activated node or anchor file
    let anchor_files: std::collections::HashSet<String> = anchors
        .iter()
        .filter_map(|anchor| anchor.file.clone())
        .collect();
    let filtered_risk_flags: Vec<_> = map
        .risk_flags
        .iter()
        .filter(|risk| activated_set.contains(&risk.scope) || anchor_files.contains(&risk.scope))
        .cloned()
        .collect();

    let activation_summary = Some(ActivationSummary {
        activated_node_count: activation_map.activations.len(),
        max_depth_reached: activation_map.max_depth_reached,
        top_activated: activation_map.top_activated(10),
    });

    let budget = Budget {
        max_anchors: anchor_limit,
        max_files: scope_limit,
        ..Default::default()
    };
    let snippets = select_snippets(root, map, &anchors, &budget);

    let anchor_confidence = if anchors.is_empty() {
        0.0
    } else if task.kind.is_explain_repo() && anchors.len() >= 3 {
        0.85
    } else {
        0.75
    };
    let scope_confidence = if in_scope.files.is_empty() && in_scope.areas.is_empty() {
        0.0
    } else if task.kind.is_explain_repo() && !in_scope.areas.is_empty() {
        0.8
    } else {
        0.70
    };

    let mut pack = ContextPack {
        task,
        summary: crate::context_pack::PackSummary {
            snapshot: crate::context_pack::PackSnapshot {
                languages: map.snapshot.languages.clone(),
                top_level_dirs: map.snapshot.top_level_dirs.clone(),
                readme_path: map.snapshot.readme_path.clone(),
            },
            files_count: map.snapshot.files.len(),
            functions_count: map.functions.len(),
            classes_count: map.classes.len(),
            docs_count: map.docs.len(),
            configs_count: map.configs.len(),
        },
        signals,
        overview,
        anchors,
        in_scope,
        out_of_scope,
        dependencies,
        impact,
        snippets,
        file_contents: Vec::new(),
        risk_flags: filtered_risk_flags,
        navigation_order: navigation,
        budget,
        confidence: crate::context_pack::Confidence {
            anchor_confidence,
            scope_confidence,
        },
        activation_summary,
    };
    pack.sort();
    pack
}

pub fn build_context_pack_with_content(
    root: &Path,
    map: &RepositoryMap,
    task: TaskInput,
    content_budget: usize,
) -> ContextPack {
    let mut pack = build_context_pack(root, map, task);
    pack.budget.content_budget = content_budget;
    pack.file_contents = read_file_contents(
        root,
        &pack.anchors,
        &pack.in_scope,
        &pack.snippets,
        &pack.budget,
    );
    pack
}

#[derive(Debug, Clone)]
pub struct ContextPackInputs {
    pub summary: crate::context_pack::PackSummary,
    pub signals: crate::graph::signals::GraphSignals,
    pub overview: RepoOverview,
    pub anchors: Vec<Anchor>,
    pub in_scope: ScopeBoundary,
    pub out_of_scope: ScopeBoundary,
    pub dependencies: Vec<DependencyEdge>,
    pub impact: Vec<ImpactItem>,
    pub snippets: Vec<Snippet>,
    pub risk_flags: Vec<RiskFlag>,
    pub navigation_order: Vec<String>,
    pub activation_summary: Option<ActivationSummary>,
}

impl ContextPackInputs {
    fn into_context_pack(self, task: TaskInput, budget: Budget) -> ContextPack {
        let anchor_confidence = if self.anchors.is_empty() {
            0.0
        } else if task.kind.is_explain_repo() && self.anchors.len() >= 3 {
            0.85
        } else {
            0.75
        };
        let scope_confidence = if self.in_scope.files.is_empty() && self.in_scope.areas.is_empty() {
            0.0
        } else if task.kind.is_explain_repo() && !self.in_scope.areas.is_empty() {
            0.8
        } else {
            0.70
        };

        let mut pack = ContextPack {
            task,
            summary: self.summary,
            signals: self.signals,
            overview: self.overview,
            anchors: self.anchors,
            in_scope: self.in_scope,
            out_of_scope: self.out_of_scope,
            dependencies: self.dependencies,
            impact: self.impact,
            snippets: self.snippets,
            file_contents: Vec::new(),
            risk_flags: self.risk_flags,
            navigation_order: self.navigation_order,
            budget,
            confidence: crate::context_pack::Confidence {
                anchor_confidence,
                scope_confidence,
            },
            activation_summary: self.activation_summary,
        };
        pack.sort();
        pack
    }
}

pub fn build_context_pack_redb(
    root: &Path,
    store: &ReadOnlyGraphStore,
    task: TaskInput,
) -> Result<ContextPack, GraphStoreError> {
    let anchor_limit = if task.kind.is_explain_repo() { 5 } else { 3 };
    let scope_limit = if task.kind.is_explain_repo() { 8 } else { 5 };
    let budget = Budget {
        max_anchors: anchor_limit,
        max_files: scope_limit,
        ..Default::default()
    };
    let inputs = build_context_pack_inputs_redb(root, store, &task, &budget)?;
    Ok(inputs.into_context_pack(task, budget))
}

pub fn build_context_pack_with_content_redb(
    root: &Path,
    store: &ReadOnlyGraphStore,
    task: TaskInput,
    content_budget: usize,
) -> Result<ContextPack, GraphStoreError> {
    let mut pack = build_context_pack_redb(root, store, task)?;
    pack.budget.content_budget = content_budget;
    pack.file_contents = read_file_contents(
        root,
        &pack.anchors,
        &pack.in_scope,
        &pack.snippets,
        &pack.budget,
    );
    Ok(pack)
}

pub fn build_context_pack_inputs_redb(
    root: &Path,
    store: &ReadOnlyGraphStore,
    task: &TaskInput,
    budget: &Budget,
) -> Result<ContextPackInputs, GraphStoreError> {
    let summary_overview = overview_v2_all_redb(store)?;
    let summary = pack_summary_redb(&summary_overview);
    let anchors = resolve_anchors_redb(store, task, budget.max_anchors)?;
    let navigation = task_navigation_order_redb(store, task, &anchors)?;
    let signals = evaluate_graph_signals_redb(store)?;
    let overview = if task.kind.is_explain_repo() {
        let view = graph_overview_view_redb(store)?;
        RepoOverview {
            overview_docs: view.overview_docs,
            code_areas: view.code_areas,
            reference_areas: view.reference_areas,
            subareas: view.subareas,
            entrypoints: view.entrypoints,
            key_configs: view.key_configs,
            representative_code_files: view.representative_code_files,
            representative_docs: view.representative_docs,
        }
    } else {
        Default::default()
    };

    let primary_areas = primary_area_names_redb(store, &anchors)?;
    let (mut dependencies, mut impact) = if task.kind.is_explain_repo() {
        (
            overview_dependencies_redb(store, &overview)?,
            overview_impact_redb(&overview),
        )
    } else {
        (
            focused_dependencies_redb(store, &anchors, &primary_areas)?,
            focused_impact_redb(store, &anchors, &primary_areas)?,
        )
    };
    dependencies.sort();
    dependencies.dedup();
    impact.sort();
    impact.dedup();

    let profile = hormone_profile(&task.kind);
    let activation_map = spread_activation_redb(store, &anchors, &profile)?;
    let activated_set = activation_map.activated_set(0.1);
    let in_scope = build_in_scope_redb(store, &anchors, budget.max_files)?;
    let out_of_scope =
        build_out_of_scope_activated_redb(store, &anchors, &task.kind, &activated_set)?;
    let risk_flags = filtered_risk_flags_redb(store, &anchors, &activated_set)?;
    let activation_summary = Some(ActivationSummary {
        activated_node_count: activation_map.activations.len(),
        max_depth_reached: activation_map.max_depth_reached,
        top_activated: activation_map.top_activated(10),
    });
    let snippets = select_snippets_redb(root, store, &anchors, budget)?;

    Ok(ContextPackInputs {
        summary,
        signals,
        overview,
        anchors,
        in_scope,
        out_of_scope,
        dependencies,
        impact,
        snippets,
        risk_flags,
        navigation_order: navigation,
        activation_summary,
    })
}

fn is_likely_text(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    !matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "svg"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "zip"
            | "tar"
            | "gz"
            | "bz2"
            | "xz"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "pdf"
            | "doc"
            | "docx"
            | "mp3"
            | "mp4"
            | "wav"
            | "avi"
            | "bin"
            | "dat"
            | "o"
            | "a"
            | "class"
    )
}

fn read_file_contents(
    root: &Path,
    anchors: &[Anchor],
    in_scope: &ScopeBoundary,
    snippets: &[Snippet],
    budget: &Budget,
) -> Vec<FileContent> {
    let mut seen = HashSet::new();
    let mut candidates: Vec<(String, &str)> = Vec::new();

    // Priority 1: anchor files
    for anchor in anchors {
        if let Some(file) = &anchor.file {
            if seen.insert(file.clone()) {
                candidates.push((file.clone(), "anchor_target"));
            }
        }
        if anchor.kind == AnchorKind::File && seen.insert(anchor.id.clone()) {
            candidates.push((anchor.id.clone(), "anchor_target"));
        }
    }

    // Priority 2: in-scope files
    for item in &in_scope.files {
        if seen.insert(item.value.clone()) {
            candidates.push((item.value.clone(), "in_scope"));
        }
    }

    // Priority 3: snippet files not already included
    for snippet in snippets {
        if seen.insert(snippet.file.clone()) {
            candidates.push((snippet.file.clone(), "snippet"));
        }
    }

    let mut contents = Vec::new();
    let mut cumulative_bytes: usize = 0;

    for (path, reason) in &candidates {
        if contents.len() >= budget.max_content_files {
            break;
        }
        let absolute = root.join(path);
        if !is_likely_text(&absolute) {
            continue;
        }
        let text = match fs::read_to_string(&absolute) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let total_lines = text.lines().count();
        if total_lines == 0 {
            continue;
        }

        let (truncated, end_line) = if total_lines <= budget.max_lines_per_file {
            (text, total_lines)
        } else {
            let cut: String = text
                .lines()
                .take(budget.max_lines_per_file)
                .collect::<Vec<_>>()
                .join("\n");
            (cut + "\n(truncated)", budget.max_lines_per_file)
        };

        if cumulative_bytes + truncated.len() > budget.content_budget {
            break;
        }
        cumulative_bytes += truncated.len();

        contents.push(FileContent {
            path: path.clone(),
            content: truncated,
            start_line: 1,
            end_line,
            total_lines,
            reason: reason.to_string(),
        });
    }

    contents
}

fn overview_v2_all_redb(store: &ReadOnlyGraphStore) -> Result<OverviewV2, GraphStoreError> {
    let mut overview = store.overview_v2(OverviewV2Limits {
        area_limit: usize::MAX,
        directory_limit: usize::MAX,
        entrypoint_limit: usize::MAX,
        risk_limit: usize::MAX,
        file_limit: usize::MAX,
        function_limit: usize::MAX,
        class_limit: usize::MAX,
        doc_limit: usize::MAX,
        config_limit: usize::MAX,
        surface_limit: usize::MAX,
        unresolved_limit: usize::MAX,
    })?;
    overview.areas = store.list_areas(None)?;
    Ok(overview)
}

fn pack_summary_redb(overview: &OverviewV2) -> crate::context_pack::PackSummary {
    let mut languages = overview
        .repo
        .as_ref()
        .map(|repo| repo.languages.clone())
        .unwrap_or_default();
    if languages.is_empty() {
        languages = overview
            .files
            .iter()
            .filter_map(|file| file.language.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }

    let top_level_dirs = overview
        .files
        .iter()
        .filter_map(|file| file.path.split_once('/').map(|(dir, _)| dir.to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let readme_path = overview
        .docs
        .iter()
        .find(|doc| doc.doc_type == "readme")
        .map(|doc| doc.path.clone())
        .or_else(|| {
            overview
                .files
                .iter()
                .find(|file| {
                    file.path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("README.md"))
                })
                .map(|file| file.path.clone())
        });

    crate::context_pack::PackSummary {
        snapshot: crate::context_pack::PackSnapshot {
            languages,
            top_level_dirs,
            readme_path,
        },
        files_count: overview.files.len(),
        functions_count: overview.functions.len(),
        classes_count: overview.classes.len(),
        docs_count: overview.docs.len(),
        configs_count: overview.configs.len(),
    }
}

fn overview_dependencies_redb(
    store: &ReadOnlyGraphStore,
    overview: &RepoOverview,
) -> Result<Vec<DependencyEdge>, GraphStoreError> {
    let focus = overview
        .code_areas
        .iter()
        .chain(overview.entrypoints.iter())
        .chain(overview.representative_code_files.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    for focus_item in focus {
        for source_id in matching_target_ids_redb(store, &focus_item)? {
            for edge in store.neighbors(&source_id, NeighborDirection::Outgoing, None)? {
                if !matches!(
                    edge.kind,
                    EdgeKind::Contains
                        | EdgeKind::Defines
                        | EdgeKind::EntrypointFor
                        | EdgeKind::Configures
                ) {
                    continue;
                }
                let target = display_for_redb_id(store, edge.other.as_str())?;
                if target.starts_with("doc:") || target.starts_with("repo:") {
                    continue;
                }
                edges.push(DependencyEdge {
                    from: display_for_redb_id(store, &source_id)?,
                    to: target,
                    kind: edge_kind_label(&edge.kind).to_string(),
                });
            }
        }
    }
    edges.sort();
    edges.dedup();
    edges.truncate(6);
    Ok(edges)
}

fn overview_impact_redb(overview: &RepoOverview) -> Vec<ImpactItem> {
    overview
        .entrypoints
        .iter()
        .take(2)
        .map(|entry| ImpactItem {
            symbol: entry.clone(),
            file: entry.clone(),
            reason: "entrypoint candidate".to_string(),
        })
        .collect()
}

fn focused_dependencies_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    primary_areas: &[String],
) -> Result<Vec<DependencyEdge>, GraphStoreError> {
    let mut dependencies = Vec::new();
    for anchor in anchors {
        let source_id = anchor.id.as_str();
        for edge in store.neighbors(source_id, NeighborDirection::Outgoing, None)? {
            if !matches!(
                edge.kind,
                EdgeKind::Configures
                    | EdgeKind::EntrypointFor
                    | EdgeKind::Imports
                    | EdgeKind::Calls
                    | EdgeKind::References
            ) {
                continue;
            }
            let display = display_for_redb_id(store, edge.other.as_str())?;
            if !primary_areas.is_empty()
                && !display_in_primary_area_redb(store, &display, primary_areas)?
            {
                continue;
            }
            dependencies.push(DependencyEdge {
                from: display_for_redb_id(store, source_id)?,
                to: display,
                kind: edge_kind_label(&edge.kind).to_string(),
            });
        }
        if dependencies.is_empty() {
            for dependency in dependency_frontier_redb(store, source_id)?
                .into_iter()
                .take(3)
            {
                if !primary_areas.is_empty()
                    && !display_in_primary_area_redb(store, &dependency, primary_areas)?
                {
                    continue;
                }
                dependencies.push(DependencyEdge {
                    from: display_for_redb_id(store, source_id)?,
                    to: dependency,
                    kind: "related".to_string(),
                });
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    dependencies.truncate(6);
    Ok(dependencies)
}

fn focused_impact_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    primary_areas: &[String],
) -> Result<Vec<ImpactItem>, GraphStoreError> {
    let mut impact = Vec::new();
    for anchor in anchors {
        let target_id = anchor.id.as_str();
        for edge in store.neighbors(target_id, NeighborDirection::Incoming, None)? {
            if !matches!(
                edge.kind,
                EdgeKind::Configures
                    | EdgeKind::EntrypointFor
                    | EdgeKind::Calls
                    | EdgeKind::References
            ) {
                continue;
            }
            let display = display_for_redb_id(store, edge.other.as_str())?;
            if !primary_areas.is_empty()
                && !display_in_primary_area_redb(store, &display, primary_areas)?
            {
                continue;
            }
            impact.push(ImpactItem {
                symbol: display.clone(),
                file: display,
                reason: format!("reverse {}", edge_kind_label(&edge.kind)),
            });
        }
        if impact.is_empty() {
            for item in impact_frontier_redb(store, target_id)?.into_iter().take(3) {
                if !primary_areas.is_empty()
                    && !display_in_primary_area_redb(store, &item, primary_areas)?
                {
                    continue;
                }
                impact.push(ImpactItem {
                    symbol: item.clone(),
                    file: item,
                    reason: "reverse dependency".to_string(),
                });
            }
        }
    }
    impact.sort();
    impact.dedup();
    impact.truncate(6);
    Ok(impact)
}

fn build_out_of_scope_activated_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    task_kind: &crate::model::task::TaskKind,
    activated_set: &HashSet<String>,
) -> Result<ScopeBoundary, GraphStoreError> {
    let mut boundary = ScopeBoundary::default();
    if matches!(task_kind, crate::model::task::TaskKind::ExplainRepo) {
        return Ok(boundary);
    }

    let primary_areas = primary_area_names_redb(store, anchors)?;
    if !primary_areas.is_empty() {
        let overview = overview_v2_all_redb(store)?;
        for area in overview.areas {
            if primary_areas.contains(&area.name) {
                continue;
            }
            let area_has_activation = overview
                .files
                .iter()
                .filter(|file| file.area_id.as_deref() == Some(area.id.as_str()))
                .any(|file| activated_set.contains(&file.id) || activated_set.contains(&file.path));

            if area_has_activation {
                boundary.areas.push(crate::model::scope::ScopeItem::new(
                    area.name,
                    crate::model::scope::ScopeKind::Area,
                    "partially activated — exercise caution",
                ));
            } else {
                boundary.areas.push(crate::model::scope::ScopeItem::new(
                    area.name,
                    crate::model::scope::ScopeKind::Area,
                    "outside the matched primary area",
                ));
            }
        }
    }

    let anchor_files = anchor_files_redb(store, anchors)?;
    for risk in overview_v2_all_redb(store)?.risks {
        if matches!(risk.level, RiskLevel::Low) {
            continue;
        }
        if anchor_files.iter().any(|file| risk.scope == *file) {
            continue;
        }
        if activated_set.contains(&risk.scope) {
            boundary.areas.push(crate::model::scope::ScopeItem::new(
                risk.scope,
                crate::model::scope::ScopeKind::Area,
                format!("high-risk area: {}", risk.reason),
            ));
        }
    }

    boundary.sort();
    Ok(boundary)
}

fn filtered_risk_flags_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    activated_set: &HashSet<String>,
) -> Result<Vec<RiskFlag>, GraphStoreError> {
    let anchor_files = anchor_files_redb(store, anchors)?;
    Ok(overview_v2_all_redb(store)?
        .risks
        .into_iter()
        .filter(|risk| activated_set.contains(&risk.scope) || anchor_files.contains(&risk.scope))
        .collect())
}

fn display_in_primary_area_redb(
    store: &ReadOnlyGraphStore,
    display: &str,
    primary_areas: &[String],
) -> Result<bool, GraphStoreError> {
    if primary_areas.is_empty() || primary_areas.iter().any(|area| area == display) {
        return Ok(true);
    }
    Ok(file_area_name_redb(store, display)?
        .map(|area| primary_areas.contains(&area))
        .unwrap_or(false))
}

fn file_area_name_redb(
    store: &ReadOnlyGraphStore,
    file_path: &str,
) -> Result<Option<String>, GraphStoreError> {
    let Some(area_id) = store.area_for_node(file_path)? else {
        return Ok(None);
    };
    Ok(store.node_display(&area_id)?.map(|node| node.name))
}

fn focused_dependencies(
    map: &RepositoryMap,
    anchor_targets: &[String],
    primary_areas: &[String],
) -> Vec<DependencyEdge> {
    let mut dependencies = Vec::new();
    for target in anchor_targets {
        for edge in &map.edges {
            if edge.from != *target {
                continue;
            }
            if !matches!(
                edge.kind,
                EdgeKind::Configures
                    | EdgeKind::EntrypointFor
                    | EdgeKind::Imports
                    | EdgeKind::Calls
                    | EdgeKind::References
            ) {
                continue;
            }
            let display = map.display_for(&edge.to);
            if !primary_areas.is_empty() && !display_in_primary_area(map, &display, primary_areas) {
                continue;
            }
            dependencies.push(DependencyEdge {
                from: map.display_for(target),
                to: display,
                kind: edge_kind_label(&edge.kind).to_string(),
            });
        }
        if dependencies.is_empty() {
            for dependency in dependency_frontier(map, target).into_iter().take(3) {
                if !primary_areas.is_empty()
                    && !display_in_primary_area(map, &dependency, primary_areas)
                {
                    continue;
                }
                dependencies.push(DependencyEdge {
                    from: map.display_for(target),
                    to: dependency,
                    kind: "related".to_string(),
                });
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    dependencies.truncate(6);
    dependencies
}

fn focused_impact(
    map: &RepositoryMap,
    anchor_targets: &[String],
    primary_areas: &[String],
) -> Vec<ImpactItem> {
    let mut impact = Vec::new();
    for target in anchor_targets {
        for edge in &map.edges {
            if edge.to != *target {
                continue;
            }
            if !matches!(
                edge.kind,
                EdgeKind::Configures
                    | EdgeKind::EntrypointFor
                    | EdgeKind::Calls
                    | EdgeKind::References
            ) {
                continue;
            }
            let display = map.display_for(&edge.from);
            if !primary_areas.is_empty() && !display_in_primary_area(map, &display, primary_areas) {
                continue;
            }
            impact.push(ImpactItem {
                symbol: display.clone(),
                file: display,
                reason: format!("reverse {}", edge_kind_label(&edge.kind)),
            });
        }
        if impact.is_empty() {
            for item in impact_frontier(map, target).into_iter().take(3) {
                if !primary_areas.is_empty() && !display_in_primary_area(map, &item, primary_areas)
                {
                    continue;
                }
                impact.push(ImpactItem {
                    symbol: item.clone(),
                    file: item,
                    reason: "reverse dependency".to_string(),
                });
            }
        }
    }
    impact.sort();
    impact.dedup();
    impact.truncate(6);
    impact
}

fn primary_area_names(map: &RepositoryMap, anchors: &[Anchor]) -> Vec<String> {
    let mut areas = anchors
        .iter()
        .filter_map(|anchor| match anchor.kind {
            AnchorKind::Folder => Some(anchor.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if areas.is_empty() {
        for anchor in anchors {
            let file_path = match anchor.kind {
                AnchorKind::File => anchor.file.as_deref().unwrap_or(&anchor.id),
                AnchorKind::Symbol => anchor.file.as_deref().unwrap_or(""),
                AnchorKind::Folder => "",
            };
            if file_path.is_empty() {
                continue;
            }
            if let Some(area) = map
                .files
                .iter()
                .find(|file| file.path == file_path)
                .and_then(|file| file.area_id.as_deref())
                .and_then(|area_id| map.areas.iter().find(|area| area.id == area_id))
                .map(|area| area.name.clone())
                && !areas.contains(&area)
            {
                areas.push(area);
            }
        }
    }

    areas
}

fn display_in_primary_area(map: &RepositoryMap, display: &str, primary_areas: &[String]) -> bool {
    if primary_areas.is_empty() {
        return true;
    }
    if primary_areas.iter().any(|area| area == display) {
        return true;
    }
    map.files
        .iter()
        .find(|file| file.path == display)
        .and_then(|file| file.area_id.as_deref())
        .and_then(|area_id| map.areas.iter().find(|area| area.id == area_id))
        .map(|area| primary_areas.contains(&area.name))
        .unwrap_or(false)
}

fn edge_kind_label(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::BelongsTo => "belongs_to",
        EdgeKind::Defines => "defines",
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::Documents => "documents",
        EdgeKind::Configures => "configures",
        EdgeKind::EntrypointFor => "entrypoint_for",
        EdgeKind::Authorizes => "authorizes",
        EdgeKind::Exposes => "exposes",
        EdgeKind::ForwardsTo => "forwards_to",
        EdgeKind::InstallsMiddleware => "installs_middleware",
        EdgeKind::IssuesCredential => "issues_credential",
        EdgeKind::StoresCredential => "stores_credential",
        EdgeKind::UsesCredential => "uses_credential",
        EdgeKind::ValidatesCredential => "validates_credential",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build_context_pack;
    use crate::map::RepositoryMap;
    use crate::model::task::TaskInput;

    #[test]
    fn explain_repo_pack_includes_readme_anchor() {
        let root = std::env::temp_dir().join("aethyme_engine_pack_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(root.join("src/main.py"), "def main():\n    return 1\n")
            .expect("write entrypoint");

        let map = RepositoryMap::build(&root).expect("build map");
        let pack = build_context_pack(&root, &map, TaskInput::from_task_text("Explain this repo"));

        assert!(!pack.anchors.is_empty());
        assert!(
            pack.navigation_order
                .iter()
                .any(|value| value == "README.md")
        );
        assert!(pack.navigation_order.iter().any(|value| value == "src"));
        assert!(pack.out_of_scope.areas.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn change_symbol_pack_uses_anchor_file_for_dependency_frontier() {
        let root = std::env::temp_dir().join("aethyme_engine_change_symbol_pack_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "from auth import validate_token\n\n".to_string()
                + "def main():\n    return validate_token()\n",
        )
        .expect("write entrypoint");
        fs::write(
            root.join("src/auth.py"),
            "def validate_token():\n    return True\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build map");
        let pack = build_context_pack(
            &root,
            &map,
            TaskInput::from_task_text("Update validate_token flow"),
        );

        assert!(
            pack.anchors
                .iter()
                .any(|anchor| anchor.id.contains("validate_token"))
        );
        assert!(
            pack.in_scope
                .files
                .iter()
                .any(|item| item.value == "src/auth.py")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_navigation_pack_stays_area_bounded() {
        let root = std::env::temp_dir().join("aethyme_engine_config_nav_pack_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::create_dir_all(root.join("Other")).expect("create other dir");
        fs::write(root.join("GameEngine/src/lib.rs"), "pub fn main() {}\n")
            .expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");
        fs::write(root.join("Other/project.godot"), "[application]\n")
            .expect("write unrelated config");

        let map = RepositoryMap::build(&root).expect("build map");
        let pack = build_context_pack(
            &root,
            &map,
            TaskInput::from_task_text(
                "Find the manifest that manages the main code entrypoint in the GameEngine area",
            ),
        );

        assert!(
            pack.in_scope
                .areas
                .iter()
                .any(|item| item.value == "GameEngine")
        );
        assert!(
            pack.in_scope
                .files
                .iter()
                .all(|item| item.value.starts_with("GameEngine/"))
        );
        assert!(
            pack.dependencies
                .iter()
                .all(|edge| edge.to.starts_with("GameEngine") || edge.to == "GameEngine")
        );
        assert!(
            pack.impact
                .iter()
                .all(|item| item.file.starts_with("GameEngine") || item.file == "GameEngine")
        );

        let _ = fs::remove_dir_all(&root);
    }
}
