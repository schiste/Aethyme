//! PHP indexer (Phase 3.7) via tree-sitter-php.
//!
//! This is the ONE documented C-dependency exception in the
//! indexer crate. The pure-Rust PHP parser ecosystem
//! (`php-parser-rs`) is too thin to ship against MediaWiki-scale
//! workloads; tree-sitter-php is the production-tested choice.
//!
//! Extracted node kinds:
//! - Function (top-level `function`)
//! - Method (inside `class`/`interface`/`trait` block)
//! - Class
//! - Interface
//! - Trait
//! - GlobalVariable (top-level `const` declarations)
//!
//! Extracted edges:
//! - Contains (file → top-level symbol)
//! - Defines (class/interface/trait → method)
//!
//! Deferred for later commits:
//! - `namespace` walking: namespace_definition nodes contain
//!   nested classes/functions; v1 walks namespaced content as if
//!   it were at file scope. Namespace-as-Module nodes lands later.
//! - Property declarations as Field nodes.
//! - Cross-file Imports / Calls edges.

use std::sync::Mutex;

use tree_sitter::{Node, Parser, Tree};

use aethyme_graph_schema::{
    Callable, Class as SchemaClass, Confidence, Edge, EdgeAttributes,
    Function as SchemaFunction, GlobalVariable,
    Interface as SchemaInterface, Method, Node as SchemaNode, NodeId,
    ParameterSignature, Source, SourceRange,
    Trait as SchemaTrait, Visibility,
};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;
use crate::language::{
    LanguageIndexError, LanguageIndexResult, LanguageIndexer, LineIndex,
};

/// Indexer for `.php` files via tree-sitter-php.
///
/// Wraps the tree-sitter Parser in a Mutex so the indexer can be
/// Send + Sync (required by the LanguageIndexer trait). Each
/// `index_file` call acquires the mutex briefly to parse — the
/// rayon-parallel pipeline serializes parse calls but parallelizes
/// the surrounding I/O + node construction.
pub struct PhpIndexer {
    parser: Mutex<Parser>,
}

impl PhpIndexer {
    pub fn new() -> Result<Self, LanguageIndexError> {
        let mut parser = Parser::new();
        let language: tree_sitter::Language =
            tree_sitter_php::LANGUAGE_PHP.into();
        parser.set_language(&language).map_err(|e| {
            LanguageIndexError::Parse {
                message: format!("set tree-sitter-php language: {e}"),
            }
        })?;
        Ok(PhpIndexer {
            parser: Mutex::new(parser),
        })
    }
}

