//! TypeScript / JavaScript indexer (Phase 3.5) via `oxc_parser`.
//!
//! Handles both `.ts`/`.tsx` and `.js`/`.jsx`/`.cjs`/`.mjs` files.
//! Picked oxc over SWC and tree-sitter-typescript per the perf
//! discussion: ~2-3× faster than SWC, ~6-10× faster than
//! tree-sitter; pure-Rust; production-quality (used by oxlint).
//!
//! Extracted node kinds:
//! - Function (top-level FunctionDeclaration)
//! - Class (top-level ClassDeclaration) + Method (each
//!   ClassElement::MethodDefinition inside it)
//! - Interface (TSInterfaceDeclaration)
//! - TypeAlias (TSTypeAliasDeclaration)
//! - Enum (TSEnumDeclaration)
//! - GlobalVariable (for each VariableDeclarator with a simple
//!   identifier binding pattern at module scope)
//!
//! Extracted edges:
//! - Contains (file → top-level symbol)
//! - Defines (class → method)
//! - Imports (file → UnresolvedSymbol placeholder per imported
//!   binding name; Phase 4.6 stage 1 — the linker resolves these
//!   to concrete nodes when both ends of the import live in repo)
//!
//! Deferred for later commits: Calls edges (need scope
//! resolution), arrow-function expressions assigned to variables,
//! nested function declarations.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Class, ClassElement, Expression, Function as OxcFunction,
    ImportDeclaration, ImportDeclarationSpecifier, ModuleExportName, Program,
    PropertyKey, Statement, TSEnumDeclaration, TSInterfaceDeclaration,
    TSTypeAliasDeclaration,
};
use oxc_parser::Parser;
use oxc_span::{SourceType as OxcSourceType, Span};

use aethyme_graph_schema::{
    Callable, Class as SchemaClass, Confidence, Edge, EdgeAttributes,
    Enum as SchemaEnum, Function as SchemaFunction, GlobalVariable,
    Interface as SchemaInterface, Method, Node, NodeId, NodeKind,
    ParameterSignature, Source, SourceRange, TypeAlias, UnresolvedSymbol,
    Visibility,
};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;
use crate::language::{
    LanguageIndexError, LanguageIndexResult, LanguageIndexer, LineIndex,
};

/// Indexer for `.ts`/`.tsx`/`.js`/`.jsx`/`.cjs`/`.mjs` files via oxc.
///
/// One indexer instance is registered under the canonical language
/// tag "typescript" (because the filesystem walker normalizes both
/// JS and TS extensions to that tag — see `language_map.rs`).
pub struct TypeScriptIndexer;

impl TypeScriptIndexer {
    pub fn new() -> Self {
        TypeScriptIndexer
    }
}

