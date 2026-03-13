use crate::area::AreaNode;
use crate::class::ClassNode;
use crate::config::ConfigNode;
use crate::activation::ActivationMap;
use crate::context_pack::{ActivationSummary, Anchor, ContextPack, DependencyEdge, FileContent, ImpactItem, PackSnapshot, PackSummary, Snippet};
use crate::overview::{build_repo_overview, repo_overview_seed};
use crate::directory::DirectoryNode;
use crate::doc::DocNode;
use crate::edge::{Edge, EdgeKind};
use crate::file::{FileNode, FileRole};
use crate::function::FunctionNode;
use crate::graph::{GraphAnnotation, GraphNode, GraphNodeKind};
use crate::map::{RepositoryBuildProfile, RepositoryMap};
use crate::navigation::{
    GraphExpandView, GraphNodeView, GraphRelationItem, GraphRelationView, TaskAnchorsView, TaskExpandView,
    TaskScopeView, RepoOverviewView,
};
use crate::repo::RepoSnapshot;
use crate::risk::{RiskArea, RiskFlag, RiskLevel};
use crate::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::search::SearchHit;
use crate::signals::{GraphSignals, SignalAssessment};
use crate::symbol::{Symbol, SymbolKind};
use crate::task::{TaskInput, TaskKind};
use crate::workspace::{BlastRadiusItem, CrossEdgeKind, CrossRepoEdge, SharedDependency, WorkspaceGraph};

pub fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn string(value: &str) -> String {
    format!("\"{}\"", escape(value))
}

fn string_array(values: &[String]) -> String {
    format!("[{}]", values.iter().map(|value| string(value)).collect::<Vec<_>>().join(","))
}

fn scope_kind(kind: &ScopeKind) -> &'static str {
    match kind {
        ScopeKind::File => "file",
        ScopeKind::Folder => "folder",
        ScopeKind::Symbol => "symbol",
        ScopeKind::Area => "area",
    }
}

fn symbol_kind(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Constant => "constant",
    }
}

fn task_kind(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::ExplainRepo => "explain_repo",
        TaskKind::ExplainComponent => "explain_component",
        TaskKind::ChangeSymbol => "change_symbol",
        TaskKind::TraceImpact => "trace_impact",
        TaskKind::NavigateConfigOwnership => "navigate_config_ownership",
        TaskKind::Unknown => "unknown",
    }
}

fn edge_kind(kind: &EdgeKind) -> &'static str {
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

fn risk_area(area: &RiskArea) -> String {
    match area {
        RiskArea::Auth => "auth".to_string(),
        RiskArea::Permissions => "permissions".to_string(),
        RiskArea::Secrets => "secrets".to_string(),
        RiskArea::Migrations => "migrations".to_string(),
        RiskArea::Infra => "infra".to_string(),
        RiskArea::Billing => "billing".to_string(),
        RiskArea::SharedCore => "shared-core".to_string(),
        RiskArea::Destructive => "destructive".to_string(),
        RiskArea::UserDefined(value) => value.clone(),
    }
}

fn risk_level(level: &RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

fn file_role(role: &FileRole) -> &'static str {
    match role {
        FileRole::Source => "source",
        FileRole::Test => "test",
        FileRole::Doc => "doc",
        FileRole::Config => "config",
        FileRole::Asset => "asset",
        FileRole::Generated => "generated",
        FileRole::Binary => "binary",
        FileRole::Cache => "cache",
        FileRole::Unknown => "unknown",
    }
}

fn graph_node_kind(kind: &GraphNodeKind) -> &'static str {
    match kind {
        GraphNodeKind::Repo => "repo",
        GraphNodeKind::Area => "area",
        GraphNodeKind::Directory => "directory",
        GraphNodeKind::File => "file",
        GraphNodeKind::Class => "class",
        GraphNodeKind::Function => "function",
        GraphNodeKind::Doc => "doc",
        GraphNodeKind::Config => "config",
    }
}

fn scope_item(item: &ScopeItem) -> String {
    format!(
        "{{\"value\":{},\"kind\":{},\"reason\":{}}}",
        string(&item.value),
        string(scope_kind(&item.kind)),
        string(&item.reason)
    )
}

