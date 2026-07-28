use crate::context_pack::{Anchor, AnchorKind};
use crate::graph::anchors::{resolve_anchors, resolve_anchors_redb};
use crate::graph::guidance::{build_in_scope, build_out_of_scope, navigation_order};
use crate::graph::overview::build_repo_overview;
use crate::graph::signals::{GraphSignals, evaluate_graph_signals, evaluate_graph_signals_redb};
use crate::map::RepositoryMap;
use crate::model::area::AreaNode;
use crate::model::edge::Edge;
use crate::model::edge::EdgeKind;
use crate::model::file::FileRole;
use crate::model::risk::{RiskFlag, RiskLevel};
use crate::model::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::model::task::TaskInput;
use crate::store::redb::graph_store::{
    GraphRelation as RedbGraphRelation, GraphStoreError, NeighborDirection, NodeDisplay,
    OverviewV2, OverviewV2Limits, ReadOnlyGraphStore, RedbRelationItem, StoredNode, StoredNodeKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub source: String,
    pub confidence: u16,
    pub area: Option<String>,
    pub annotations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphRelationItem {
    pub id: String,
    pub kind: String,
    pub display: String,
    pub relation: String,
    pub confidence: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRelationView {
    pub target: String,
    pub relation: String,
    pub items: Vec<GraphRelationItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAnchorsView {
    pub task: String,
    pub anchors: Vec<Anchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScopeView {
    pub task: String,
    pub navigation_order: Vec<String>,
    pub in_scope_files: Vec<String>,
    pub in_scope_symbols: Vec<String>,
    pub in_scope_areas: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskExpandView {
    pub node: String,
    pub dependencies: Vec<String>,
    pub impact: Vec<String>,
    pub docs: Vec<String>,
    pub configs: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExpandView {
    pub target: GraphNodeView,
    pub parents: Vec<GraphRelationItem>,
    pub children: Vec<GraphRelationItem>,
    pub callers: Vec<GraphRelationItem>,
    pub callees: Vec<GraphRelationItem>,
    pub docs: Vec<GraphRelationItem>,
    pub configs: Vec<GraphRelationItem>,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoOverviewView {
    pub repo: String,
    pub overview_docs: Vec<String>,
    pub code_areas: Vec<String>,
    pub reference_areas: Vec<String>,
    pub subareas: Vec<String>,
    pub entrypoints: Vec<String>,
    pub key_configs: Vec<String>,
    pub representative_code_files: Vec<String>,
    pub representative_docs: Vec<String>,
    pub signals: GraphSignals,
}

pub fn node_view(map: &RepositoryMap, target: &str) -> Option<GraphNodeView> {
    let node = resolved_target_id(map, target)
        .and_then(|id| map.graph.nodes.iter().find(|node| node.id == id))?;
    let area = area_for_node(map, &node.id);
    let annotations = map
        .graph
        .annotations
        .iter()
        .filter(|annotation| annotation.target_id == node.id)
        .map(|annotation| format!("{}: {}", annotation.kind, annotation.value))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    Some(GraphNodeView {
        id: node.id.clone(),
        kind: graph_kind(node),
        label: node.label.clone(),
        path: node.path.clone(),
        language: node.language.clone(),
        source: node.source.clone(),
        confidence: node.confidence,
        area,
        annotations,
    })
}

pub fn children_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "children", |kind, from, to, seed| {
        matches!(kind, EdgeKind::Contains | EdgeKind::Defines) && from == seed && from != to
    })
}

pub fn parents_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "parents", |kind, from, to, seed| {
        matches!(
            kind,
            EdgeKind::Contains | EdgeKind::Defines | EdgeKind::BelongsTo
        ) && to == seed
            && from != to
    })
}

pub fn callers_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "callers", |kind, _from, to, seed| {
        matches!(kind, EdgeKind::Calls) && to == seed
    })
}

pub fn callees_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "callees", |kind, from, _to, seed| {
        matches!(kind, EdgeKind::Calls) && from == seed
    })
}

pub fn docs_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "docs", |kind, from, to, seed| {
        matches!(kind, EdgeKind::Documents) && (from == seed || to == seed)
    })
}

pub fn configs_view(map: &RepositoryMap, target: &str) -> GraphRelationView {
    relation_view(map, target, "configs", |kind, from, to, seed| {
        matches!(kind, EdgeKind::Configures | EdgeKind::EntrypointFor)
            && (from == seed || to == seed)
    })
}

pub fn node_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Option<GraphNodeView>, GraphStoreError> {
    let Some(id) = resolved_redb_target_id(store, target)? else {
        return Ok(None);
    };
    let Some(node) = store.node_display(&id)? else {
        return Ok(None);
    };
    Ok(Some(redb_node_view(store, node)?))
}

pub fn children_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Children, "children")
}

pub fn parents_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Parents, "parents")
}

pub fn callers_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Callers, "callers")
}

pub fn callees_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Callees, "callees")
}

pub fn docs_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Docs, "docs")
}

pub fn configs_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    relation_view_redb(store, target, RedbGraphRelation::Configs, "configs")
}

pub fn task_anchors_view(map: &RepositoryMap, task: &TaskInput) -> TaskAnchorsView {
    let anchors = resolve_anchors(map, task, 5);
    TaskAnchorsView {
        task: task.raw.clone(),
        anchors,
    }
}

pub fn task_anchors_view_redb(
    store: &ReadOnlyGraphStore,
    task: &TaskInput,
) -> Result<TaskAnchorsView, GraphStoreError> {
    let anchors = resolve_anchors_redb(store, task, 5)?;
    Ok(TaskAnchorsView {
        task: task.raw.clone(),
        anchors,
    })
}

pub fn task_scope_view(map: &RepositoryMap, task: &TaskInput) -> TaskScopeView {
    let anchors = resolve_anchors(map, task, 5);
    // Memoized on RepositoryMap — first read computes, subsequent reads
    // are pointer-cheap. Saves repeating ~25 seconds of signals
    // evaluation across anchors + scope + next on a 12K-file repo.
    let signals = map.signals();
    let max_files = if task.kind.is_change_task() && signals.hidden_coupling.score < 35 {
        4
    } else if task.kind.is_change_task() {
        6
    } else {
        8
    };
    let mut in_scope = build_in_scope(map, &anchors, max_files);
    let out_of_scope = build_out_of_scope(map, &anchors, &task.kind);
    let risks = risks_for_anchors(map, &anchors);
    if task.kind.is_change_task() {
        extend_change_scope(map, &anchors, &mut in_scope);
        cap_change_scope(&mut in_scope, max_files);
    }

    TaskScopeView {
        task: task.raw.clone(),
        navigation_order: task_navigation_order(map, task, &anchors),
        in_scope_files: in_scope.files.into_iter().map(|item| item.value).collect(),
        in_scope_symbols: in_scope
            .symbols
            .into_iter()
            .map(|item| item.value)
            .collect(),
        in_scope_areas: in_scope.areas.into_iter().map(|item| item.value).collect(),
        out_of_scope: out_of_scope
            .areas
            .into_iter()
            .map(|item| item.value)
            .collect(),
        risks,
    }
}

pub fn task_scope_view_redb(
    store: &ReadOnlyGraphStore,
    task: &TaskInput,
) -> Result<TaskScopeView, GraphStoreError> {
    let anchors = resolve_anchors_redb(store, task, 5)?;
    let max_files = if task.kind.is_change_task() { 6 } else { 8 };
    let mut in_scope = build_in_scope_redb(store, &anchors, max_files)?;
    let out_of_scope = build_out_of_scope_redb(store, &anchors, &task.kind)?;
    let risks = risks_for_redb_anchors(store, &anchors)?;
    if task.kind.is_change_task() {
        extend_change_scope_redb(store, &anchors, &mut in_scope)?;
        cap_change_scope(&mut in_scope, max_files);
    }

    Ok(TaskScopeView {
        task: task.raw.clone(),
        navigation_order: task_navigation_order_redb(store, task, &anchors)?,
        in_scope_files: in_scope.files.into_iter().map(|item| item.value).collect(),
        in_scope_symbols: in_scope
            .symbols
            .into_iter()
            .map(|item| item.value)
            .collect(),
        in_scope_areas: in_scope.areas.into_iter().map(|item| item.value).collect(),
        out_of_scope: out_of_scope
            .areas
            .into_iter()
            .map(|item| item.value)
            .collect(),
        risks,
    })
}

pub fn task_next_view(map: &RepositoryMap, task: &TaskInput) -> GraphRelationView {
    let anchors = resolve_anchors(map, task, 5);
    let signals = map.signals();
    let items = if task.kind.is_explain_repo() {
        let overview = graph_overview_view(map);
        overview_navigation_order(&overview)
            .iter()
            .filter_map(|item| relation_item_for_display(map, item, "next"))
            .collect::<Vec<_>>()
    } else if task.kind.is_change_task() {
        let mut items = change_task_next_items(map, &anchors);
        if signals.hidden_coupling.score < 35 {
            items.truncate(2);
        }
        items
    } else {
        task_navigation_order(map, task, &anchors)
            .iter()
            .filter_map(|item| relation_item_for_display(map, item, "next"))
            .collect::<Vec<_>>()
    };
    GraphRelationView {
        target: task.raw.clone(),
        relation: "next".to_string(),
        items,
    }
}

