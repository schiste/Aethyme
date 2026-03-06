use crate::area::AreaNode;
use crate::class::ClassNode;
use crate::config::ConfigNode;
use crate::context_pack::{Anchor, ContextPack, DependencyEdge, ImpactItem, Snippet};
use crate::directory::DirectoryNode;
use crate::doc::DocNode;
use crate::edge::{Edge, EdgeKind};
use crate::file::{FileNode, FileRole};
use crate::function::FunctionNode;
use crate::graph::{GraphAnnotation, GraphNode, GraphNodeKind};
use crate::map::RepositoryMap;
use crate::repo::RepoSnapshot;
use crate::risk::{RiskArea, RiskFlag, RiskLevel};
use crate::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::search::SearchHit;
use crate::symbol::{Symbol, SymbolKind};
use crate::task::{TaskInput, TaskKind};

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
    format!(
        "{{\"task\":{},\"anchors\":[{}],\"in_scope\":{},\"out_of_scope\":{},\"dependencies\":[{}],\"impact\":[{}],\"snippets\":[{}],\"risk_flags\":[{}],\"navigation_order\":{},\"budget\":{{\"max_anchors\":{},\"max_files\":{},\"max_snippets\":{},\"dependency_depth\":{},\"impact_depth\":{}}},\"confidence\":{{\"anchor_confidence\":{},\"scope_confidence\":{}}}}}",
        task_input(&pack.task),
        pack.anchors.iter().map(anchor).collect::<Vec<_>>().join(","),
        scope_boundary(&pack.in_scope),
        scope_boundary(&pack.out_of_scope),
        pack.dependencies.iter().map(dependency).collect::<Vec<_>>().join(","),
        pack.impact.iter().map(impact).collect::<Vec<_>>().join(","),
        pack.snippets.iter().map(snippet).collect::<Vec<_>>().join(","),
        pack.risk_flags.iter().map(risk_flag).collect::<Vec<_>>().join(","),
        string_array(&pack.navigation_order),
        pack.budget.max_anchors,
        pack.budget.max_files,
        pack.budget.max_snippets,
        pack.budget.dependency_depth,
        pack.budget.impact_depth,
        pack.confidence.anchor_confidence,
        pack.confidence.scope_confidence,
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
        "{{\"snapshot\":{},\"areas\":[{}],\"directories\":[{}],\"files\":[{}],\"classes\":[{}],\"functions\":[{}],\"docs\":[{}],\"configs\":[{}],\"symbols\":[{}],\"edges\":[{}],\"risk_flags\":[{}],\"graph\":{{\"nodes\":[{}],\"edges\":[{}],\"annotations\":[{}]}}}}",
        repo_snapshot(&map.snapshot),
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
        map.graph.nodes.iter().map(graph_node).collect::<Vec<_>>().join(","),
        map.graph.edges.iter().map(edge).collect::<Vec<_>>().join(","),
        map.graph.annotations.iter().map(annotation).collect::<Vec<_>>().join(","),
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

fn graph_node(node: &GraphNode) -> String {
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
