use crate::context_pack::{Anchor, AnchorKind};
use crate::graph::anchors::resolve_anchors;
use crate::graph::guidance::{build_in_scope, build_out_of_scope, navigation_order};
use crate::graph::overview::build_repo_overview;
use crate::graph::signals::{GraphSignals, evaluate_graph_signals};
use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::risk::RiskFlag;
use crate::model::task::TaskInput;

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

pub fn task_anchors_view(map: &RepositoryMap, task: &TaskInput) -> TaskAnchorsView {
    let anchors = resolve_anchors(map, task, 5);
    TaskAnchorsView {
        task: task.raw.clone(),
        anchors,
    }
}

pub fn task_scope_view(map: &RepositoryMap, task: &TaskInput) -> TaskScopeView {
    let anchors = resolve_anchors(map, task, 5);
    let signals = evaluate_graph_signals(map);
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

pub fn task_next_view(map: &RepositoryMap, task: &TaskInput) -> GraphRelationView {
    let anchors = resolve_anchors(map, task, 5);
    let signals = evaluate_graph_signals(map);
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

fn relation_item_for_display(
    map: &RepositoryMap,
    display: &str,
    relation: &str,
) -> Option<GraphRelationItem> {
    let target_id = resolved_target_id(map, display)?;
    relation_item(map, &target_id, relation, 1000)
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
        if !seed.contains(&function.file_path) {
            seed.push(function.file_path.clone());
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

fn direct_change_relations(map: &RepositoryMap, target: &str) -> [GraphRelationView; 4] {
    [
        callers_view(map, target),
        callees_view(map, target),
        docs_view(map, target),
        configs_view(map, target),
    ]
}

fn change_display_for_relation_item(map: &RepositoryMap, item: GraphRelationItem) -> String {
    if item.kind == "function" {
        if let Some(function) = map.functions.iter().find(|function| function.id == item.id) {
            return function.file_path.clone();
        }
    }
    item.display
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
            .map(|function| function.qualified_name.clone())
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