pub fn task_next_view_redb(
    store: &ReadOnlyGraphStore,
    task: &TaskInput,
) -> Result<GraphRelationView, GraphStoreError> {
    let anchors = resolve_anchors_redb(store, task, 5)?;
    let items = if task.kind.is_explain_repo() {
        overview_navigation_items_redb(store)?
    } else if task.kind.is_change_task() {
        change_task_next_items_redb(store, &anchors)?
    } else {
        let mut items = Vec::new();
        for item in task_navigation_order_redb(store, task, &anchors)? {
            if let Some(item) = relation_item_for_task_display_redb(store, &item, "next")? {
                items.push(item);
            }
        }
        items
    };
    Ok(GraphRelationView {
        target: task.raw.clone(),
        relation: "next".to_string(),
        items,
    })
}

pub fn task_expand_view(map: &RepositoryMap, target: &str) -> TaskExpandView {
    let dependencies = relation_strings(callees_view(map, target));
    let impact = relation_strings(callers_view(map, target));
    let docs = relation_strings(docs_view(map, target));
    let configs = relation_strings(configs_view(map, target));
    let risks = risks_for_target(map, target);
    TaskExpandView {
        node: target.to_string(),
        dependencies,
        impact,
        docs,
        configs,
        risks,
    }
}

pub fn task_expand_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<TaskExpandView, GraphStoreError> {
    let dependencies = relation_strings(callees_view_redb(store, target)?);
    let impact = relation_strings(callers_view_redb(store, target)?);
    let docs = relation_strings(docs_view_redb(store, target)?);
    let configs = relation_strings(configs_view_redb(store, target)?);
    let risks = risks_for_redb_target(store, target)?;
    Ok(TaskExpandView {
        node: target.to_string(),
        dependencies,
        impact,
        docs,
        configs,
        risks,
    })
}

pub fn graph_expand_view(map: &RepositoryMap, target: &str) -> Option<GraphExpandView> {
    let target_view = node_view(map, target)?;
    Some(GraphExpandView {
        target: target_view,
        parents: limited_items(parents_view(map, target).items, 5),
        children: limited_items(children_view(map, target).items, 8),
        callers: limited_items(callers_view(map, target).items, 8),
        callees: limited_items(callees_view(map, target).items, 8),
        docs: limited_items(docs_view(map, target).items, 5),
        configs: limited_items(configs_view(map, target).items, 5),
        risks: risks_for_target(map, target),
    })
}

pub fn graph_expand_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Option<GraphExpandView>, GraphStoreError> {
    let Some(target_view) = node_view_redb(store, target)? else {
        return Ok(None);
    };

    Ok(Some(GraphExpandView {
        target: target_view,
        parents: limited_items(parents_view_redb(store, target)?.items, 5),
        children: limited_items(children_view_redb(store, target)?.items, 8),
        callers: limited_items(callers_view_redb(store, target)?.items, 8),
        callees: limited_items(callees_view_redb(store, target)?.items, 8),
        docs: limited_items(docs_view_redb(store, target)?.items, 5),
        configs: limited_items(configs_view_redb(store, target)?.items, 5),
        risks: risks_for_redb_target(store, target)?,
    }))
}

pub fn graph_overview_view(map: &RepositoryMap) -> RepoOverviewView {
    let overview = build_repo_overview(map, &repo_navigation_seed(map));
    RepoOverviewView {
        repo: map.snapshot.repo_name(),
        overview_docs: overview.overview_docs,
        code_areas: overview.code_areas,
        reference_areas: overview.reference_areas,
        subareas: overview.subareas,
        entrypoints: overview.entrypoints,
        key_configs: overview.key_configs,
        representative_code_files: overview.representative_code_files,
        representative_docs: overview.representative_docs,
        signals: evaluate_graph_signals(map),
    }
}

pub fn graph_overview_view_redb(
    store: &ReadOnlyGraphStore,
) -> Result<RepoOverviewView, GraphStoreError> {
    let signals = evaluate_graph_signals_redb(store)?;
    let mut overview = store.overview_v2(overview_view_limits())?;
    overview.areas = store.list_areas(None)?;
    let edges = store.all_edges()?;
    let navigation_seed = repo_navigation_seed_redb(&overview);

    let code_area_limit = if signals.boundary_clarity.score < 55 {
        2
    } else {
        3
    };
    let reference_area_limit = if signals.parser_visibility.score < 60 {
        3
    } else {
        2
    };
    let entrypoint_limit = if signals.entrypoint_clarity.score >= 70 {
        3
    } else {
        1
    };
    let key_config_limit = if signals.config_hygiene.score >= 70 {
        3
    } else {
        2
    };

    let overview_docs = overview
        .docs
        .iter()
        .filter(|doc| matches!(doc.doc_type.as_str(), "readme" | "architecture"))
        .map(|doc| doc.path.clone())
        .take(3)
        .collect::<Vec<_>>();

    let mut area_candidates = overview
        .areas
        .iter()
        .filter(|area| !area.inferred && !area.name.starts_with('.'))
        .map(|area| {
            let score = area_score_redb(&overview, &edges, area.id.as_str(), area.name.as_str());
            let profile = area_profile_redb(&overview, &edges, area.id.as_str());
            (score, profile, area)
        })
        .filter(|(_, _, area)| {
            navigation_seed.iter().any(|item| item == &area.name)
                || overview_docs
                    .iter()
                    .any(|path| path.starts_with(&format!("{}/", area.name)))
        })
        .collect::<Vec<_>>();
    area_candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.code_bearing.cmp(&left.1.code_bearing))
            .then_with(|| left.2.name.cmp(&right.2.name))
    });
    let code_areas = select_top_redb_areas(area_candidates.clone(), code_area_limit, true);
    let reference_areas = select_top_redb_areas(area_candidates, reference_area_limit, false);

    let mut subarea_candidates = overview
        .areas
        .iter()
        .filter(|area| area.inferred)
        .map(|area| {
            (
                subarea_score_redb(&overview, area.id.as_str(), area.name.as_str()),
                area.name.clone(),
            )
        })
        .filter(|(_, name)| {
            code_areas
                .iter()
                .any(|area| name.starts_with(&format!("{area}/")) || name == area)
        })
        .collect::<Vec<_>>();
    subarea_candidates
        .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let subareas = subarea_candidates
        .into_iter()
        .take(4)
        .map(|(_, name)| name)
        .collect::<Vec<_>>();

    let entrypoints = overview_entrypoints_redb(&overview, &edges, entrypoint_limit);
    let key_configs = overview_key_configs_redb(&overview, store, &code_areas, key_config_limit)?;

    let representative_code_files = navigation_seed
        .iter()
        .filter(|item| is_code_like_path_for_overview(item))
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
    let representative_docs = navigation_seed
        .iter()
        .filter(|item| is_doc_like_path_for_overview(item))
        .take(5)
        .cloned()
        .collect::<Vec<_>>();

    Ok(RepoOverviewView {
        repo: repo_name_redb(&overview),
        overview_docs,
        code_areas,
        reference_areas,
        subareas,
        entrypoints,
        key_configs,
        representative_code_files,
        representative_docs,
        signals,
    })
}

fn relation_view<F>(
    map: &RepositoryMap,
    target: &str,
    relation: &str,
    predicate: F,
) -> GraphRelationView
where
    F: Fn(&EdgeKind, &str, &str, &str) -> bool,
{
    let seed = resolved_target_id(map, target).unwrap_or_else(|| target.to_string());
    let mut items = map
        .edges
        .iter()
        .filter(|edge| predicate(&edge.kind, &edge.from, &edge.to, &seed))
        .filter_map(|edge| {
            let related = if edge.from == seed {
                &edge.to
            } else {
                &edge.from
            };
            relation_item(map, related, edge_kind_label(&edge.kind), edge.confidence)
        })
        .collect::<Vec<_>>();
    items.sort();
    items.dedup();
    GraphRelationView {
        target: target.to_string(),
        relation: relation.to_string(),
        items,
    }
}

fn relation_view_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
    relation: RedbGraphRelation,
    relation_name: &str,
) -> Result<GraphRelationView, GraphStoreError> {
    let Some(id) = resolved_redb_target_id(store, target)? else {
        return Ok(GraphRelationView {
            target: target.to_string(),
            relation: relation_name.to_string(),
            items: Vec::new(),
        });
    };

    let view = store.relation_view(&id, relation)?;
    let mut items = view
        .items
        .into_iter()
        .map(redb_relation_item)
        .collect::<Vec<_>>();
    items.sort();
    items.dedup();
    Ok(GraphRelationView {
        target: target.to_string(),
        relation: relation_name.to_string(),
        items,
    })
}

fn redb_node_view(
    store: &ReadOnlyGraphStore,
    node: NodeDisplay,
) -> Result<GraphNodeView, GraphStoreError> {
    let area = redb_area_name(store, &node)?;
    let (source, confidence) = redb_node_source_confidence(node.kind);
    let annotations = redb_annotations(store, &node)?;
    Ok(GraphNodeView {
        id: node.id,
        kind: redb_kind_label(node.kind).to_string(),
        label: node.name,
        path: node.path,
        language: node.language,
        source: source.to_string(),
        confidence,
        area,
        annotations,
    })
}

fn redb_relation_item(item: RedbRelationItem) -> GraphRelationItem {
    let display = item.node.path.clone().unwrap_or(item.node.display);
    GraphRelationItem {
        id: item.node.id,
        kind: redb_kind_label(item.node.kind).to_string(),
        display,
        relation: item.relation,
        confidence: item.confidence,
    }
}