impl LanguageIndexer for PhpIndexer {
    fn language(&self) -> &'static str {
        "php"
    }

    fn index_file(
        &self,
        ctx: &IndexerContext,
        indexed_file: &IndexedFile,
        content: &str,
    ) -> Result<LanguageIndexResult, LanguageIndexError> {
        let tree: Tree = {
            let mut parser = self.parser.lock().map_err(|_| {
                LanguageIndexError::Parse {
                    message: "PHP parser mutex poisoned".to_string(),
                }
            })?;
            parser.parse(content, None).ok_or_else(|| {
                LanguageIndexError::Parse {
                    message: "tree-sitter-php returned no tree".to_string(),
                }
            })?
        };

        let root = tree.root_node();
        let line_index = LineIndex::new(content);

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let file_id = indexed_file.top_node.id().clone();
        let repo = ctx.repo_name();
        let source_path = &*indexed_file.source_path;

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            walk_top_level(
                child,
                content,
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

#[allow(clippy::too_many_arguments)]
fn walk_top_level(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    line_index: &LineIndex,
    nodes: &mut Vec<SchemaNode>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    match node.kind() {
        "function_definition" => {
            if let Some(fn_node) =
                build_function(node, content, repo, source_path, line_index, true)?
            {
                let id = fn_node.id().clone();
                nodes.push(SchemaNode::Function(fn_node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        "class_declaration" => {
            if let Some(class) =
                build_class(node, content, repo, source_path, line_index)?
            {
                let class_id = class.id().clone();
                nodes.push(SchemaNode::Class(class));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    class_id.clone(),
                ));
                walk_class_body(
                    node,
                    content,
                    repo,
                    source_path,
                    class_id,
                    line_index,
                    nodes,
                    edges,
                    /* is_static_default */ false,
                )?;
            }
        }
        "interface_declaration" => {
            if let Some(iface) =
                build_interface(node, content, repo, source_path, line_index)?
            {
                let iface_id = iface.id().clone();
                nodes.push(SchemaNode::Interface(iface));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    iface_id.clone(),
                ));
                walk_class_body(
                    node,
                    content,
                    repo,
                    source_path,
                    iface_id,
                    line_index,
                    nodes,
                    edges,
                    /* is_static_default */ false,
                )?;
            }
        }
        "trait_declaration" => {
            if let Some(t) =
                build_trait(node, content, repo, source_path, line_index)?
            {
                let trait_id = t.id().clone();
                nodes.push(SchemaNode::Trait(t));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    trait_id.clone(),
                ));
                walk_class_body(
                    node,
                    content,
                    repo,
                    source_path,
                    trait_id,
                    line_index,
                    nodes,
                    edges,
                    /* is_static_default */ false,
                )?;
            }
        }
        "namespace_definition" => {
            // v1: walk namespace body as if top-level. Namespace-
            // as-Module emission is deferred.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_top_level(
                    child,
                    content,
                    repo,
                    source_path,
                    file_id,
                    line_index,
                    nodes,
                    edges,
                )?;
            }
        }
        "const_declaration" => {
            // Each const_element under a const_declaration is one
            // global constant. tree-sitter-php names these
            // "const_element" with a "name" child.
            let mut cursor = node.walk();
            for el in node.named_children(&mut cursor) {
                if el.kind() == "const_element"
                    && let Some(name) = field_text(el, "name", content)
                        .or_else(|| first_named_child_text(el, content))
                {
                    let source_range = node_range(node, line_index)?;
                    let g = GlobalVariable::new(
                        repo,
                        source_path,
                        &name,
                        None,
                        source_range,
                    )
                    .map_err(node_construction_err)?;
                    let id = g.id().clone();
                    nodes.push(SchemaNode::GlobalVariable(g));
                    edges.push(structural_edge(
                        EdgeAttributes::Contains,
                        file_id.clone(),
                        id,
                    ));
                }
            }
        }
        _ => {} // other top-level constructs ignored in v1
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn walk_class_body(
    container: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    container_id: NodeId,
    line_index: &LineIndex,
    nodes: &mut Vec<SchemaNode>,
    edges: &mut Vec<Edge>,
    _is_static_default: bool,
) -> Result<(), LanguageIndexError> {
    // The class/interface/trait body sits under a "declaration_list"
    // child. Walk that for method_declaration nodes.
    let Some(body) = container.child_by_field_name("body") else {
        return Ok(());
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if member.kind() == "method_declaration"
            && let Some(method) = build_method(
                member,
                content,
                repo,
                source_path,
                container_id.clone(),
                line_index,
            )?
        {
            let id = method.id().clone();
            nodes.push(SchemaNode::Method(method));
            edges.push(structural_edge(
                EdgeAttributes::Defines,
                container_id.clone(),
                id,
            ));
        }
    }
    Ok(())
}

// ─── Builders ────────────────────────────────────────────────────────