impl Default for TypeScriptIndexer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageIndexer for TypeScriptIndexer {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn index_file(
        &self,
        ctx: &IndexerContext,
        indexed_file: &IndexedFile,
        content: &str,
    ) -> Result<LanguageIndexResult, LanguageIndexError> {
        // oxc parser uses an arena allocator. The Program references
        // memory inside the arena, so we have to do all our work
        // within the same scope where the Allocator is alive.
        let allocator = Allocator::default();
        let source_type = source_type_for(&indexed_file.source_path);
        let parser_return =
            Parser::new(&allocator, content, source_type).parse();

        if !parser_return.errors.is_empty() {
            // Hard parse errors. Even oxc's recoverable cases set
            // panicked=false but populate errors; we only fail loudly
            // when the parser actually panicked (couldn't recover at
            // all). Recoverable errors still produce a usable
            // program, so we proceed in those cases — capturing the
            // best partial AST is better than zero nodes for a file
            // with one syntax error.
            if parser_return.panicked {
                return Err(LanguageIndexError::Parse {
                    message: format_diagnostics(&parser_return.errors),
                });
            }
        }
        let program: &Program = &parser_return.program;
        let line_index = LineIndex::new(content);

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let file_id = indexed_file.top_node.id().clone();
        let repo = ctx.repo_name();
        let source_path = &*indexed_file.source_path;

        for stmt in &program.body {
            walk_top_level_statement(
                stmt,
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

fn walk_top_level_statement(
    stmt: &Statement,
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    line_index: &LineIndex,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    match stmt {
        Statement::FunctionDeclaration(f) => {
            if let Some(node) =
                build_function(repo, source_path, f, line_index, true)?
            {
                let id = node.id().clone();
                nodes.push(Node::Function(node));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    id,
                ));
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(class) = build_class(repo, source_path, c, line_index)?
            {
                let class_id = class.id().clone();
                nodes.push(Node::Class(class));
                edges.push(structural_edge(
                    EdgeAttributes::Contains,
                    file_id.clone(),
                    class_id.clone(),
                ));
                for member in &c.body.body {
                    if let ClassElement::MethodDefinition(m) = member {
                        let name = property_key_name(&m.key);
                        if let Some(name) = name {
                            let method = build_method(
                                repo,
                                source_path,
                                name,
                                &m.value,
                                m.r#static,
                                class_id.clone(),
                                line_index,
                                m.span,
                            )?;
                            let id = method.id().clone();
                            nodes.push(Node::Method(method));
                            edges.push(structural_edge(
                                EdgeAttributes::Defines,
                                class_id.clone(),
                                id,
                            ));
                        }
                    }
                }
            }
        }
        Statement::TSInterfaceDeclaration(i) => {
            let iface = build_interface(repo, source_path, i, line_index)?;
            let id = iface.id().clone();
            nodes.push(Node::Interface(iface));
            edges.push(structural_edge(
                EdgeAttributes::Contains,
                file_id.clone(),
                id,
            ));
        }
        Statement::TSTypeAliasDeclaration(t) => {
            let alias = build_type_alias(repo, source_path, t, line_index)?;
            let id = alias.id().clone();
            nodes.push(Node::TypeAlias(alias));
            edges.push(structural_edge(
                EdgeAttributes::Contains,
                file_id.clone(),
                id,
            ));
        }
        Statement::TSEnumDeclaration(e) => {
            let en = build_enum(repo, source_path, e, line_index)?;
            let id = en.id().clone();
            nodes.push(Node::Enum(en));
            edges.push(structural_edge(
                EdgeAttributes::Contains,
                file_id.clone(),
                id,
            ));
        }
        Statement::VariableDeclaration(v) => {
            for decl in &v.declarations {
                if let BindingPattern::BindingIdentifier(id) = &decl.id {
                    let global = build_global(
                        repo,
                        source_path,
                        id.name.as_str(),
                        decl.span,
                        line_index,
                    )?;
                    let global_id = global.id().clone();
                    nodes.push(Node::GlobalVariable(global));
                    edges.push(structural_edge(
                        EdgeAttributes::Contains,
                        file_id.clone(),
                        global_id,
                    ));
                }
            }
        }
        Statement::ImportDeclaration(imp) => {
            emit_ts_import_placeholders(
                repo, source_path, file_id, imp, nodes, edges,
            )?;
        }
        _ => {} // other statement kinds are ignored at top level in v1
    }
    Ok(())
}

// ─── Builders ────────────────────────────────────────────────────────

fn build_function(
    repo: &str,
    source_path: &str,
    f: &OxcFunction,
    line_index: &LineIndex,
    is_top_level: bool,
) -> Result<Option<SchemaFunction>, LanguageIndexError> {
    // Anonymous functions (no id) at the top level are unusual but
    // legal in TS (e.g. export default function() {}). Skip them in
    // v1 — they don't have an addressable name.
    let Some(id) = &f.id else {
        return Ok(None);
    };
    let name = id.name.as_str();
    let parameters = parameter_signatures_for_function(f);
    let signature =
        synthesize_function_signature(name, &parameters, f.r#async);
    let source_range = span_to_source_range(f.span, line_index)?;
    let visibility = Visibility::Public;
    let function = SchemaFunction::new(
        repo,
        source_path,
        name,
        &signature,
        parameters,
        None,
        source_range,
        visibility,
        is_top_level,
    )
    .map_err(node_construction_err)?;
    Ok(Some(function))
}

// 8 args is over clippy's default 7-arg limit. Allowed here because
// every arg is load-bearing for the Method node's construction
// invariants — collapsing them into a struct just to satisfy the
// lint would obscure the call site without removing any actual
// state.
#[allow(clippy::too_many_arguments)]
fn build_method(
    repo: &str,
    source_path: &str,
    name: &str,
    value: &OxcFunction,
    is_static: bool,
    receiver_type: NodeId,
    line_index: &LineIndex,
    method_span: Span,
) -> Result<Method, LanguageIndexError> {
    let parameters = parameter_signatures_for_function(value);
    let signature =
        synthesize_function_signature(name, &parameters, value.r#async);
    let source_range = span_to_source_range(method_span, line_index)?;
    let visibility = Visibility::Public;
    // TypeScript methods are conventionally overridable in subclasses
    // unless marked otherwise; we keep them virtual unless they're
    // static.
    let is_virtual = !is_static;
    Method::new(
        repo,
        source_path,
        name,
        &signature,
        parameters,
        None,
        source_range,
        visibility,
        receiver_type,
        is_static,
        is_virtual,
    )
    .map_err(node_construction_err)
}

fn build_class(
    repo: &str,
    source_path: &str,
    c: &Class,
    line_index: &LineIndex,
) -> Result<Option<SchemaClass>, LanguageIndexError> {
    let Some(id) = &c.id else {
        return Ok(None); // anonymous class declarations are skipped in v1
    };
    let source_range = span_to_source_range(c.span, line_index)?;
    SchemaClass::new(
        repo,
        source_path,
        id.name.as_str(),
        source_range,
        Visibility::Public,
    )
    .map(Some)
    .map_err(node_construction_err)
}

fn build_interface(
    repo: &str,
    source_path: &str,
    i: &TSInterfaceDeclaration,
    line_index: &LineIndex,
) -> Result<SchemaInterface, LanguageIndexError> {
    let source_range = span_to_source_range(i.span, line_index)?;
    SchemaInterface::new(
        repo,
        source_path,
        i.id.name.as_str(),
        source_range,
        Visibility::Public,
    )
    .map_err(node_construction_err)
}

fn build_type_alias(
    repo: &str,
    source_path: &str,
    t: &TSTypeAliasDeclaration,
    line_index: &LineIndex,
) -> Result<TypeAlias, LanguageIndexError> {
    let source_range = span_to_source_range(t.span, line_index)?;
    // We don't render the type expression back to source in v1.
    // Carry a placeholder so consumers know the alias has a target.
    let target_type = "<type>";
    TypeAlias::new(
        repo,
        source_path,
        t.id.name.as_str(),
        target_type,
        source_range,
        Visibility::Public,
    )
    .map_err(node_construction_err)
}

fn build_enum(
    repo: &str,
    source_path: &str,
    e: &TSEnumDeclaration,
    line_index: &LineIndex,
) -> Result<SchemaEnum, LanguageIndexError> {
    let source_range = span_to_source_range(e.span, line_index)?;
    // Variant extraction deferred to a follow-up commit. v1 emits
    // the Enum node with an empty variants Vec; queries that ask
    // "find all enums" still work; queries that walk variants get
    // an empty list.
    SchemaEnum::new(
        repo,
        source_path,
        e.id.name.as_str(),
        Vec::new(),
        source_range,
        Visibility::Public,
    )
    .map_err(node_construction_err)
}

fn build_global(
    repo: &str,
    source_path: &str,
    name: &str,
    span: Span,
    line_index: &LineIndex,
) -> Result<GlobalVariable, LanguageIndexError> {
    let source_range = span_to_source_range(span, line_index)?;
    GlobalVariable::new(repo, source_path, name, None, source_range)
        .map_err(node_construction_err)
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn property_key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StaticMemberExpression(_) => None,
        _ => match key.as_expression() {
            Some(Expression::StringLiteral(s)) => Some(s.value.as_str()),
            Some(Expression::Identifier(i)) => Some(i.name.as_str()),
            _ => None,
        },
    }
}

fn parameter_signatures_for_function(
    f: &OxcFunction,
) -> Vec<ParameterSignature> {
    let mut out = Vec::new();
    for p in &f.params.items {
        let name = match &p.pattern {
            BindingPattern::BindingIdentifier(id) => id.name.as_str(),
            // Destructuring patterns — emit a placeholder. Faithful
            // reproduction is a future refinement.
            BindingPattern::ObjectPattern(_) => "<destructured>",
            BindingPattern::ArrayPattern(_) => "<destructured>",
            BindingPattern::AssignmentPattern(_) => "<default>",
        };
        // type_annotation lives on the FormalParameter in 0.125,
        // not on the BindingPattern.
        let type_str = p
            .type_annotation
            .as_ref()
            .map(|_| "<annotated>".into());
        out.push(ParameterSignature {
            name: name.into(),
            type_str,
            default_value: None,
        });
    }
    out
}

fn synthesize_function_signature(
    name: &str,
    parameters: &[ParameterSignature],
    is_async: bool,
) -> String {
    let prefix = if is_async { "async function" } else { "function" };
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

fn span_to_source_range(
    span: Span,
    line_index: &LineIndex,
) -> Result<SourceRange, LanguageIndexError> {
    let start = line_index.line_at(span.start as usize);
    let end = line_index.line_at(span.end as usize);
    SourceRange::new(start, end).map_err(|e| {
        LanguageIndexError::NodeConstruction {
            message: format!("source range: {e}"),
        }
    })
}

fn source_type_for(source_path: &str) -> OxcSourceType {
    // Default to a TS module unless the extension says otherwise.
    let lower = source_path.to_ascii_lowercase();
    let mut st = OxcSourceType::default().with_typescript(true);
    if lower.ends_with(".tsx") || lower.ends_with(".jsx") {
        st = st.with_jsx(true);
    }
    if lower.ends_with(".cjs") {
        st = st.with_script(true);
    }
    st
}

fn format_diagnostics<D: std::fmt::Display>(diagnostics: &[D]) -> String {
    // Concatenate a few diagnostic messages. We accept any Display
    // type so we don't take a dep on oxc_diagnostics just to name
    // the concrete type — OxcDiagnostic implements Display and
    // that's all we need here.
    diagnostics
        .iter()
        .take(3)
        .map(|d| format!("{d}"))
        .collect::<Vec<_>>()
        .join("; ")
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

// ─── Imports (Phase 4.6 stage 1) ─────────────────────────────────────
//
// For each ES module import declaration we emit one
// `UnresolvedSymbol` placeholder per imported binding name plus a
// corresponding `Imports` edge from the file node. The linker pass
// (Phase 4.5) resolves these when both ends of the import live in
// this repo.
//
//   `import "side-effect-module";`       → name="side-effect-module",
//                                          import_path="side-effect-module",
//                                          is_namespace=true (one placeholder)
//   `import foo from "react";`           → name="foo",
//                                          import_path="react",
//                                          is_default=true
//   `import * as ns from "./util";`      → name="ns",
//                                          import_path="./util",
//                                          is_namespace=true
//   `import { x } from "./util";`        → name="x",
//                                          import_path="./util::x",
//                                          is_named=true
//   `import { x as y } from "./util";`   → name="y",
//                                          import_path="./util::x",
//                                          is_named=true
//
// The `import_path` carries the module specifier joined to the
// imported name by `::` rather than `.`. `::` is illegal in both
// JS identifiers and ES module specifiers, so a future linker that
// splits `import_path` to recover (module, symbol) has an
// unambiguous separator. The `.` separator that Python uses is
// fine for Python's dotted module paths but would conflate with
// scoped package names and identifiers containing dots in JS
// land.
//
// TypeScript's path resolution (`./x` → `<importing-dir>/x.ts`,
// `@scope/pkg` → external) is the linker's responsibility;
// today's linker does literal lookup and will leave TS imports
// unresolved until that work lands. The stage-1 placeholders are
// still valuable: they pin the *intent* of the import statement
// into the graph for future resolution.

fn emit_ts_import_placeholders(
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    imp: &ImportDeclaration,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    let source = imp.source.value.as_str();
    let Some(specifiers) = &imp.specifiers else {
        // `import "side-effect-module";` — no local bindings; the
        // file is pulling the module in for side effects only. We
        // emit a single placeholder named after the module so the
        // import statement is still discoverable in the graph.
        let placeholder = UnresolvedSymbol::new(
            repo,
            source_path,
            source,
            Some(NodeKind::Module),
            file_id.clone(),
        )
        .map_err(node_construction_err)?;
        let id = placeholder.id().clone();
        nodes.push(Node::UnresolvedSymbol(placeholder));
        edges.push(Edge::new(
            file_id.clone(),
            id,
            EdgeAttributes::Imports {
                import_path: source.into(),
                is_namespace: true,
                is_default: false,
                is_named: false,
            },
            Source::Structure,
            Confidence::FULL,
        ));
        return Ok(());
    };

    for spec in specifiers {
        let (binding, expected_kind, is_namespace, is_default, is_named, path_suffix) =
            match spec {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => (
                    s.local.name.as_str().to_string(),
                    None, // default export can be anything
                    false,
                    true,
                    false,
                    None,
                ),
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => (
                    s.local.name.as_str().to_string(),
                    Some(NodeKind::Module),
                    true,
                    false,
                    false,
                    None,
                ),
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    let imported_name = match &s.imported {
                        ModuleExportName::IdentifierName(n) => n.name.as_str(),
                        ModuleExportName::IdentifierReference(n) => n.name.as_str(),
                        ModuleExportName::StringLiteral(sl) => sl.value.as_str(),
                    };
                    (
                        s.local.name.as_str().to_string(),
                        None,
                        false,
                        false,
                        true,
                        Some(imported_name.to_string()),
                    )
                }
            };

        let import_path = match &path_suffix {
            Some(name) => format!("{source}::{name}"),
            None => source.to_string(),
        };
        let placeholder = UnresolvedSymbol::new(
            repo,
            source_path,
            &binding,
            expected_kind,
            file_id.clone(),
        )
        .map_err(node_construction_err)?;
        let id = placeholder.id().clone();
        nodes.push(Node::UnresolvedSymbol(placeholder));
        edges.push(Edge::new(
            file_id.clone(),
            id,
            EdgeAttributes::Imports {
                import_path: import_path.into(),
                is_namespace,
                is_default,
                is_named,
            },
            Source::Structure,
            Confidence::FULL,
        ));
    }
    Ok(())
}