fn relation_item(
    map: &RepositoryMap,
    target_id: &str,
    relation: &str,
    confidence: u16,
) -> Option<GraphRelationItem> {
    let node = map.graph.nodes.iter().find(|node| node.id == target_id)?;
    Some(GraphRelationItem {
        id: node.id.clone(),
        kind: graph_kind(node),
        display: node.path.clone().unwrap_or_else(|| node.label.clone()),
        relation: relation.to_string(),
        confidence,
    })
}

fn resolved_redb_target_id(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Option<String>, GraphStoreError> {
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }

    if store.node_display(target)?.is_some() {
        return Ok(Some(target.to_string()));
    }

    if let Some(file) = store.resolve_file_path(target)? {
        return Ok(Some(file.id));
    }

    for area in store.list_areas(None)? {
        if area.id == target
            || area.name.eq_ignore_ascii_case(target)
            || area.path_prefix.eq_ignore_ascii_case(target)
        {
            return Ok(Some(area.id));
        }
    }

    for node in store.nodes_under_path(target)? {
        if node.id() == target
            || node
                .path()
                .map(|path| path.eq_ignore_ascii_case(target))
                .unwrap_or(false)
        {
            return Ok(Some(node.id().to_string()));
        }
    }

    if let Some((path, name)) = target.rsplit_once("::") {
        for symbol in store.find_symbols(name, None)? {
            if symbol.path.eq_ignore_ascii_case(path)
                || format!("{}::{}", symbol.path, symbol.name).eq_ignore_ascii_case(target)
            {
                return Ok(Some(symbol.id));
            }
        }
    }

    if let Some(symbol) = store.find_symbols(target, None)?.into_iter().next() {
        return Ok(Some(symbol.id));
    }

    Ok(None)
}

fn redb_area_name(
    store: &ReadOnlyGraphStore,
    node: &NodeDisplay,
) -> Result<Option<String>, GraphStoreError> {
    if node.kind == StoredNodeKind::Area {
        return Ok(Some(node.name.clone()));
    }

    let Some(area_id) = node.area_id.as_deref() else {
        return Ok(None);
    };
    Ok(store.node_display(area_id)?.map(|area| area.name))
}

fn redb_annotations(
    store: &ReadOnlyGraphStore,
    node: &NodeDisplay,
) -> Result<Vec<String>, GraphStoreError> {
    let mut annotations = Vec::new();

    if node.kind == StoredNodeKind::File {
        if let Some(path) = node.path.as_deref() {
            for risk in store.risk_for_node_or_path(&node.id)? {
                if risk.scope == path {
                    annotations.push(format!(
                        "risk: {}",
                        format!("{:?}", risk.area).to_ascii_lowercase()
                    ));
                }
            }
        }
    }

    match store.get_node(&node.id)? {
        Some(StoredNode::Doc(doc)) => {
            annotations.push(format!("doc_type: {}", doc.doc_type));
        }
        Some(StoredNode::Config(config)) => {
            annotations.push(format!("config_type: {}", config.config_type));
        }
        _ => {}
    }

    if !store
        .neighbors(
            &node.id,
            NeighborDirection::Outgoing,
            Some(EdgeKind::EntrypointFor),
        )?
        .is_empty()
    {
        annotations.push("navigation: entrypoint".to_string());
    }

    annotations.sort();
    annotations.dedup();
    Ok(annotations)
}

fn redb_node_source_confidence(kind: StoredNodeKind) -> (&'static str, u16) {
    match kind {
        StoredNodeKind::Repository
        | StoredNodeKind::Area
        | StoredNodeKind::Directory
        | StoredNodeKind::File => ("structure", 1000),
        StoredNodeKind::Function | StoredNodeKind::Class => ("code", 1000),
        StoredNodeKind::Doc => ("docs", 900),
        StoredNodeKind::Config => ("config", 900),
        StoredNodeKind::BehaviorTestSurface
        | StoredNodeKind::CliSurface
        | StoredNodeKind::CredentialOperation
        | StoredNodeKind::JobSurface
        | StoredNodeKind::MiddlewareInstallation
        | StoredNodeKind::ProxySurface
        | StoredNodeKind::QueueSurface
        | StoredNodeKind::RouteSurface
        | StoredNodeKind::WebhookSurface
        | StoredNodeKind::WorkerSurface => ("surface-flow", 850),
        StoredNodeKind::Unresolved => ("unresolved", 500),
    }
}

fn redb_kind_label(kind: StoredNodeKind) -> &'static str {
    match kind {
        StoredNodeKind::Repository => "repo",
        StoredNodeKind::Directory => "directory",
        StoredNodeKind::File => "file",
        StoredNodeKind::Area => "area",
        StoredNodeKind::Function => "function",
        StoredNodeKind::Class => "class",
        StoredNodeKind::Doc => "doc",
        StoredNodeKind::Config => "config",
        StoredNodeKind::BehaviorTestSurface => "behavior_test_surface",
        StoredNodeKind::CliSurface => "cli_surface",
        StoredNodeKind::CredentialOperation => "credential_operation",
        StoredNodeKind::JobSurface => "job_surface",
        StoredNodeKind::MiddlewareInstallation => "middleware_installation",
        StoredNodeKind::ProxySurface => "proxy_surface",
        StoredNodeKind::QueueSurface => "queue_surface",
        StoredNodeKind::RouteSurface => "route_surface",
        StoredNodeKind::WebhookSurface => "webhook_surface",
        StoredNodeKind::WorkerSurface => "worker_surface",
        StoredNodeKind::Unresolved => "unresolved",
    }
}

fn relation_item_for_display(
    map: &RepositoryMap,
    display: &str,
    relation: &str,
) -> Option<GraphRelationItem> {
    let target_id = resolved_target_id(map, display)?;
    relation_item(map, &target_id, relation, 1000)
}

fn relation_item_for_display_redb(
    store: &ReadOnlyGraphStore,
    display: &str,
    relation: &str,
) -> Result<Option<GraphRelationItem>, GraphStoreError> {
    let Some(target_id) = resolved_redb_target_id(store, display)? else {
        return Ok(None);
    };
    let Some(node) = store.node_display(&target_id)? else {
        return Ok(None);
    };
    Ok(Some(GraphRelationItem {
        id: node.id,
        kind: redb_kind_label(node.kind).to_string(),
        display: node.path.unwrap_or(node.display),
        relation: relation.to_string(),
        confidence: 1000,
    }))
}

fn relation_item_for_task_display_redb(
    store: &ReadOnlyGraphStore,
    display: &str,
    relation: &str,
) -> Result<Option<GraphRelationItem>, GraphStoreError> {
    let semantic = store.overview_v2(OverviewV2Limits {
        area_limit: 0,
        directory_limit: 0,
        entrypoint_limit: 0,
        risk_limit: 0,
        file_limit: 0,
        function_limit: 0,
        class_limit: 0,
        doc_limit: 500,
        config_limit: 500,
        surface_limit: 0,
        unresolved_limit: 0,
    })?;
    if let Some(config) = semantic
        .configs
        .into_iter()
        .find(|config| config.path == display)
    {
        return Ok(Some(GraphRelationItem {
            id: config.id,
            kind: "config".to_string(),
            display: config.path,
            relation: relation.to_string(),
            confidence: 1000,
        }));
    }
    if let Some(doc) = semantic.docs.into_iter().find(|doc| doc.path == display) {
        return Ok(Some(GraphRelationItem {
            id: doc.id,
            kind: "doc".to_string(),
            display: doc.path,
            relation: relation.to_string(),
            confidence: 1000,
        }));
    }
    relation_item_for_display_redb(store, display, relation)
}

fn relation_strings(view: GraphRelationView) -> Vec<String> {
    view.items.into_iter().map(|item| item.display).collect()
}

fn limited_items(mut items: Vec<GraphRelationItem>, limit: usize) -> Vec<GraphRelationItem> {
    items.truncate(limit);
    items
}

fn task_navigation_order(map: &RepositoryMap, task: &TaskInput, anchors: &[Anchor]) -> Vec<String> {
    if task.kind.is_change_task() {
        change_task_order(map, anchors)
    } else {
        navigation_order(anchors)
    }
}

pub(crate) fn task_navigation_order_redb(
    store: &ReadOnlyGraphStore,
    task: &TaskInput,
    anchors: &[Anchor],
) -> Result<Vec<String>, GraphStoreError> {
    if task.kind.is_change_task() {
        change_task_order_redb(store, anchors)
    } else {
        Ok(navigation_order(anchors))
    }
}