fn build_function(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    line_index: &LineIndex,
    is_top_level: bool,
) -> Result<Option<SchemaFunction>, LanguageIndexError> {
    let Some(name) = field_text(node, "name", content) else {
        return Ok(None);
    };
    let parameters = parameter_signatures(node, content);
    let signature = synthesize_function_signature(&name, &parameters);
    let source_range = node_range(node, line_index)?;
    let visibility = Visibility::Public; // PHP top-level functions are always public
    SchemaFunction::new(
        repo,
        source_path,
        &name,
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

fn build_method(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    receiver_type: NodeId,
    line_index: &LineIndex,
) -> Result<Option<Method>, LanguageIndexError> {
    let Some(name) = field_text(node, "name", content) else {
        return Ok(None);
    };
    let parameters = parameter_signatures(node, content);
    let signature = synthesize_function_signature(&name, &parameters);
    let source_range = node_range(node, line_index)?;
    let visibility = php_method_visibility(node, content);
    let is_static = php_method_is_static(node, content);
    // PHP methods are virtual by default unless declared `final`.
    let is_virtual =
        !node_text(node, content).contains("final ");
    Method::new(
        repo,
        source_path,
        &name,
        &signature,
        parameters,
        None,
        source_range,
        visibility,
        receiver_type,
        is_static,
        is_virtual,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_class(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    line_index: &LineIndex,
) -> Result<Option<SchemaClass>, LanguageIndexError> {
    let Some(name) = field_text(node, "name", content) else {
        return Ok(None);
    };
    let source_range = node_range(node, line_index)?;
    SchemaClass::new(repo, source_path, &name, source_range, Visibility::Public)
        .map(Some)
        .map_err(node_construction_err)
}

fn build_interface(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    line_index: &LineIndex,
) -> Result<Option<SchemaInterface>, LanguageIndexError> {
    let Some(name) = field_text(node, "name", content) else {
        return Ok(None);
    };
    let source_range = node_range(node, line_index)?;
    SchemaInterface::new(
        repo,
        source_path,
        &name,
        source_range,
        Visibility::Public,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_trait(
    node: Node<'_>,
    content: &str,
    repo: &str,
    source_path: &str,
    line_index: &LineIndex,
) -> Result<Option<SchemaTrait>, LanguageIndexError> {
    let Some(name) = field_text(node, "name", content) else {
        return Ok(None);
    };
    let source_range = node_range(node, line_index)?;
    SchemaTrait::new(repo, source_path, &name, source_range, Visibility::Public)
        .map(Some)
        .map_err(node_construction_err)
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn parameter_signatures(
    function_or_method: Node<'_>,
    content: &str,
) -> Vec<ParameterSignature> {
    let mut out = Vec::new();
    let Some(params) = function_or_method.child_by_field_name("parameters") else {
        return out;
    };
    let mut cursor = params.walk();
    for p in params.named_children(&mut cursor) {
        // Parameter kinds in tree-sitter-php include
        // simple_parameter, variadic_parameter, property_promotion.
        if matches!(
            p.kind(),
            "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
        ) {
            // Name is under field "name" or appears as a
            // variable_name child node ($foo).
            let name = field_text(p, "name", content)
                .or_else(|| first_named_child_named(p, "variable_name", content))
                .unwrap_or_else(|| "<unnamed>".to_string());
            let type_str = field_text(p, "type", content).map(Into::into);
            out.push(ParameterSignature {
                name: name.into(),
                type_str,
                default_value: None,
            });
        }
    }
    out
}

fn synthesize_function_signature(
    name: &str,
    parameters: &[ParameterSignature],
) -> String {
    let params_str = parameters
        .iter()
        .map(|p| {
            if let Some(ty) = &p.type_str {
                format!("{} {}", ty, p.name)
            } else {
                p.name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("function {name}({params_str})")
}

fn php_method_visibility(node: Node<'_>, content: &str) -> Visibility {
    // Method visibility lives in a "visibility_modifier" child of
    // the method_declaration. Check for "public" / "protected" /
    // "private" by source-text inspection — simpler than navigating
    // the typed modifier nodes and version-stable.
    let text = node_text(node, content);
    if text.contains("private ") {
        Visibility::Private
    } else if text.contains("protected ") {
        Visibility::Protected
    } else {
        // PHP defaults to public when no visibility keyword is present.
        Visibility::Public
    }
}

fn php_method_is_static(node: Node<'_>, content: &str) -> bool {
    node_text(node, content).contains("static ")
}

fn node_range(
    node: Node<'_>,
    line_index: &LineIndex,
) -> Result<SourceRange, LanguageIndexError> {
    let start = line_index.line_at(node.start_byte());
    let end = line_index.line_at(node.end_byte());
    SourceRange::new(start, end).map_err(|e| {
        LanguageIndexError::NodeConstruction {
            message: format!("source range: {e}"),
        }
    })
}

fn field_text(
    node: Node<'_>,
    field: &str,
    content: &str,
) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|c| c.utf8_text(content.as_bytes()).ok())
        .map(|s| s.to_string())
}

fn first_named_child_text(
    node: Node<'_>,
    content: &str,
) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .next()
        .and_then(|c| c.utf8_text(content.as_bytes()).ok())
        .map(|s| s.to_string())
}

fn first_named_child_named(
    node: Node<'_>,
    kind: &str,
    content: &str,
) -> Option<String> {
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if c.kind() == kind {
            return c.utf8_text(content.as_bytes()).ok().map(|s| s.to_string());
        }
    }
    None
}

fn node_text<'a>(node: Node<'_>, content: &'a str) -> &'a str {
    node.utf8_text(content.as_bytes()).unwrap_or("")
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