fn scope_boundary(boundary: &ScopeBoundary) -> String {
    format!(
        "{{\"files\":[{}],\"symbols\":[{}],\"areas\":[{}]}}",
        boundary.files.iter().map(scope_item).collect::<Vec<_>>().join(","),
        boundary.symbols.iter().map(scope_item).collect::<Vec<_>>().join(","),
        boundary.areas.iter().map(scope_item).collect::<Vec<_>>().join(",")
    )
}

fn task_input(task: &TaskInput) -> String {
    format!(
        "{{\"raw\":{},\"normalized\":{},\"kind\":{}}}",
        string(&task.raw),
        string(&task.normalized),
        string(task_kind(&task.kind))
    )
}

fn anchor(anchor: &Anchor) -> String {
    let kind = match anchor.kind {
        crate::context_pack::AnchorKind::Symbol => "symbol",
        crate::context_pack::AnchorKind::File => "file",
        crate::context_pack::AnchorKind::Folder => "folder",
    };
    let file = anchor.file.as_deref().map(string).unwrap_or_else(|| "null".to_string());
    format!(
        "{{\"kind\":{},\"id\":{},\"file\":{},\"reason\":{}}}",
        string(kind),
        string(&anchor.id),
        file,
        string(&anchor.reason)
    )
}

fn dependency(edge: &DependencyEdge) -> String {
    format!(
        "{{\"from\":{},\"to\":{},\"kind\":{}}}",
        string(&edge.from),
        string(&edge.to),
        string(&edge.kind)
    )
}

fn impact(item: &ImpactItem) -> String {
    format!(
        "{{\"symbol\":{},\"file\":{},\"reason\":{}}}",
        string(&item.symbol),
        string(&item.file),
        string(&item.reason)
    )
}

fn snippet(snippet: &Snippet) -> String {
    format!(
        "{{\"file\":{},\"start_line\":{},\"end_line\":{},\"kind\":{}}}",
        string(&snippet.file),
        snippet.start_line,
        snippet.end_line,
        string(&snippet.kind)
    )
}

fn file_content(fc: &FileContent) -> String {
    format!(
        "{{\"path\":{},\"content\":{},\"start_line\":{},\"end_line\":{},\"total_lines\":{},\"reason\":{}}}",
        string(&fc.path),
        string(&fc.content),
        fc.start_line,
        fc.end_line,
        fc.total_lines,
        string(&fc.reason)
    )
}

fn risk_flag(flag: &RiskFlag) -> String {
    format!(
        "{{\"scope\":{},\"area\":{},\"level\":{},\"reason\":{}}}",
        string(&flag.scope),
        string(&risk_area(&flag.area)),
        string(risk_level(&flag.level)),
        string(&flag.reason)
    )
}

pub fn context_pack(pack: &ContextPack) -> String {
    let activation_json = pack
        .activation_summary
        .as_ref()
        .map(activation_summary)
        .unwrap_or_else(|| "null".to_string());
    let file_contents_json = pack.file_contents.iter().map(file_content).collect::<Vec<_>>().join(",");
    format!(
        "{{\"task\":{},\"summary\":{},\"signals\":{},\"overview\":{},\"anchors\":[{}],\"in_scope\":{},\"out_of_scope\":{},\"dependencies\":[{}],\"impact\":[{}],\"snippets\":[{}],\"file_contents\":[{}],\"risk_flags\":[{}],\"navigation_order\":{},\"budget\":{{\"max_anchors\":{},\"max_files\":{},\"max_snippets\":{},\"dependency_depth\":{},\"impact_depth\":{},\"snippet_window\":{},\"content_budget\":{},\"max_content_files\":{},\"max_lines_per_file\":{}}},\"confidence\":{{\"anchor_confidence\":{},\"scope_confidence\":{}}},\"activation_summary\":{}}}",
        task_input(&pack.task),
        pack_summary(&pack.summary),
        graph_signals(&pack.signals),
        repo_overview(pack),
        pack.anchors.iter().map(anchor).collect::<Vec<_>>().join(","),
        scope_boundary(&pack.in_scope),
        scope_boundary(&pack.out_of_scope),
        pack.dependencies.iter().map(dependency).collect::<Vec<_>>().join(","),
        pack.impact.iter().map(impact).collect::<Vec<_>>().join(","),
        pack.snippets.iter().map(snippet).collect::<Vec<_>>().join(","),
        file_contents_json,
        pack.risk_flags.iter().map(risk_flag).collect::<Vec<_>>().join(","),
        string_array(&pack.navigation_order),
        pack.budget.max_anchors,
        pack.budget.max_files,
        pack.budget.max_snippets,
        pack.budget.dependency_depth,
        pack.budget.impact_depth,
        pack.budget.snippet_window,
        pack.budget.content_budget,
        pack.budget.max_content_files,
        pack.budget.max_lines_per_file,
        pack.confidence.anchor_confidence,
        pack.confidence.scope_confidence,
        activation_json,
    )
}