pub(crate) fn build_in_scope_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    max_files: usize,
) -> Result<ScopeBoundary, GraphStoreError> {
    let mut boundary = ScopeBoundary::default();
    let mut files = Vec::new();
    let primary_areas = primary_area_names_redb(store, anchors)?;
    let primary_area_set = primary_areas
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();

    for anchor in anchors {
        match anchor.kind {
            AnchorKind::File | AnchorKind::Symbol => {
                let file = match anchor.file.clone() {
                    Some(file) => Some(file),
                    None => file_for_redb_symbol(store, &anchor.id)?,
                };
                if let Some(file) = file {
                    if !primary_area_set.is_empty()
                        && !file_in_primary_areas_redb(store, &file, &primary_area_set)?
                    {
                        continue;
                    }
                    if !files.iter().any(|f| f == &file) {
                        files.push(file);
                    }
                }
            }
            AnchorKind::Folder => {
                let area_name =
                    redb_area_display_name(store, &anchor.id)?.unwrap_or_else(|| anchor.id.clone());
                push_unique_scope_area(&mut boundary, area_name, "primary top-level area");
            }
        }
    }

    for area in &primary_areas {
        push_unique_scope_area(&mut boundary, area.clone(), "primary top-level area");
    }

    for file in files.into_iter().take(max_files) {
        boundary.files.push(ScopeItem::new(
            file.clone(),
            ScopeKind::File,
            "anchor-adjacent file",
        ));
        for node in store.nodes_under_path(&file)? {
            match node {
                StoredNode::Function(function) => {
                    let in_primary_area = match function.area_id.as_deref() {
                        Some(area_id) => redb_area_name_by_id(store, area_id)?
                            .is_some_and(|area| primary_area_set.contains(&area)),
                        None => false,
                    };
                    if !primary_area_set.is_empty() && !in_primary_area {
                        continue;
                    }
                    if function.file_path.as_str() == file {
                        boundary.symbols.push(ScopeItem::new(
                            function.qualified_name.to_string(),
                            ScopeKind::Symbol,
                            "function defined in in-scope file",
                        ));
                    }
                }
                StoredNode::Class(class) => {
                    let in_primary_area = match class.area_id.as_deref() {
                        Some(area_id) => redb_area_name_by_id(store, area_id)?
                            .is_some_and(|area| primary_area_set.contains(&area)),
                        None => false,
                    };
                    if !primary_area_set.is_empty() && !in_primary_area {
                        continue;
                    }
                    if class.file_path.as_str() == file {
                        boundary.symbols.push(ScopeItem::new(
                            class.qualified_name.to_string(),
                            ScopeKind::Symbol,
                            "class defined in in-scope file",
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    boundary.sort();
    Ok(boundary)
}

fn build_out_of_scope_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    task_kind: &crate::model::task::TaskKind,
) -> Result<ScopeBoundary, GraphStoreError> {
    let mut boundary = ScopeBoundary::default();
    if matches!(task_kind, crate::model::task::TaskKind::ExplainRepo) {
        return Ok(boundary);
    }

    let primary_areas = primary_area_names_redb(store, anchors)?;
    if !primary_areas.is_empty() {
        for area in store.list_areas(None)? {
            if !primary_areas.contains(&area.name) {
                boundary.areas.push(ScopeItem::new(
                    area.name,
                    ScopeKind::Area,
                    "outside the matched primary area",
                ));
            }
        }
    }

    let anchor_files = anchor_files_redb(store, anchors)?;
    let overview = store.overview_v2(OverviewV2Limits {
        risk_limit: 100,
        ..OverviewV2Limits::default()
    })?;
    for risk in overview.risks {
        if matches!(risk.level, RiskLevel::Low) {
            continue;
        }
        if !anchor_files.iter().any(|file| risk.scope == *file) {
            boundary.areas.push(ScopeItem::new(
                risk.scope,
                ScopeKind::Area,
                format!("high-risk area: {}", risk.reason),
            ));
        }
    }

    boundary.sort();
    Ok(boundary)
}

pub(crate) fn primary_area_names_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
) -> Result<Vec<String>, GraphStoreError> {
    let folder_areas = anchors
        .iter()
        .filter_map(|anchor| match anchor.kind {
            AnchorKind::Folder => Some(anchor.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !folder_areas.is_empty() {
        let mut unique = Vec::new();
        for area in folder_areas {
            let name = redb_area_display_name(store, &area)?.unwrap_or(area);
            if !unique.contains(&name) {
                unique.push(name);
            }
        }
        return Ok(unique);
    }

    let mut areas = Vec::new();
    for anchor in anchors {
        match anchor.kind {
            AnchorKind::Folder => {}
            AnchorKind::File => {
                let file = anchor.file.as_deref().unwrap_or(&anchor.id);
                if let Some(area) = file_area_name_redb_nav(store, file)? {
                    if !areas.contains(&area) {
                        areas.push(area);
                    }
                }
            }
            AnchorKind::Symbol => {
                let file = match anchor.file.clone() {
                    Some(file) => Some(file),
                    None => file_for_redb_symbol(store, &anchor.id)?,
                };
                if let Some(file) = file {
                    if let Some(area) = file_area_name_redb_nav(store, &file)? {
                        if !areas.contains(&area) {
                            areas.push(area);
                        }
                    }
                }
            }
        }
    }
    Ok(areas)
}

fn file_in_primary_areas_redb(
    store: &ReadOnlyGraphStore,
    file_path: &str,
    primary_areas: &std::collections::BTreeSet<String>,
) -> Result<bool, GraphStoreError> {
    Ok(
        file_area_name_redb_nav(store, file_path)?
            .is_some_and(|area| primary_areas.contains(&area)),
    )
}

fn redb_area_display_name(
    store: &ReadOnlyGraphStore,
    id_or_name: &str,
) -> Result<Option<String>, GraphStoreError> {
    for area in store.list_areas(None)? {
        if area.id == id_or_name || area.name == id_or_name {
            return Ok(Some(area.name));
        }
    }
    Ok(None)
}

fn redb_area_name_by_id(
    store: &ReadOnlyGraphStore,
    area_id: &str,
) -> Result<Option<String>, GraphStoreError> {
    Ok(store.node_display(area_id)?.map(|node| node.name))
}

fn file_area_name_redb_nav(
    store: &ReadOnlyGraphStore,
    file_path: &str,
) -> Result<Option<String>, GraphStoreError> {
    let Some(area_id) = store.area_for_node(file_path)? else {
        return Ok(None);
    };
    redb_area_name_by_id(store, &area_id)
}

pub(crate) fn file_for_redb_symbol(
    store: &ReadOnlyGraphStore,
    symbol_id: &str,
) -> Result<Option<String>, GraphStoreError> {
    Ok(store.node_display(symbol_id)?.and_then(|node| {
        if matches!(node.kind, StoredNodeKind::Function | StoredNodeKind::Class) {
            node.path
        } else {
            None
        }
    }))
}

fn push_unique_scope_area(boundary: &mut ScopeBoundary, value: String, reason: &str) {
    let item = ScopeItem::new(value, ScopeKind::Area, reason);
    if !boundary.areas.contains(&item) {
        boundary.areas.push(item);
    }
}

pub(crate) fn anchor_files_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
) -> Result<std::collections::BTreeSet<String>, GraphStoreError> {
    let mut files = std::collections::BTreeSet::new();
    for anchor in anchors {
        let file = match anchor.file.clone() {
            Some(file) => Some(file),
            None => file_for_redb_symbol(store, &anchor.id)?,
        };
        if let Some(file) = file {
            files.insert(file);
        }
    }
    Ok(files)
}

fn resolved_target_id(map: &RepositoryMap, target: &str) -> Option<String> {
    map.matching_target_ids(target)
        .into_iter()
        .find(|candidate| map.graph.nodes.iter().any(|node| node.id == *candidate))
}

fn area_for_node(map: &RepositoryMap, node_id: &str) -> Option<String> {
    if let Some(file) = map.files.iter().find(|file| file.id == node_id) {
        return area_name(map, file.area_id.as_deref());
    }
    if let Some(function) = map.functions.iter().find(|function| function.id == node_id) {
        return area_name(map, function.area_id.as_deref());
    }
    if let Some(class) = map.classes.iter().find(|class| class.id == node_id) {
        return area_name(map, class.area_id.as_deref());
    }
    if let Some(doc) = map.docs.iter().find(|doc| doc.id == node_id) {
        return area_name(map, doc.area_id.as_deref());
    }
    if let Some(config) = map.configs.iter().find(|config| config.id == node_id) {
        return area_name(map, config.area_id.as_deref());
    }
    if let Some(area) = map.areas.iter().find(|area| area.id == node_id) {
        return Some(area.name.clone());
    }
    None
}

fn area_name(map: &RepositoryMap, area_id: Option<&str>) -> Option<String> {
    let area_id = area_id?;
    map.areas
        .iter()
        .find(|area| area.id == area_id)
        .map(|area| area.name.clone())
}

fn graph_kind(node: &crate::model::graph::GraphNode) -> String {
    match node.kind {
        crate::model::graph::GraphNodeKind::Repo => "repo",
        crate::model::graph::GraphNodeKind::Area => "area",
        crate::model::graph::GraphNodeKind::Directory => "directory",
        crate::model::graph::GraphNodeKind::File => "file",
        crate::model::graph::GraphNodeKind::Class => "class",
        crate::model::graph::GraphNodeKind::Function => "function",
        crate::model::graph::GraphNodeKind::Doc => "doc",
        crate::model::graph::GraphNodeKind::Config => "config",
        crate::model::graph::GraphNodeKind::BehaviorTestSurface => "behavior_test_surface",
        crate::model::graph::GraphNodeKind::CliSurface => "cli_surface",
        crate::model::graph::GraphNodeKind::CredentialOperation => "credential_operation",
        crate::model::graph::GraphNodeKind::JobSurface => "job_surface",
        crate::model::graph::GraphNodeKind::MiddlewareInstallation => "middleware_installation",
        crate::model::graph::GraphNodeKind::ProxySurface => "proxy_surface",
        crate::model::graph::GraphNodeKind::QueueSurface => "queue_surface",
        crate::model::graph::GraphNodeKind::RouteSurface => "route_surface",
        crate::model::graph::GraphNodeKind::WebhookSurface => "webhook_surface",
        crate::model::graph::GraphNodeKind::WorkerSurface => "worker_surface",
    }
    .to_string()
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
        EdgeKind::RewritesHeader => "rewrites_header",
        EdgeKind::TestedBy => "tested_by",
    }
}

fn risks_for_anchors(map: &RepositoryMap, anchors: &[Anchor]) -> Vec<String> {
    let mut risks = Vec::new();
    for anchor in anchors {
        for risk in risks_for_target(map, &anchor.id) {
            if !risks.contains(&risk) {
                risks.push(risk);
            }
        }
    }
    risks
}

fn risks_for_redb_anchors(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
) -> Result<Vec<String>, GraphStoreError> {
    let mut risks = Vec::new();
    for anchor in anchors {
        if matches!(anchor.kind, AnchorKind::Folder) {
            continue;
        }
        for risk in store.risk_for_node_or_path(&anchor.id)? {
            let risk = risk_string(&risk);
            if !risks.contains(&risk) {
                risks.push(risk);
            }
        }
    }
    risks.sort();
    risks.dedup();
    Ok(risks)
}

fn risks_for_target(map: &RepositoryMap, target: &str) -> Vec<String> {
    let ids = map.matching_target_ids(target);
    let mut risks = map
        .risk_flags
        .iter()
        .filter(|risk| ids.iter().any(|id| risk_matches_id(map, &risk.scope, id)))
        .map(risk_string)
        .collect::<Vec<_>>();
    risks.sort();
    risks.dedup();
    risks
}

fn risks_for_redb_target(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Vec<String>, GraphStoreError> {
    let Some(id) = resolved_redb_target_id(store, target)? else {
        return Ok(Vec::new());
    };
    let mut risks = store
        .risk_for_node_or_path(&id)?
        .iter()
        .map(risk_string)
        .collect::<Vec<_>>();
    risks.sort();
    risks.dedup();
    Ok(risks)
}

fn repo_navigation_seed(map: &RepositoryMap) -> Vec<String> {
    let mut seed = Vec::new();
    for doc in &map.docs {
        if matches!(doc.doc_type.as_str(), "readme" | "architecture") && !seed.contains(&doc.path) {
            seed.push(doc.path.clone());
        }
    }
    for area in map.areas.iter().filter(|area| !area.inferred) {
        if !seed.contains(&area.name) {
            seed.push(area.name.clone());
        }
    }
    for function in map
        .functions
        .iter()
        .filter(|function| function.name == "main")
    {
        let file_path_str = function.file_path.as_str();
        if !seed.iter().any(|s| s == file_path_str) {
            seed.push(function.file_path.to_string());
        }
    }
    for config in &map.configs {
        if !seed.contains(&config.path) {
            seed.push(config.path.clone());
        }
    }
    seed
}

fn overview_navigation_order(view: &RepoOverviewView) -> Vec<String> {
    let mut order = Vec::new();
    for item in view
        .overview_docs
        .iter()
        .chain(view.code_areas.iter())
        .chain(view.reference_areas.iter())
        .chain(view.subareas.iter())
        .chain(view.key_configs.iter())
        .chain(view.entrypoints.iter())
        .chain(view.representative_code_files.iter())
        .chain(view.representative_docs.iter())
    {
        if !order.contains(item) {
            order.push(item.clone());
        }
    }
    order
}

fn overview_navigation_items_redb(
    store: &ReadOnlyGraphStore,
) -> Result<Vec<GraphRelationItem>, GraphStoreError> {
    let mut overview = store.overview_v2(overview_view_limits())?;
    overview.areas = store.list_areas(None)?;
    let edges = store.all_edges()?;

    let overview_docs = overview
        .docs
        .iter()
        .filter(|doc| matches!(doc.doc_type.as_str(), "readme" | "architecture"))
        .take(3)
        .collect::<Vec<_>>();

    let navigation_seed = repo_navigation_seed_redb(&overview);
    let code_area_limit = 2;
    let reference_area_limit = 3;
    let entrypoint_limit = 1;
    let key_config_limit = 2;

    let mut area_candidates = overview
        .areas
        .iter()
        .filter(|area| !area.inferred && !area.name.starts_with('.'))
        .map(|area| {
            let score = area_score_redb(&overview, &edges, area.id.as_str(), area.name.as_str());
            let profile = area_profile_redb(&overview, &edges, area.id.as_str());
            (score, profile, area)
        })
        .filter(|(_, _, area)| {
            navigation_seed.iter().any(|item| item == &area.name)
                || overview_docs
                    .iter()
                    .any(|doc| doc.path.starts_with(&format!("{}/", area.name)))
        })
        .collect::<Vec<_>>();
    area_candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.code_bearing.cmp(&left.1.code_bearing))
            .then_with(|| left.2.name.cmp(&right.2.name))
    });

    let code_areas = select_top_redb_areas(area_candidates.clone(), code_area_limit, true);
    let reference_areas = select_top_redb_areas(area_candidates, reference_area_limit, false);

    let mut key_configs = overview
        .configs
        .iter()
        .filter(|config| {
            if code_areas.is_empty() {
                return config_is_overview_eligible_redb(
                    config.path.as_str(),
                    config.config_type.as_str(),
                );
            }
            config_is_overview_eligible_redb(config.path.as_str(), config.config_type.as_str())
                && config
                    .area_id
                    .as_deref()
                    .map(|area_id| code_areas.iter().any(|area| area_id.ends_with(area)))
                    .unwrap_or(false)
        })
        .map(|config| {
            let score = config_score_redb(
                store,
                config.id.as_str(),
                config.path.as_str(),
                config.config_type.as_str(),
            )?;
            Ok((score, config.config_type.clone(), config))
        })
        .collect::<Result<Vec<_>, GraphStoreError>>()?;
    key_configs.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.2.path.cmp(&right.2.path))
    });

    let mut selected_configs = Vec::new();
    let mut seen_config_families = std::collections::BTreeSet::<String>::new();
    let mut seen_config_paths = std::collections::BTreeSet::<String>::new();
    for (_, config_type, config) in key_configs {
        let family = config_family_key_redb(config_type.as_str(), config.path.as_str());
        if seen_config_families.contains(&family) || seen_config_paths.contains(&config.path) {
            continue;
        }
        seen_config_families.insert(family);
        seen_config_paths.insert(config.path.clone());
        selected_configs.push(config);
        if selected_configs.len() == key_config_limit {
            break;
        }
    }

    let entrypoint_paths = overview_entrypoints_redb(&overview, &edges, entrypoint_limit);
    let entrypoints = entrypoint_paths.iter().collect::<Vec<_>>();

    let representative_code_files = navigation_seed
        .iter()
        .filter(|item| is_code_like_path_for_overview(item))
        .take(5)
        .collect::<Vec<_>>();
    let representative_docs = navigation_seed
        .iter()
        .filter(|item| is_doc_like_path_for_overview(item))
        .take(5)
        .collect::<Vec<_>>();

    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for doc in overview_docs {
        push_unique_overview_item(&mut items, &mut seen, &doc.id, "doc", &doc.path);
    }
    for area_name in code_areas.iter().chain(reference_areas.iter()) {
        if let Some(area) = overview.areas.iter().find(|area| &area.name == area_name) {
            push_unique_overview_item(&mut items, &mut seen, &area.id, "area", &area.name);
        }
    }
    for config in selected_configs {
        push_unique_overview_item(&mut items, &mut seen, &config.id, "config", &config.path);
    }
    for path in entrypoints
        .into_iter()
        .map(String::as_str)
        .chain(representative_code_files.into_iter().map(String::as_str))
        .chain(representative_docs.into_iter().map(String::as_str))
    {
        if seen.contains(path) {
            continue;
        }
        if let Some(item) = overview_item_for_path(store, path)? {
            seen.insert(item.display.clone());
            items.push(item);
        }
    }
    Ok(items)
}

fn push_unique_overview_item(
    items: &mut Vec<GraphRelationItem>,
    seen: &mut std::collections::BTreeSet<String>,
    id: &str,
    kind: &str,
    display: &str,
) {
    if !seen.insert(display.to_string()) {
        return;
    }
    items.push(GraphRelationItem {
        id: id.to_string(),
        kind: kind.to_string(),
        display: display.to_string(),
        relation: "next".to_string(),
        confidence: 1000,
    });
}

fn overview_item_for_path(
    store: &ReadOnlyGraphStore,
    path: &str,
) -> Result<Option<GraphRelationItem>, GraphStoreError> {
    for doc in store
        .overview_v2(OverviewV2Limits {
            area_limit: 0,
            directory_limit: 0,
            entrypoint_limit: 0,
            risk_limit: 0,
            file_limit: 0,
            function_limit: 0,
            class_limit: 0,
            doc_limit: 500,
            config_limit: 0,
            surface_limit: 0,
            unresolved_limit: 0,
        })?
        .docs
    {
        if doc.path == path {
            return Ok(Some(GraphRelationItem {
                id: doc.id,
                kind: "doc".to_string(),
                display: doc.path,
                relation: "next".to_string(),
                confidence: 1000,
            }));
        }
    }

    relation_item_for_display_redb(store, path, "next")
}

fn repo_navigation_seed_redb(overview: &OverviewV2) -> Vec<String> {
    let mut seed = Vec::new();
    for doc in &overview.docs {
        if matches!(doc.doc_type.as_str(), "readme" | "architecture") && !seed.contains(&doc.path) {
            seed.push(doc.path.clone());
        }
    }
    for area in overview.areas.iter().filter(|area| !area.inferred) {
        if !seed.contains(&area.name) {
            seed.push(area.name.clone());
        }
    }
    for function in overview
        .functions
        .iter()
        .filter(|function| function.name == "main")
    {
        let file_path = function.file_path.to_string();
        if !seed.contains(&file_path) {
            seed.push(file_path);
        }
    }
    for config in &overview.configs {
        if !seed.contains(&config.path) {
            seed.push(config.path.clone());
        }
    }
    seed
}

fn overview_view_limits() -> OverviewV2Limits {
    OverviewV2Limits {
        area_limit: usize::MAX,
        directory_limit: 0,
        entrypoint_limit: usize::MAX,
        risk_limit: 0,
        file_limit: usize::MAX,
        function_limit: usize::MAX,
        class_limit: usize::MAX,
        doc_limit: usize::MAX,
        config_limit: usize::MAX,
        surface_limit: usize::MAX,
        unresolved_limit: usize::MAX,
    }
}

fn repo_name_redb(overview: &OverviewV2) -> String {
    overview
        .repository
        .as_ref()
        .map(|repo| repo.name.clone())
        .or_else(|| {
            overview.repo.as_ref().and_then(|meta| {
                std::path::Path::new(&meta.root_path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(ToString::to_string)
            })
        })
        .unwrap_or_else(|| "repo".to_string())
}

fn overview_entrypoints_redb(
    overview: &OverviewV2,
    edges: &[Edge],
    entrypoint_limit: usize,
) -> Vec<String> {
    let mut entrypoints = edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter_map(|edge| {
            let display = display_for_overview_redb(overview, edge.to.as_str());
            if is_code_like_path_for_overview(display.as_str()) {
                Some((edge.confidence, display))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    entrypoints.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    entrypoints
        .into_iter()
        .map(|(_, display)| display)
        .fold(Vec::new(), |mut acc, item| {
            if !acc.contains(&item) && acc.len() < entrypoint_limit {
                acc.push(item);
            }
            acc
        })
}

fn overview_key_configs_redb(
    overview: &OverviewV2,
    store: &ReadOnlyGraphStore,
    code_areas: &[String],
    key_config_limit: usize,
) -> Result<Vec<String>, GraphStoreError> {
    let mut key_configs = overview
        .configs
        .iter()
        .filter(|config| {
            if code_areas.is_empty() {
                return config_is_overview_eligible_redb(
                    config.path.as_str(),
                    config.config_type.as_str(),
                );
            }
            config_is_overview_eligible_redb(config.path.as_str(), config.config_type.as_str())
                && config
                    .area_id
                    .as_deref()
                    .map(|area_id| code_areas.iter().any(|area| area_id.ends_with(area)))
                    .unwrap_or(false)
        })
        .map(|config| {
            let score = config_score_redb(
                store,
                config.id.as_str(),
                config.path.as_str(),
                config.config_type.as_str(),
            )?;
            Ok((score, config.config_type.clone(), config.path.clone()))
        })
        .collect::<Result<Vec<_>, GraphStoreError>>()?;
    key_configs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.2.cmp(&right.2)));

    let mut key_config_paths = Vec::new();
    let mut seen_families = std::collections::BTreeSet::<String>::new();
    let mut seen_paths = std::collections::BTreeSet::<String>::new();
    for (_, config_type, path) in key_configs {
        let family = config_family_key_redb(config_type.as_str(), path.as_str());
        if seen_families.contains(&family) || seen_paths.contains(&path) {
            continue;
        }
        seen_families.insert(family);
        seen_paths.insert(path.clone());
        key_config_paths.push(path);
        if key_config_paths.len() == key_config_limit {
            break;
        }
    }
    Ok(key_config_paths)
}

fn display_for_overview_redb(overview: &OverviewV2, value: &str) -> String {
    overview
        .files
        .iter()
        .find(|file| file.id == value)
        .map(|file| file.path.clone())
        .or_else(|| {
            overview.functions.iter().find_map(|function| {
                if function.id.as_str() == value {
                    Some(format!("{}::{}", function.file_path, function.name))
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            overview.classes.iter().find_map(|class| {
                if class.id.as_str() == value {
                    Some(format!("{}::{}", class.file_path, class.name))
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            overview
                .docs
                .iter()
                .find(|doc| doc.id == value)
                .map(|doc| doc.path.clone())
        })
        .or_else(|| {
            overview
                .configs
                .iter()
                .find(|config| config.id == value)
                .map(|config| config.path.clone())
        })
        .or_else(|| {
            overview.unresolved.iter().find_map(|unresolved| {
                if unresolved.id.as_str() == value {
                    Some(format!("{}::{}", unresolved.file_path, unresolved.name))
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            overview
                .areas
                .iter()
                .find(|area| area.id == value)
                .map(|area| area.name.clone())
        })
        .unwrap_or_else(|| value.to_string())
}

fn area_id_for_overview_redb(overview: &OverviewV2, value: &str) -> Option<String> {
    overview
        .files
        .iter()
        .find(|file| file.id == value)
        .and_then(|file| file.area_id.clone())
        .or_else(|| {
            overview
                .functions
                .iter()
                .find(|function| function.id.as_str() == value)
                .and_then(|function| function.area_id.as_ref().map(ToString::to_string))
        })
        .or_else(|| {
            overview
                .classes
                .iter()
                .find(|class| class.id.as_str() == value)
                .and_then(|class| class.area_id.as_ref().map(ToString::to_string))
        })
        .or_else(|| {
            overview
                .docs
                .iter()
                .find(|doc| doc.id == value)
                .and_then(|doc| doc.area_id.clone())
        })
        .or_else(|| {
            overview
                .configs
                .iter()
                .find(|config| config.id == value)
                .and_then(|config| config.area_id.clone())
        })
        .or_else(|| {
            overview
                .unresolved
                .iter()
                .find(|unresolved| unresolved.id.as_str() == value)
                .and_then(|unresolved| unresolved.area_id.as_ref().map(ToString::to_string))
        })
        .or_else(|| {
            overview
                .areas
                .iter()
                .find(|area| area.id == value)
                .map(|area| area.id.clone())
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RedbAreaProfile {
    code_bearing: bool,
}

fn area_profile_redb(overview: &OverviewV2, edges: &[Edge], area_id: &str) -> RedbAreaProfile {
    let source_count = overview
        .files
        .iter()
        .filter(|file| file.area_id.as_deref() == Some(area_id))
        .filter(|file| matches!(file.role, FileRole::Source))
        .count();
    let function_count = overview
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count();
    let class_count = overview
        .classes
        .iter()
        .filter(|class| class.area_id.as_deref() == Some(area_id))
        .count();
    let entrypoint_count = edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter(|edge| {
            area_id_for_overview_redb(overview, edge.from.as_str()).as_deref() == Some(area_id)
                || area_id_for_overview_redb(overview, edge.to.as_str()).as_deref() == Some(area_id)
        })
        .count();

    RedbAreaProfile {
        code_bearing: source_count > 0
            || function_count > 0
            || class_count > 0
            || entrypoint_count > 0,
    }
}

fn area_score_redb(overview: &OverviewV2, edges: &[Edge], area_id: &str, area_name: &str) -> i32 {
    let files = overview
        .files
        .iter()
        .filter(|file| file.area_id.as_deref() == Some(area_id))
        .collect::<Vec<_>>();
    let source_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Source))
        .count() as i32;
    let config_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Config))
        .count() as i32;
    let doc_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Doc))
        .count() as i32;
    let asset_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Asset))
        .count() as i32;
    let test_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Test))
        .count() as i32;
    let unknown_count = files
        .iter()
        .filter(|file| {
            matches!(
                file.role,
                FileRole::Unknown | FileRole::Generated | FileRole::Cache | FileRole::Binary
            )
        })
        .count() as i32;

    let function_count = overview
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count() as i32;
    let class_count = overview
        .classes
        .iter()
        .filter(|class| class.area_id.as_deref() == Some(area_id))
        .count() as i32;
    let architecture_docs = overview
        .docs
        .iter()
        .filter(|doc| doc.area_id.as_deref() == Some(area_id))
        .filter(|doc| doc.doc_type == "architecture")
        .count() as i32;
    let readme_docs = overview
        .docs
        .iter()
        .filter(|doc| doc.area_id.as_deref() == Some(area_id))
        .filter(|doc| doc.doc_type == "readme")
        .count() as i32;
    let config_score = overview
        .configs
        .iter()
        .filter(|config| config.area_id.as_deref() == Some(area_id))
        .map(|config| config_weight_redb(config.config_type.as_str()))
        .sum::<i32>();
    let entrypoint_count = edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter(|edge| {
            area_id_for_overview_redb(overview, edge.from.as_str()).as_deref() == Some(area_id)
                || area_id_for_overview_redb(overview, edge.to.as_str()).as_deref() == Some(area_id)
        })
        .count() as i32;
    let entrypoint_score = entrypoint_count * 16;

    let code_presence_bonus =
        if source_count > 0 || function_count > 0 || class_count > 0 || config_count > 0 {
            40
        } else {
            0
        };
    let doc_score = std::cmp::min(doc_count, 4) * 2 + architecture_docs * 6 + readme_docs * 4;
    let unknown_penalty = if source_count == 0 && function_count == 0 && class_count == 0 {
        unknown_count * 2
    } else {
        unknown_count
    };
    let docs_bonus = overview
        .docs
        .iter()
        .filter(|doc| doc.path.starts_with(&format!("{area_name}/")))
        .filter(|doc| matches!(doc.doc_type.as_str(), "readme" | "architecture"))
        .count() as i32
        * 6;

    code_presence_bonus
        + source_count * 10
        + function_count * 20
        + class_count * 24
        + config_score
        + entrypoint_score
        + doc_score
        + asset_count
        + test_count * 2
        + docs_bonus
        - unknown_penalty
}

fn subarea_score_redb(overview: &OverviewV2, area_id: &str, area_name: &str) -> i32 {
    let files = overview
        .files
        .iter()
        .filter(|file| file.path.starts_with(&format!("{area_name}/")))
        .collect::<Vec<_>>();
    let source_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Source))
        .count() as i32;
    let config_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Config))
        .count() as i32;
    let doc_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Doc))
        .count() as i32;
    let unknown_count = files
        .iter()
        .filter(|file| {
            matches!(
                file.role,
                FileRole::Unknown | FileRole::Generated | FileRole::Cache | FileRole::Binary
            )
        })
        .count() as i32;

    let symbol_score = overview
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count() as i32
        * 12
        + overview
            .classes
            .iter()
            .filter(|class| class.area_id.as_deref() == Some(area_id))
            .count() as i32
            * 14;

    let config_score = overview
        .configs
        .iter()
        .filter(|config| config.path.starts_with(&format!("{area_name}/")))
        .map(|config| config_weight_redb(config.config_type.as_str()))
        .sum::<i32>();

    source_count * 8
        + config_count * 6
        + symbol_score
        + config_score
        + std::cmp::min(doc_count, 3) * 2
        - unknown_count
}

