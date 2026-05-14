//! Rust indexer (Phase 3.6) via rust-analyzer's `ra_ap_syntax` crate.
//!
//! ra_ap_syntax is the same parser that powers rust-analyzer's
//! IDE features. Pure Rust, MIT/Apache-2.0, optimized for both
//! cold and incremental parses. Picked over `syn` because syn is
//! proc-macro-oriented (small token streams, narrow API surface);
//! ra_ap_syntax is designed for full source files at IDE
//! responsiveness.
//!
//! Extracted node kinds:
//! - Function (top-level `fn`)
//! - Method (each `fn` inside a `trait` block — its receiver_type
//!   is the trait's NodeId)
//! - Struct, Enum, Trait, TypeAlias
//! - GlobalVariable (top-level `const` / `static`)
//!
//! Extracted edges:
//! - Contains (file → top-level symbol)
//! - Defines (trait → trait method)
//!
//! Deferred for later commits:
//! - `impl` block methods. v1 emits these as Functions because
//!   the impl block's target type sits in a different syntactic
//!   block, and resolving "impl Foo {}" → "struct Foo {}" needs
//!   cross-block resolution that doesn't exist yet. Methods
//!   defined inside `trait` blocks DO emit as Methods because the
//!   trait NodeId is in scope.
//! - `union` is mapped to Struct (no Union kind in the schema).
//! - Nested module items (`mod inner { fn ... }`). v1 walks only
//!   the file's top-level items.

use ra_ap_syntax::ast::{HasModuleItem, HasName, HasVisibility};
use ra_ap_syntax::{ast, AstNode, Edition, SourceFile, SyntaxNode};

use aethyme_graph_schema::{
    Callable, Confidence, Edge, EdgeAttributes, Enum as SchemaEnum,
    Function as SchemaFunction, GlobalVariable, Method, Node, NodeId,
    ParameterSignature, Source, SourceRange, Struct as SchemaStruct,
    Trait as SchemaTrait, TypeAlias, Visibility,
};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;
use crate::language::{
    LanguageIndexError, LanguageIndexResult, LanguageIndexer, LineIndex,
};

/// Indexer for `.rs` files via ra_ap_syntax.
pub struct RustIndexer;

impl RustIndexer {
    pub fn new() -> Self {
        RustIndexer
    }
}

impl Default for RustIndexer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageIndexer for RustIndexer {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn index_file(
        &self,
        ctx: &IndexerContext,
        indexed_file: &IndexedFile,
        content: &str,
    ) -> Result<LanguageIndexResult, LanguageIndexError> {
        // ra_ap_syntax produces a syntax tree even on parse errors;
        // we proceed with the partial tree and capture errors only
        // when we want to surface them. v1 ignores them (the
        // produced nodes are still useful).
        let parse = SourceFile::parse(content, Edition::CURRENT);
        let file = parse.tree();
        let line_index = LineIndex::new(content);

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let file_id = indexed_file.top_node.id().clone();
        let repo = ctx.repo_name();
        let source_path = &*indexed_file.source_path;

        for item in file.items() {
            walk_top_level_item(
                &item,
                repo,
                source_path,
                &file_id,
                &line_index,
                &mut nodes,
                &mut edges,
            )?;
        }

        Ok(LanguageIndexResult {
            additional_nodes: nodes,
            additional_edges: edges,
        })
    }
}