fn pack_snapshot(snapshot: &PackSnapshot) -> String {
    let readme_path = snapshot
        .readme_path
        .as_deref()
        .map(string)
        .unwrap_or_else(|| "null".to_string());
    format!(
        "{{\"languages\":{},\"top_level_dirs\":{},\"readme_path\":{}}}",
        string_array(&snapshot.languages),
        string_array(&snapshot.top_level_dirs),
        readme_path,
    )
}

fn pack_summary(summary: &PackSummary) -> String {
    format!(
        "{{\"snapshot\":{},\"files_count\":{},\"functions_count\":{},\"classes_count\":{},\"docs_count\":{},\"configs_count\":{}}}",
        pack_snapshot(&summary.snapshot),
        summary.files_count,
        summary.functions_count,
        summary.classes_count,
        summary.docs_count,
        summary.configs_count,
    )
}

fn repo_overview(pack: &ContextPack) -> String {
    format!(
        "{{\"overview_docs\":{},\"code_areas\":{},\"reference_areas\":{},\"subareas\":{},\"entrypoints\":{},\"key_configs\":{},\"representative_code_files\":{},\"representative_docs\":{}}}",
        string_array(&pack.overview.overview_docs),
        string_array(&pack.overview.code_areas),
        string_array(&pack.overview.reference_areas),
        string_array(&pack.overview.subareas),
        string_array(&pack.overview.entrypoints),
        string_array(&pack.overview.key_configs),
        string_array(&pack.overview.representative_code_files),
        string_array(&pack.overview.representative_docs),
    )
}