fn select_top_redb_areas(
    candidates: Vec<(i32, RedbAreaProfile, &AreaNode)>,
    limit: usize,
    code_bearing: bool,
) -> Vec<String> {
    let matching_count = candidates
        .iter()
        .filter(|(_, profile, _)| profile.code_bearing == code_bearing)
        .count();
    let prefer_matching = matching_count >= limit;

    let mut selected = Vec::new();
    if prefer_matching {
        for (_, profile, area) in &candidates {
            if profile.code_bearing == code_bearing && !selected.contains(&area.name) {
                selected.push(area.name.clone());
                if selected.len() == limit {
                    return selected;
                }
            }
        }
    }

    if !code_bearing {
        return selected;
    }

    for (_, _, area) in candidates {
        if !selected.contains(&area.name) {
            selected.push(area.name.clone());
            if selected.len() == limit {
                break;
            }
        }
    }
    selected
}

fn config_score_redb(
    store: &ReadOnlyGraphStore,
    config_id: &str,
    config_path: &str,
    config_type: &str,
) -> Result<i32, GraphStoreError> {
    let mut entrypoint_code_targets = 0;
    let mut entrypoint_area_targets = 0;
    let mut configures_code_targets = 0;
    let mut configures_area_targets = 0;

    for relation in store.neighbors(config_id, NeighborDirection::Outgoing, None)? {
        let target_kind = store
            .get_node(relation.other.as_str())?
            .map(|node| node.kind());
        let target_is_code_artifact = matches!(
            target_kind,
            Some(StoredNodeKind::File | StoredNodeKind::Function | StoredNodeKind::Class)
        );
        let target_is_area = matches!(target_kind, Some(StoredNodeKind::Area));
        match relation.kind {
            EdgeKind::EntrypointFor if target_is_code_artifact => entrypoint_code_targets += 1,
            EdgeKind::EntrypointFor if target_is_area => entrypoint_area_targets += 1,
            EdgeKind::Configures if target_is_code_artifact => configures_code_targets += 1,
            EdgeKind::Configures if target_is_area => configures_area_targets += 1,
            _ => {}
        }
    }

    let relationship_score = entrypoint_code_targets.min(2) * 28
        + entrypoint_area_targets.min(1) * 6
        + configures_code_targets.min(3) * 10
        + configures_area_targets.min(1) * 4;
    let path_bonus = if config_path.ends_with("Cargo.toml") {
        18
    } else if config_path.ends_with("package.json") {
        14
    } else if config_path.ends_with("project.godot") {
        8
    } else {
        0
    };

    Ok(relationship_score + config_weight_redb(config_type) + path_bonus)
}