fn walk_top_level_item(
    item: &ast::Item,
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    line_index: &LineIndex,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    match item {
        ast::Item::Fn(f) => {
            if let Some(fn_node) =
                build_function(repo, source_path, f, line_index, true)?
            {
                let id = fn_node.id().clone();
                nodes.push(Node::Function(fn_node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        ast::Item::Struct(s) => {
            if let Some(node) = build_struct(repo, source_path, s, line_index)?
            {
                let id = node.id().clone();
                nodes.push(Node::Struct(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        // Rust unions map to Struct in our schema (no Union kind).
        ast::Item::Union(u) => {
            let Some(name) = u.name() else { return Ok(()); };
            let source_range = node_range(u.syntax(), line_index)?;
            let visibility = visibility_from_rust(u.visibility());
            let node = SchemaStruct::new(
                repo,
                source_path,
                name.text().as_ref(),
                source_range,
                visibility,
            )
            .map_err(node_construction_err)?;
            let id = node.id().clone();
            nodes.push(Node::Struct(node));
            edges.push(structural_edge(
                EdgeAttributes::Contains,
                file_id.clone(),
                id,
            ));
        }
        ast::Item::Enum(e) => {
            if let Some(node) = build_enum(repo, source_path, e, line_index)? {
                let id = node.id().clone();
                nodes.push(Node::Enum(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        ast::Item::Trait(t) => {
            if let Some(trait_node) =
                build_trait(repo, source_path, t, line_index)?
            {
                let trait_id = trait_node.id().clone();
                nodes.push(Node::Trait(trait_node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    trait_id.clone(),
                ));
                // Methods defined inside the trait body. The
                // receiver_type is the trait's NodeId.
                if let Some(item_list) = t.assoc_item_list() {
                    for assoc in item_list.assoc_items() {
                        if let ast::AssocItem::Fn(f) = assoc
                            && let Some(method) = build_method_for_trait(
                                repo,
                                source_path,
                                &f,
                                trait_id.clone(),
                                line_index,
                            )?
                        {
                            let id = method.id().clone();
                            nodes.push(Node::Method(method));
                            edges.push(structural_edge(
                                EdgeAttributes::Defines,
                                trait_id.clone(),
                                id,
                            ));
                        }
                    }
                }
            }
        }
        ast::Item::TypeAlias(t) => {
            if let Some(node) =
                build_type_alias(repo, source_path, t, line_index)?
            {
                let id = node.id().clone();
                nodes.push(Node::TypeAlias(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        ast::Item::Impl(i) => {
            // v1: emit each impl method as a top-level Function
            // (Contains file→fn). Future commit can resolve the
            // impl's target type and emit these as Methods.
            if let Some(item_list) = i.assoc_item_list() {
                for assoc in item_list.assoc_items() {
                    if let ast::AssocItem::Fn(f) = assoc
                        && let Some(fn_node) = build_function(
                            repo,
                            source_path,
                            &f,
                            line_index,
                            /* is_top_level */ false,
                        )?
                    {
                        let id = fn_node.id().clone();
                        nodes.push(Node::Function(fn_node));
                        edges.push(structural_edge(
                            EdgeAttributes::Contains,
                            file_id.clone(),
                            id,
                        ));
                    }
                }
            }
        }
        ast::Item::Const(c) => {
            if let Some(name) = c.name() {
                let source_range = node_range(c.syntax(), line_index)?;
                let node = GlobalVariable::new(
                    repo,
                    source_path,
                    name.text().as_ref(),
                    None,
                    source_range,
                )
                .map_err(node_construction_err)?;
                let id = node.id().clone();
                nodes.push(Node::GlobalVariable(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        ast::Item::Static(s) => {
            if let Some(name) = s.name() {
                let source_range = node_range(s.syntax(), line_index)?;
                let node = GlobalVariable::new(
                    repo,
                    source_path,
                    name.text().as_ref(),
                    None,
                    source_range,
                )
                .map_err(node_construction_err)?;
                let id = node.id().clone();
                nodes.push(Node::GlobalVariable(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        // Module, Use, ExternBlock, ExternCrate, MacroCall, MacroDef,
        // MacroRules, AsmExpr — ignored in v1.
        _ => {}
    }
    Ok(())
}

// ─── Builders ────────────────────────────────────────────────────────

fn build_function(
    repo: &str,
    source_path: &str,
    f: &ast::Fn,
    line_index: &LineIndex,
    is_top_level: bool,
) -> Result<Option<SchemaFunction>, LanguageIndexError> {
    let Some(name) = f.name() else { return Ok(None); };
    let parameters = parameter_signatures(f);
    let is_async = f.async_token().is_some();
    let signature = synthesize_function_signature(
        name.text().as_ref(),
        &parameters,
        is_async,
    );
    let source_range = node_range(f.syntax(), line_index)?;
    let visibility = visibility_from_rust(f.visibility());
    SchemaFunction::new(
        repo,
        source_path,
        name.text().as_ref(),
        &signature,
        parameters,
        None,
        source_range,
        visibility,
        is_top_level,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_method_for_trait(
    repo: &str,
    source_path: &str,
    f: &ast::Fn,
    receiver_type: NodeId,
    line_index: &LineIndex,
) -> Result<Option<Method>, LanguageIndexError> {
    let Some(name) = f.name() else { return Ok(None); };
    let parameters = parameter_signatures(f);
    let is_async = f.async_token().is_some();
    let signature = synthesize_function_signature(
        name.text().as_ref(),
        &parameters,
        is_async,
    );
    let source_range = node_range(f.syntax(), line_index)?;
    let visibility = visibility_from_rust(f.visibility());
    // Trait method without a body is the abstract/default-method
    // pattern in Rust. Mark all trait methods as is_virtual=true;
    // is_static for trait methods is determined by whether the
    // first param is `self` (we approximate via param_list).
    let is_static = !has_self_param(f);
    Method::new(
        repo,
        source_path,
        name.text().as_ref(),
        &signature,
        parameters,
        None,
        source_range,
        visibility,
        receiver_type,
        is_static,
        true,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_struct(
    repo: &str,
    source_path: &str,
    s: &ast::Struct,
    line_index: &LineIndex,
) -> Result<Option<SchemaStruct>, LanguageIndexError> {
    let Some(name) = s.name() else { return Ok(None); };
    let source_range = node_range(s.syntax(), line_index)?;
    let visibility = visibility_from_rust(s.visibility());
    SchemaStruct::new(
        repo,
        source_path,
        name.text().as_ref(),
        source_range,
        visibility,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_enum(
    repo: &str,
    source_path: &str,
    e: &ast::Enum,
    line_index: &LineIndex,
) -> Result<Option<SchemaEnum>, LanguageIndexError> {
    let Some(name) = e.name() else { return Ok(None); };
    let source_range = node_range(e.syntax(), line_index)?;
    let visibility = visibility_from_rust(e.visibility());
    SchemaEnum::new(
        repo,
        source_path,
        name.text().as_ref(),
        Vec::new(), // variant extraction deferred to a future commit
        source_range,
        visibility,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_trait(
    repo: &str,
    source_path: &str,
    t: &ast::Trait,
    line_index: &LineIndex,
) -> Result<Option<SchemaTrait>, LanguageIndexError> {
    let Some(name) = t.name() else { return Ok(None); };
    let source_range = node_range(t.syntax(), line_index)?;
    let visibility = visibility_from_rust(t.visibility());
    SchemaTrait::new(
        repo,
        source_path,
        name.text().as_ref(),
        source_range,
        visibility,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_type_alias(
    repo: &str,
    source_path: &str,
    t: &ast::TypeAlias,
    line_index: &LineIndex,
) -> Result<Option<TypeAlias>, LanguageIndexError> {
    let Some(name) = t.name() else { return Ok(None); };
    let source_range = node_range(t.syntax(), line_index)?;
    let visibility = visibility_from_rust(t.visibility());
    // v1: target_type is a placeholder. Faithful reproduction
    // needs source-text round-trip of the type expression.
    let target_type = "<type>";
    TypeAlias::new(
        repo,
        source_path,
        name.text().as_ref(),
        target_type,
        source_range,
        visibility,
    )
    .map(Some)
    .map_err(node_construction_err)
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn parameter_signatures(f: &ast::Fn) -> Vec<ParameterSignature> {
    let mut out = Vec::new();
    let Some(params) = f.param_list() else { return out; };
    // Include self_param as the first parameter when present, to keep
    // the signature recognizable. Its name is always "self".
    if let Some(_self_param) = params.self_param() {
        out.push(ParameterSignature {
            name: "self".into(),
            type_str: None,
            default_value: None,
        });
    }
    for p in params.params() {
        let name = p
            .pat()
            .map(|pat| pat.syntax().text().to_string())
            .unwrap_or_else(|| "<unnamed>".to_string());
        let type_str = p.ty().map(|t| Box::from(t.syntax().text().to_string()));
        out.push(ParameterSignature {
            name: name.into(),
            type_str,
            default_value: None,
        });
    }
    out
}

fn has_self_param(f: &ast::Fn) -> bool {
    f.param_list()
        .and_then(|p| p.self_param())
        .is_some()
}

fn synthesize_function_signature(
    name: &str,
    parameters: &[ParameterSignature],
    is_async: bool,
) -> String {
    let prefix = if is_async { "async fn" } else { "fn" };
    let params_str = parameters
        .iter()
        .map(|p| {
            if let Some(ty) = &p.type_str {
                format!("{}: {ty}", p.name)
            } else {
                p.name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{prefix} {name}({params_str})")
}

fn node_range(
    node: &SyntaxNode,
    line_index: &LineIndex,
) -> Result<SourceRange, LanguageIndexError> {
    let range = node.text_range();
    let start = line_index.line_at(u32::from(range.start()) as usize);
    let end = line_index.line_at(u32::from(range.end()) as usize);
    SourceRange::new(start, end).map_err(|e| {
        LanguageIndexError::NodeConstruction {
            message: format!("source range: {e}"),
        }
    })
}

fn visibility_from_rust(vis: Option<ast::Visibility>) -> Visibility {
    // Rust's `pub` keyword presence → Public; otherwise the item is
    // module-private. Granular variants (`pub(crate)`, `pub(super)`,
    // `pub(in path)`) all collapse to Module in our schema's
    // 4-variant taxonomy.
    //
    // Heuristic: ast::Visibility's source text gives us the easiest
    // way to discriminate "bare pub" from "pub(crate)" etc. across
    // ra_ap_syntax versions where the typed API has churned.
    match vis {
        Some(v) => {
            let text = v.syntax().text().to_string();
            let trimmed = text.trim();
            if trimmed == "pub" {
                Visibility::Public
            } else {
                Visibility::Module
            }
        }
        None => Visibility::Module,
    }
}

fn structural_edge(
    attributes: EdgeAttributes,
    src: NodeId,
    dst: NodeId,
) -> Edge {
    Edge::new(src, dst, attributes, Source::Structure, Confidence::FULL)
}

fn node_construction_err(
    e: impl std::fmt::Display,
) -> LanguageIndexError {
    LanguageIndexError::NodeConstruction {
        message: e.to_string(),
    }
}