pub fn search_hits(hits: &[SearchHit]) -> String {
    format!(
        "[{}]",
        hits.iter()
            .map(|hit| {
                format!(
                    "{{\"id\":{},\"name\":{},\"kind\":{},\"file\":{},\"line\":{},\"score\":{},\"reason\":{}}}",
                    string(&hit.id),
                    string(&hit.name),
                    string(&hit.kind),
                    string(&hit.file),
                    hit.line,
                    hit.score,
                    string(&hit.reason)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub fn string_list(items: &[String]) -> String {
    string_array(items)
}

pub fn repository_map(map: &RepositoryMap) -> String {
    format!(
        "{{\"snapshot\":{},\"signals\":{},\"areas\":[{}],\"directories\":[{}],\"files\":[{}],\"classes\":[{}],\"functions\":[{}],\"docs\":[{}],\"configs\":[{}],\"symbols\":[{}],\"edges\":[{}],\"risk_flags\":[{}],\"graph\":{{\"nodes\":[{}],\"edges\":[{}],\"annotations\":[{}]}}}}",
        repo_snapshot(&map.snapshot),
        graph_signals(&crate::signals::evaluate_graph_signals(map)),
        map.areas.iter().map(area).collect::<Vec<_>>().join(","),
        map.directories.iter().map(directory).collect::<Vec<_>>().join(","),
        map.files.iter().map(file).collect::<Vec<_>>().join(","),
        map.classes.iter().map(class).collect::<Vec<_>>().join(","),
        map.functions.iter().map(function).collect::<Vec<_>>().join(","),
        map.docs.iter().map(doc).collect::<Vec<_>>().join(","),
        map.configs.iter().map(config).collect::<Vec<_>>().join(","),
        map.symbols.iter().map(symbol).collect::<Vec<_>>().join(","),
        map.edges.iter().map(edge).collect::<Vec<_>>().join(","),
        map.risk_flags.iter().map(risk_flag).collect::<Vec<_>>().join(","),
        map.graph
            .nodes
            .iter()
            .map(graph_node_record)
            .collect::<Vec<_>>()
            .join(","),
        map.graph.edges.iter().map(edge).collect::<Vec<_>>().join(","),
        map.graph.annotations.iter().map(annotation).collect::<Vec<_>>().join(","),
    )
}

pub fn inspect_brief(map: &RepositoryMap) -> String {
    let overview = build_repo_overview(map, &repo_overview_seed(map));
    format!(
        "{{\"snapshot\":{},\"signals\":{},\"areas\":[{}],\"entrypoints\":{},\"key_configs\":{}}}",
        repo_snapshot_compact(&map.snapshot),
        graph_signals(&crate::signals::evaluate_graph_signals(map)),
        map.areas.iter().map(area).collect::<Vec<_>>().join(","),
        string_array(&overview.entrypoints),
        string_array(&overview.key_configs),
    )
}

pub fn inspect_structure(map: &RepositoryMap) -> String {
    let overview = build_repo_overview(map, &repo_overview_seed(map));
    let compact_files = map.files.iter().map(|f| {
        format!(
            "{{\"path\":{},\"role\":{},\"area_id\":{}}}",
            string(&f.path),
            string(file_role(&f.role)),
            f.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        )
    }).collect::<Vec<_>>().join(",");
    format!(
        "{{\"snapshot\":{},\"signals\":{},\"areas\":[{}],\"files\":[{}],\"configs\":[{}],\"docs\":[{}],\"entrypoints\":{},\"key_configs\":{}}}",
        repo_snapshot_compact(&map.snapshot),
        graph_signals(&crate::signals::evaluate_graph_signals(map)),
        map.areas.iter().map(area).collect::<Vec<_>>().join(","),
        compact_files,
        map.configs.iter().map(config).collect::<Vec<_>>().join(","),
        map.docs.iter().map(doc).collect::<Vec<_>>().join(","),
        string_array(&overview.entrypoints),
        string_array(&overview.key_configs),
    )
}

pub fn build_profile(profile: &RepositoryBuildProfile) -> String {
    format!(
        "{{\"total_duration_ms\":{},\"stages\":[{}],\"counts\":{{\"repo_files\":{},\"source_files\":{},\"doc_files\":{},\"config_files\":{},\"areas\":{},\"directories\":{},\"classes\":{},\"functions\":{},\"docs\":{},\"configs\":{},\"graph_nodes\":{},\"graph_edges\":{},\"graph_annotations\":{}}},\"cache\":{{\"hits\":{},\"misses\":{}}}}}",
        profile.total_duration_ms,
        profile
            .stages
            .iter()
            .map(|stage| {
                format!(
                    "{{\"name\":{},\"duration_ms\":{}}}",
                    string(&stage.name),
                    stage.duration_ms
                )
            })
            .collect::<Vec<_>>()
            .join(","),
        profile.repo_files,
        profile.source_files,
        profile.doc_files,
        profile.config_files,
        profile.areas,
        profile.directories,
        profile.classes,
        profile.functions,
        profile.docs,
        profile.configs,
        profile.graph_nodes,
        profile.graph_edges,
        profile.graph_annotations,
        profile.cache_hits,
        profile.cache_misses,
    )
}

pub fn graph_node_view(view: &GraphNodeView) -> String {
    format!(
        "{{\"id\":{},\"kind\":{},\"label\":{},\"path\":{},\"language\":{},\"source\":{},\"confidence\":{},\"area\":{},\"annotations\":{}}}",
        string(&view.id),
        string(&view.kind),
        string(&view.label),
        view.path.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        view.language.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        string(&view.source),
        view.confidence,
        view.area.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        string_array(&view.annotations),
    )
}

pub fn graph_relation(view: &GraphRelationView) -> String {
    format!(
        "{{\"target\":{},\"relation\":{},\"items\":[{}]}}",
        string(&view.target),
        string(&view.relation),
        view.items.iter().map(graph_relation_item).collect::<Vec<_>>().join(",")
    )
}

pub fn graph_expand_view(view: &GraphExpandView) -> String {
    format!(
        "{{\"target\":{},\"parents\":[{}],\"children\":[{}],\"callers\":[{}],\"callees\":[{}],\"docs\":[{}],\"configs\":[{}],\"risks\":{}}}",
        graph_node_view(&view.target),
        view.parents.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        view.children.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        view.callers.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        view.callees.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        view.docs.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        view.configs.iter().map(graph_relation_item).collect::<Vec<_>>().join(","),
        string_array(&view.risks),
    )
}

pub fn repo_overview_view(view: &RepoOverviewView) -> String {
    format!(
        "{{\"repo\":{},\"signals\":{},\"overview_docs\":{},\"code_areas\":{},\"reference_areas\":{},\"subareas\":{},\"entrypoints\":{},\"key_configs\":{},\"representative_code_files\":{},\"representative_docs\":{}}}",
        string(&view.repo),
        graph_signals(&view.signals),
        string_array(&view.overview_docs),
        string_array(&view.code_areas),
        string_array(&view.reference_areas),
        string_array(&view.subareas),
        string_array(&view.entrypoints),
        string_array(&view.key_configs),
        string_array(&view.representative_code_files),
        string_array(&view.representative_docs),
    )
}

pub fn task_anchors_view(view: &TaskAnchorsView) -> String {
    format!(
        "{{\"task\":{},\"anchors\":[{}]}}",
        string(&view.task),
        view.anchors.iter().map(anchor).collect::<Vec<_>>().join(",")
    )
}

pub fn task_scope_view(view: &TaskScopeView) -> String {
    format!(
        "{{\"task\":{},\"navigation_order\":{},\"in_scope_files\":{},\"in_scope_symbols\":{},\"in_scope_areas\":{},\"out_of_scope\":{},\"risks\":{}}}",
        string(&view.task),
        string_array(&view.navigation_order),
        string_array(&view.in_scope_files),
        string_array(&view.in_scope_symbols),
        string_array(&view.in_scope_areas),
        string_array(&view.out_of_scope),
        string_array(&view.risks),
    )
}

pub fn task_expand_view(view: &TaskExpandView) -> String {
    format!(
        "{{\"node\":{},\"dependencies\":{},\"impact\":{},\"docs\":{},\"configs\":{},\"risks\":{}}}",
        string(&view.node),
        string_array(&view.dependencies),
        string_array(&view.impact),
        string_array(&view.docs),
        string_array(&view.configs),
        string_array(&view.risks),
    )
}

fn repo_snapshot_compact(snapshot: &RepoSnapshot) -> String {
    format!(
        "{{\"root\":{},\"languages\":{},\"top_level_dirs\":{},\"readme_path\":{},\"file_count\":{}}}",
        string(&snapshot.root),
        string_array(&snapshot.languages),
        string_array(&snapshot.top_level_dirs),
        snapshot.readme_path.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        snapshot.files.len(),
    )
}

fn repo_snapshot(snapshot: &RepoSnapshot) -> String {
    format!(
        "{{\"root\":{},\"languages\":{},\"top_level_dirs\":{},\"readme_path\":{},\"files\":[{}]}}",
        string(&snapshot.root),
        string_array(&snapshot.languages),
        string_array(&snapshot.top_level_dirs),
        snapshot.readme_path.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        snapshot
            .files
            .iter()
            .map(|file| {
                format!(
                    "{{\"path\":{},\"language\":{},\"line_count\":{},\"size_bytes\":{}}}",
                    string(&file.path),
                    file.language.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
                    file.line_count,
                    file.size_bytes,
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn graph_relation_item(item: &GraphRelationItem) -> String {
    format!(
        "{{\"id\":{},\"kind\":{},\"display\":{},\"relation\":{},\"confidence\":{}}}",
        string(&item.id),
        string(&item.kind),
        string(&item.display),
        string(&item.relation),
        item.confidence,
    )
}

fn graph_signals(signals: &GraphSignals) -> String {
    format!(
        "{{\"boundary_clarity\":{},\"entrypoint_clarity\":{},\"config_hygiene\":{},\"hidden_coupling\":{},\"parser_visibility\":{}}}",
        signal_assessment(&signals.boundary_clarity),
        signal_assessment(&signals.entrypoint_clarity),
        signal_assessment(&signals.config_hygiene),
        signal_assessment(&signals.hidden_coupling),
        signal_assessment(&signals.parser_visibility),
    )
}

fn signal_assessment(signal: &SignalAssessment) -> String {
    format!(
        "{{\"score\":{},\"level\":{},\"evidence\":{}}}",
        signal.score,
        string(&signal.level),
        string_array(&signal.evidence),
    )
}

fn area(area: &AreaNode) -> String {
    format!(
        "{{\"id\":{},\"name\":{},\"path_prefix\":{},\"inferred\":{}}}",
        string(&area.id),
        string(&area.name),
        string(&area.path_prefix),
        if area.inferred { "true" } else { "false" },
    )
}

fn directory(directory: &DirectoryNode) -> String {
    format!(
        "{{\"id\":{},\"path\":{},\"name\":{},\"area_id\":{}}}",
        string(&directory.id),
        string(&directory.path),
        string(&directory.name),
        directory.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string())
    )
}

fn file(file: &FileNode) -> String {
    format!(
        "{{\"id\":{},\"path\":{},\"name\":{},\"language\":{},\"role\":{},\"line_count\":{},\"size_bytes\":{},\"generated\":{},\"area_id\":{}}}",
        string(&file.id),
        string(&file.path),
        string(&file.name),
        file.language.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        string(file_role(&file.role)),
        file.line_count,
        file.size_bytes,
        if file.generated { "true" } else { "false" },
        file.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string())
    )
}

fn class(class: &ClassNode) -> String {
    format!(
        "{{\"id\":{},\"name\":{},\"qualified_name\":{},\"file_id\":{},\"file_path\":{},\"area_id\":{},\"language\":{},\"line\":{},\"signature\":{}}}",
        string(&class.id),
        string(&class.name),
        string(&class.qualified_name),
        string(&class.file_id),
        string(&class.file_path),
        class.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        string(&class.language),
        class.line,
        string(&class.signature)
    )
}

fn function(function: &FunctionNode) -> String {
    format!(
        "{{\"id\":{},\"name\":{},\"qualified_name\":{},\"file_id\":{},\"file_path\":{},\"area_id\":{},\"parent_class_id\":{},\"language\":{},\"line\":{},\"signature\":{}}}",
        string(&function.id),
        string(&function.name),
        string(&function.qualified_name),
        string(&function.file_id),
        string(&function.file_path),
        function.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        function.parent_class_id.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        string(&function.language),
        function.line,
        string(&function.signature)
    )
}

fn doc(doc: &DocNode) -> String {
    format!(
        "{{\"id\":{},\"file_id\":{},\"path\":{},\"title\":{},\"doc_type\":{},\"area_id\":{}}}",
        string(&doc.id),
        string(&doc.file_id),
        string(&doc.path),
        string(&doc.title),
        string(&doc.doc_type),
        doc.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string())
    )
}

fn config(config: &ConfigNode) -> String {
    format!(
        "{{\"id\":{},\"file_id\":{},\"path\":{},\"config_type\":{},\"area_id\":{}}}",
        string(&config.id),
        string(&config.file_id),
        string(&config.path),
        string(&config.config_type),
        config.area_id.as_deref().map(string).unwrap_or_else(|| "null".to_string())
    )
}

fn symbol(symbol: &Symbol) -> String {
    format!(
        "{{\"id\":{},\"name\":{},\"kind\":{},\"file\":{},\"line\":{},\"signature\":{},\"language\":{},\"area\":{}}}",
        string(&symbol.id),
        string(&symbol.name),
        string(symbol_kind(&symbol.kind)),
        string(&symbol.file),
        symbol.line,
        string(&symbol.signature),
        symbol.language.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        symbol.area.as_deref().map(string).unwrap_or_else(|| "null".to_string())
    )
}

fn edge(edge: &Edge) -> String {
    format!(
        "{{\"from\":{},\"to\":{},\"kind\":{},\"confidence\":{},\"source\":{}}}",
        string(&edge.from),
        string(&edge.to),
        string(edge_kind(&edge.kind)),
        edge.confidence,
        string(&edge.source)
    )
}

fn graph_node_record(node: &GraphNode) -> String {
    format!(
        "{{\"id\":{},\"kind\":{},\"label\":{},\"path\":{},\"language\":{},\"confidence\":{},\"source\":{}}}",
        string(&node.id),
        string(graph_node_kind(&node.kind)),
        string(&node.label),
        node.path.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        node.language.as_deref().map(string).unwrap_or_else(|| "null".to_string()),
        node.confidence,
        string(&node.source)
    )
}

fn annotation(annotation: &GraphAnnotation) -> String {
    format!(
        "{{\"target_id\":{},\"kind\":{},\"value\":{},\"confidence\":{},\"source\":{},\"reason\":{}}}",
        string(&annotation.target_id),
        string(&annotation.kind),
        string(&annotation.value),
        annotation.confidence,
        string(&annotation.source),
        string(&annotation.reason)
    )
}

fn cross_edge_kind(kind: &CrossEdgeKind) -> &'static str {
    match kind {
        CrossEdgeKind::DependsOn => "depends_on",
        CrossEdgeKind::SharedDependency => "shared_dependency",
        CrossEdgeKind::DirectReference => "direct_reference",
    }
}

fn cross_repo_edge(edge: &CrossRepoEdge) -> String {
    format!(
        "{{\"from_repo\":{},\"from_id\":{},\"to_repo\":{},\"to_id\":{},\"kind\":{},\"confidence\":{},\"evidence\":{}}}",
        string(&edge.from_repo),
        string(&edge.from_id),
        string(&edge.to_repo),
        string(&edge.to_id),
        string(cross_edge_kind(&edge.kind)),
        edge.confidence,
        string(&edge.evidence)
    )
}

fn shared_dependency(dep: &SharedDependency) -> String {
    format!(
        "{{\"name\":{},\"repos\":{},\"dep_type\":{}}}",
        string(&dep.name),
        string_array(&dep.repos),
        string(&dep.dep_type)
    )
}

pub fn workspace_graph(graph: &WorkspaceGraph) -> String {
    let repos_json: Vec<String> = graph
        .repos
        .iter()
        .map(|repo| {
            format!(
                "{{\"root\":{},\"name\":{},\"files\":{},\"functions\":{},\"classes\":{},\"external_deps\":{}}}",
                string(&repo.root),
                string(&repo.name),
                repo.map.files.len(),
                repo.map.functions.len(),
                repo.map.classes.len(),
                repo.external_deps.len(),
            )
        })
        .collect();
    format!(
        "{{\"repos\":[{}],\"cross_edges\":[{}],\"shared_dependencies\":[{}]}}",
        repos_json.join(","),
        graph.cross_edges.iter().map(cross_repo_edge).collect::<Vec<_>>().join(","),
        graph.shared_dependencies.iter().map(shared_dependency).collect::<Vec<_>>().join(","),
    )
}

pub fn blast_radius(items: &[BlastRadiusItem]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|item| {
                format!(
                    "{{\"repo\":{},\"id\":{},\"display\":{},\"reason\":{}}}",
                    string(&item.repo),
                    string(&item.id),
                    string(&item.display),
                    string(&item.reason)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub fn activation_map(map: &ActivationMap) -> String {
    format!(
        "{{\"seed_count\":{},\"total_nodes_visited\":{},\"max_depth_reached\":{},\"activations\":[{}]}}",
        map.seed_count,
        map.total_nodes_visited,
        map.max_depth_reached,
        map.activations
            .iter()
            .map(|node| {
                format!(
                    "{{\"id\":{},\"activation\":{:.4},\"depth\":{},\"source_seed\":{}}}",
                    string(&node.id),
                    node.activation,
                    node.depth,
                    string(&node.source_seed)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn activation_summary(summary: &ActivationSummary) -> String {
    let top = summary
        .top_activated
        .iter()
        .map(|(id, level)| format!("{{\"id\":{},\"activation\":{:.4}}}", string(id), level))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"activated_node_count\":{},\"max_depth_reached\":{},\"top_activated\":[{}]}}",
        summary.activated_node_count, summary.max_depth_reached, top,
    )
}