fn config_weight_redb(config_type: &str) -> i32 {
    match config_type {
        "manifest" => 12,
        "project" => 10,
        "runtime" => 8,
        "toml" => 6,
        "yaml" => 5,
        "config" => 4,
        "json" => 2,
        _ => 1,
    }
}

fn config_family_key_redb(config_type: &str, path: &str) -> String {
    let basename = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    format!("{config_type}:{basename}")
}

fn config_is_overview_eligible_redb(path: &str, config_type: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    if lowered.contains("autosave") || lowered.contains("/cache/") || lowered.contains("/tmp/") {
        return false;
    }

    match config_type {
        "manifest" | "project" | "runtime" | "toml" | "yaml" | "config" => true,
        "json" => {
            let basename = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
            basename.contains("config")
                || basename.contains("settings")
                || basename.contains("package")
                || basename.contains("manifest")
                || basename.contains("project")
                || basename.contains("schema")
                || basename.contains("template")
        }
        _ => false,
    }
}

fn is_code_like_path_for_overview(path: &str) -> bool {
    path.ends_with(".py")
        || path.ends_with(".rs")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".js")
        || path.ends_with(".jsx")
        || path.ends_with(".gd")
        || path.ends_with(".go")
}

fn is_doc_like_path_for_overview(path: &str) -> bool {
    path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.contains("documentation/")
        || path.contains("/docs/")
}

fn change_task_order(map: &RepositoryMap, anchors: &[Anchor]) -> Vec<String> {
    let mut order = navigation_order(anchors);
    if let Some(primary) = anchors.first() {
        for item in change_neighbor_displays(map, primary).into_iter().take(6) {
            if !order.contains(&item) {
                order.push(item);
            }
        }
    }
    order
}

fn change_task_order_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
) -> Result<Vec<String>, GraphStoreError> {
    let mut order = navigation_order(anchors);
    if let Some(primary) = anchors.first() {
        for item in change_neighbor_displays_redb(store, primary)?
            .into_iter()
            .take(6)
        {
            if !order.contains(&item) {
                order.push(item);
            }
        }
    }
    Ok(order)
}

fn change_task_next_items(map: &RepositoryMap, anchors: &[Anchor]) -> Vec<GraphRelationItem> {
    let mut items = Vec::new();
    for anchor in anchors.iter().take(2) {
        let display = anchor.file.clone().unwrap_or_else(|| anchor.id.clone());
        if let Some(item) = relation_item_for_display(map, &display, "next") {
            items.push(item);
        }
    }
    if let Some(primary) = anchors.first() {
        let primary_target = match primary.kind {
            AnchorKind::Symbol => primary.id.as_str(),
            _ => primary.file.as_deref().unwrap_or(&primary.id),
        };
        for relation in [
            callers_view(map, primary_target),
            callees_view(map, primary_target),
            docs_view(map, primary_target),
            configs_view(map, primary_target),
        ] {
            for item in relation.items.into_iter().take(2) {
                let adjusted_item = if item.kind == "function" {
                    if let Some(function) =
                        map.functions.iter().find(|function| function.id == item.id)
                    {
                        relation_item_for_display(map, &function.file_path, "next").unwrap_or(item)
                    } else {
                        item
                    }
                } else {
                    item
                };
                if !items.iter().any(|existing| existing.id == adjusted_item.id) {
                    items.push(adjusted_item);
                }
            }
        }
    }
    items
}

fn change_task_next_items_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
) -> Result<Vec<GraphRelationItem>, GraphStoreError> {
    let mut items = Vec::new();
    for anchor in anchors.iter().take(2) {
        let display = anchor.file.clone().unwrap_or_else(|| anchor.id.clone());
        if let Some(item) = relation_item_for_display_redb(store, &display, "next")? {
            items.push(item);
        }
    }
    if let Some(primary) = anchors.first() {
        let primary_target = match primary.kind {
            AnchorKind::Symbol => primary.id.as_str(),
            _ => primary.file.as_deref().unwrap_or(&primary.id),
        };
        for relation in [
            callers_view_redb(store, primary_target)?,
            callees_view_redb(store, primary_target)?,
            docs_view_redb(store, primary_target)?,
            configs_view_redb(store, primary_target)?,
        ] {
            for item in relation.items.into_iter().take(2) {
                let adjusted_item = if item.kind == "function" {
                    relation_item_for_display_redb(store, &item.display, "next")?.unwrap_or(item)
                } else {
                    item
                };
                if !items.iter().any(|existing| existing.id == adjusted_item.id) {
                    items.push(adjusted_item);
                }
            }
        }
    }
    Ok(items)
}

fn extend_change_scope(
    map: &RepositoryMap,
    anchors: &[Anchor],
    in_scope: &mut crate::model::scope::ScopeBoundary,
) {
    let current_files = in_scope
        .files
        .iter()
        .map(|item| item.value.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut additional = Vec::new();
    for anchor in anchors.iter().take(2) {
        for display in change_neighbor_displays(map, anchor).into_iter().take(4) {
            if display.contains('/')
                && !display.ends_with(".md")
                && !display.ends_with(".mdx")
                && !current_files.contains(&display)
                && !additional.contains(&display)
            {
                additional.push(display);
            }
        }
    }
    for file in additional.into_iter().take(3) {
        in_scope.files.push(crate::model::scope::ScopeItem::new(
            file,
            crate::model::scope::ScopeKind::File,
            "caller/callee/config neighbor for change task",
        ));
    }
    in_scope.symbols = scoped_change_symbols(map, &in_scope.files);
    in_scope.sort();
}

fn extend_change_scope_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    in_scope: &mut ScopeBoundary,
) -> Result<(), GraphStoreError> {
    let current_files = in_scope
        .files
        .iter()
        .map(|item| item.value.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut additional = Vec::new();
    for anchor in anchors.iter().take(2) {
        for display in change_neighbor_displays_redb(store, anchor)?
            .into_iter()
            .take(4)
        {
            if display.contains('/')
                && !display.ends_with(".md")
                && !display.ends_with(".mdx")
                && !current_files.contains(&display)
                && !additional.contains(&display)
            {
                additional.push(display);
            }
        }
    }
    for file in additional.into_iter().take(3) {
        in_scope.files.push(ScopeItem::new(
            file,
            ScopeKind::File,
            "caller/callee/config neighbor for change task",
        ));
    }
    in_scope.symbols = scoped_change_symbols_redb(store, &in_scope.files)?;
    in_scope.sort();
    Ok(())
}

fn cap_change_scope(in_scope: &mut crate::model::scope::ScopeBoundary, max_files: usize) {
    in_scope.files.truncate(max_files);
    let allowed_files = in_scope
        .files
        .iter()
        .map(|item| item.value.clone())
        .collect::<std::collections::BTreeSet<_>>();
    in_scope.symbols.retain(|item| {
        item.value
            .split_once("::")
            .map(|value| allowed_files.contains(value.0))
            .unwrap_or(false)
    });
}

fn change_neighbor_displays(map: &RepositoryMap, anchor: &Anchor) -> Vec<String> {
    let target = match anchor.kind {
        AnchorKind::Symbol => anchor.id.as_str(),
        _ => anchor.file.as_deref().unwrap_or(&anchor.id),
    };
    let mut displays = Vec::new();
    for relation in direct_change_relations(map, target) {
        for item in relation.items {
            let display = change_display_for_relation_item(map, item);
            if !displays.contains(&display) {
                displays.push(display);
            }
        }
    }
    if anchor.kind == AnchorKind::File {
        let file_function_ids = map
            .functions
            .iter()
            .filter(|function| function.file_path == target)
            .map(|function| function.id.clone())
            .collect::<Vec<_>>();
        for function_id in file_function_ids {
            for relation in [
                callers_view(map, &function_id),
                callees_view(map, &function_id),
            ] {
                for item in relation.items {
                    let display = change_display_for_relation_item(map, item);
                    if !displays.contains(&display) {
                        displays.push(display);
                    }
                }
            }
        }
    }
    displays
}

fn change_neighbor_displays_redb(
    store: &ReadOnlyGraphStore,
    anchor: &Anchor,
) -> Result<Vec<String>, GraphStoreError> {
    let target = match anchor.kind {
        AnchorKind::Symbol => anchor.id.as_str(),
        _ => anchor.file.as_deref().unwrap_or(&anchor.id),
    };
    let mut displays = Vec::new();
    for relation in direct_change_relations_redb(store, target)? {
        for item in relation.items {
            let display = change_display_for_redb_relation_item(store, item)?;
            if !displays.contains(&display) {
                displays.push(display);
            }
        }
    }
    if anchor.kind == AnchorKind::File {
        let file = anchor.file.as_deref().unwrap_or(&anchor.id);
        let file_function_ids = store
            .functions_under_path(file)?
            .into_iter()
            .filter(|function| function.file_path.as_str() == file)
            .map(|function| function.id.to_string())
            .collect::<Vec<_>>();
        for function_id in file_function_ids {
            for relation in [
                callers_view_redb(store, &function_id)?,
                callees_view_redb(store, &function_id)?,
            ] {
                for item in relation.items {
                    let display = change_display_for_redb_relation_item(store, item)?;
                    if !displays.contains(&display) {
                        displays.push(display);
                    }
                }
            }
        }
    }
    Ok(displays)
}

fn direct_change_relations(map: &RepositoryMap, target: &str) -> [GraphRelationView; 4] {
    [
        callers_view(map, target),
        callees_view(map, target),
        docs_view(map, target),
        configs_view(map, target),
    ]
}

fn direct_change_relations_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<[GraphRelationView; 4], GraphStoreError> {
    Ok([
        callers_view_redb(store, target)?,
        callees_view_redb(store, target)?,
        docs_view_redb(store, target)?,
        configs_view_redb(store, target)?,
    ])
}

fn change_display_for_relation_item(map: &RepositoryMap, item: GraphRelationItem) -> String {
    if item.kind == "function" {
        if let Some(function) = map.functions.iter().find(|function| function.id == item.id) {
            return function.file_path.to_string();
        }
    }
    item.display
}

fn change_display_for_redb_relation_item(
    store: &ReadOnlyGraphStore,
    item: GraphRelationItem,
) -> Result<String, GraphStoreError> {
    if item.kind == "function" {
        if let Some(node) = store.node_display(&item.id)? {
            if let Some(path) = node.path {
                return Ok(path);
            }
        }
    }
    Ok(item.display)
}

fn scoped_change_symbols(
    map: &RepositoryMap,
    files: &[crate::model::scope::ScopeItem],
) -> Vec<crate::model::scope::ScopeItem> {
    let mut symbols = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for file in files.iter().take(2) {
        let mut file_symbols = map
            .functions
            .iter()
            .filter(|function| function.file_path == file.value)
            .map(|function| function.qualified_name.to_string())
            .collect::<Vec<_>>();
        file_symbols.sort();
        for qualified_name in file_symbols.into_iter().take(8) {
            if seen.insert(qualified_name.clone()) {
                symbols.push(crate::model::scope::ScopeItem::new(
                    qualified_name,
                    crate::model::scope::ScopeKind::Symbol,
                    "symbol defined in in-scope change file",
                ));
            }
        }
    }
    symbols
}

fn scoped_change_symbols_redb(
    store: &ReadOnlyGraphStore,
    files: &[ScopeItem],
) -> Result<Vec<ScopeItem>, GraphStoreError> {
    let mut symbols = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for file in files.iter().take(2) {
        let mut file_symbols = store
            .nodes_under_path(&file.value)?
            .into_iter()
            .filter_map(|node| match node {
                StoredNode::Function(function) if function.file_path.as_str() == file.value => {
                    Some(function.qualified_name.to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        file_symbols.sort();
        for qualified_name in file_symbols.into_iter().take(8) {
            if seen.insert(qualified_name.clone()) {
                symbols.push(ScopeItem::new(
                    qualified_name,
                    ScopeKind::Symbol,
                    "symbol defined in in-scope change file",
                ));
            }
        }
    }
    Ok(symbols)
}

fn risk_matches_id(map: &RepositoryMap, scope: &str, target_id: &str) -> bool {
    if scope == target_id {
        return true;
    }
    if let Some(file) = map.files.iter().find(|file| file.id == target_id) {
        return file.path == scope;
    }
    if let Some(function) = map
        .functions
        .iter()
        .find(|function| function.id == target_id)
    {
        return function.file_path == scope;
    }
    if let Some(class) = map.classes.iter().find(|class| class.id == target_id) {
        return class.file_path == scope;
    }
    if let Some(doc) = map.docs.iter().find(|doc| doc.id == target_id) {
        return doc.path == scope;
    }
    if let Some(config) = map.configs.iter().find(|config| config.id == target_id) {
        return config.path == scope;
    }
    false
}

fn risk_string(risk: &RiskFlag) -> String {
    format!("{} ({:?}): {}", risk.scope, risk.area, risk.reason)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        callers_view, children_view, docs_view, graph_expand_view, node_view, task_anchors_view,
        task_scope_view,
    };
    use crate::map::RepositoryMap;
    use crate::model::task::TaskInput;

    #[test]
    fn node_view_returns_function_metadata() {
        let root = std::env::temp_dir().join("aethyme_engine_navigation_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/auth")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(
            root.join("src/auth/service.py"),
            "def validate_token():\n    return True\n\ndef main():\n    return validate_token()\n",
        )
        .expect("write source file");
        fs::write(root.join("src/auth/architecture.md"), "# Auth\n").expect("write docs");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let function = map
            .functions
            .iter()
            .find(|function| function.name == "validate_token")
            .expect("function present");
        let view = node_view(&map, &function.id).expect("node view");

        assert_eq!(view.kind, "function");
        assert_eq!(view.path.as_deref(), Some("src/auth/service.py"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn navigation_views_expose_children_docs_and_task_scope() {
        let root = std::env::temp_dir().join("aethyme_engine_navigation_scope_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(
            root.join("GameEngine/technical-architecture.md"),
            "# Architecture\n",
        )
        .expect("write docs");
        fs::write(
            root.join("GameEngine/src/main.rs"),
            "fn helper() {}\nfn main() { helper(); }\n",
        )
        .expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let children = children_view(&map, "GameEngine");
        assert!(!children.items.is_empty());
        let docs = docs_view(&map, "GameEngine/src/main.rs");
        let task = TaskInput::from_task_text("Explain this repo");
        let anchors = task_anchors_view(&map, &task);
        let scope = task_scope_view(&map, &task);

        assert!(!anchors.anchors.is_empty());
        assert!(!scope.navigation_order.is_empty());
        assert!(docs.relation == "docs");

        let main = map
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("main function");
        let callers = callers_view(&map, &main.id);
        assert!(callers.items.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn graph_expand_view_exposes_compact_navigation_slice() {
        let root = std::env::temp_dir().join("aethyme_engine_navigation_expand_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::write(
            root.join("GameEngine/technical-architecture.md"),
            "# Architecture\n",
        )
        .expect("write docs");
        fs::write(
            root.join("GameEngine/src/main.rs"),
            "fn helper() {}\nfn main() { helper(); }\n",
        )
        .expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let view = graph_expand_view(&map, "GameEngine/Cargo.toml").expect("expand view");

        assert_eq!(view.target.kind, "config");
        assert!(!view.configs.is_empty() || !view.docs.is_empty() || !view.parents.is_empty());

        let _ = fs::remove_dir_all(&root);
    }
}
