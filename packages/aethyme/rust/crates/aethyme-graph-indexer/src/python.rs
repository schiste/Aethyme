//! Python indexer (Phase 3.3). Uses ruff's Python parser to extract
//! Function, Method, Class, and GlobalVariable nodes plus their
//! structural edges (Contains / Defines).
//!
//! Scope of the v1 indexer:
//!
//! - **Extracted node kinds:** Function (top-level), Method (inside
//!   Class), Class, GlobalVariable (module-level Assign / AnnAssign),
//!   UnresolvedSymbol (import placeholders — Phase 4.4).
//! - **Extracted edges:** Contains (file → top-level symbol),
//!   Defines (class → method), Imports (file → UnresolvedSymbol
//!   per imported name).
//! - **NOT yet extracted:** call-graph edges (Calls), nested-function
//!   definitions inside function bodies (we extract only the outermost
//!   function, plus methods inside classes). These land in later
//!   commits as the indexer matures.
//!
//! Cross-file resolution model (Phase 4.4): for each `import` or
//! `from … import …` statement we emit one `UnresolvedSymbol` per
//! imported name plus a corresponding `Imports` edge from the file
//! node. The `UnresolvedSymbol` carries the binding name (`np` for
//! `import numpy as np`); the edge's `import_path` carries the
//! dotted source path (`numpy`). A future linker pass will look up
//! each `UnresolvedSymbol` in the global symbol index and rewrite
//! edge targets to point at concrete nodes (resolved cross-file
//! edges) — that pass needs no Python-specific code, only the
//! placeholders this commit emits.
//!
//! The parser-error path returns `LanguageIndexError::Parse` —
//! callers should log the diagnostic and continue with the
//! filesystem-only File node for that file (better-than-nothing
//! degradation).

use ruff_python_ast::{
    ModModule, Stmt, StmtAnnAssign, StmtAssign, StmtClassDef, StmtFunctionDef, StmtImport,
    StmtImportFrom,
};
use ruff_python_parser::parse_module;
use ruff_text_size::TextRange;

use aethyme_graph_schema::{
    Callable, Class, Confidence, Edge, EdgeAttributes, Function, GlobalVariable, Method, Node,
    NodeId, NodeKind, ParameterSignature, Source, SourceRange, UnresolvedSymbol, Visibility,
};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;
use crate::language::{LanguageIndexError, LanguageIndexResult, LanguageIndexer, LineIndex};

/// Indexer for `.py` / `.pyi` files via ruff's Python parser.
pub struct PythonIndexer;

impl PythonIndexer {
    pub fn new() -> Self {
        PythonIndexer
    }
}

impl Default for PythonIndexer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageIndexer for PythonIndexer {
    fn language(&self) -> &'static str {
        "python"
    }

    fn index_file(
        &self,
        ctx: &IndexerContext,
        indexed_file: &IndexedFile,
        content: &str,
    ) -> Result<LanguageIndexResult, LanguageIndexError> {
        let parsed = parse_module(content).map_err(|e| LanguageIndexError::Parse {
            message: format!("{e:?}"),
        })?;
        let module: &ModModule = parsed.syntax();
        let line_index = LineIndex::new(content);

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let file_id = indexed_file.top_node.id().clone();
        let repo = ctx.repo_name();
        let source_path = &*indexed_file.source_path;

        for stmt in &module.body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let function = build_function(
                        repo,
                        source_path,
                        f,
                        &line_index,
                        /* is_top_level */ true,
                    )?;
                    let function_id = function.id().clone();
                    nodes.push(Node::Function(function));
                    edges.push(structural_edge(
                        EdgeAttributes::Contains,
                        file_id.clone(),
                        function_id,
                    ));
                }
                Stmt::ClassDef(c) => {
                    let class = build_class(repo, source_path, c, &line_index)?;
                    let class_id = class.id().clone();
                    nodes.push(Node::Class(class));
                    edges.push(structural_edge(
                        EdgeAttributes::Contains,
                        file_id.clone(),
                        class_id.clone(),
                    ));

                    // Walk class body for methods.
                    for member in &c.body {
                        if let Stmt::FunctionDef(m) = member {
                            let method =
                                build_method(repo, source_path, m, class_id.clone(), &line_index)?;
                            let method_id = method.id().clone();
                            nodes.push(Node::Method(method));
                            edges.push(structural_edge(
                                EdgeAttributes::Defines,
                                class_id.clone(),
                                method_id,
                            ));
                        }
                    }
                }
                Stmt::Assign(a) => {
                    if let Some(global) =
                        build_global_from_assign(repo, source_path, a, &line_index)?
                    {
                        let global_id = global.id().clone();
                        nodes.push(Node::GlobalVariable(global));
                        edges.push(structural_edge(
                            EdgeAttributes::Contains,
                            file_id.clone(),
                            global_id,
                        ));
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Some(global) =
                        build_global_from_ann_assign(repo, source_path, a, &line_index)?
                    {
                        let global_id = global.id().clone();
                        nodes.push(Node::GlobalVariable(global));
                        edges.push(structural_edge(
                            EdgeAttributes::Contains,
                            file_id.clone(),
                            global_id,
                        ));
                    }
                }
                Stmt::Import(imp) => {
                    emit_import_placeholders(
                        repo,
                        source_path,
                        &file_id,
                        imp,
                        &mut nodes,
                        &mut edges,
                    )?;
                }
                Stmt::ImportFrom(imp) => {
                    emit_import_from_placeholders(
                        repo,
                        source_path,
                        &file_id,
                        imp,
                        &mut nodes,
                        &mut edges,
                    )?;
                }
                _ => {} // ignore other top-level statements in v1
            }
        }

        Ok(LanguageIndexResult {
            additional_nodes: nodes,
            additional_edges: edges,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn build_function(
    repo: &str,
    source_path: &str,
    f: &StmtFunctionDef,
    line_index: &LineIndex,
    is_top_level: bool,
) -> Result<Function, LanguageIndexError> {
    let name = f.name.as_str();
    let parameters = parameter_signatures(&f.parameters);
    let signature = synthesize_function_signature(name, &parameters, f.is_async);
    let source_range = range_to_source_range(f.range, line_index)?;
    let visibility = visibility_from_name(name);
    Function::new(
        repo,
        source_path,
        name,
        &signature,
        parameters,
        None, // return_type extraction deferred
        source_range,
        visibility,
        is_top_level,
    )
    .map_err(|e| LanguageIndexError::NodeConstruction {
        message: e.to_string(),
    })
}

fn build_method(
    repo: &str,
    source_path: &str,
    m: &StmtFunctionDef,
    receiver_type: NodeId,
    line_index: &LineIndex,
) -> Result<Method, LanguageIndexError> {
    let name = m.name.as_str();
    let parameters = parameter_signatures(&m.parameters);
    let signature = synthesize_function_signature(name, &parameters, m.is_async);
    let source_range = range_to_source_range(m.range, line_index)?;
    let visibility = visibility_from_name(name);
    let is_static = m
        .decorator_list
        .iter()
        .any(|d| decorator_name(d) == Some("staticmethod"));
    // Heuristic: Python doesn't have explicit `virtual` like Java, so
    // we conservatively mark all methods as virtual (subclass can
    // override). is_static overrides this.
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
    .map_err(|e| LanguageIndexError::NodeConstruction {
        message: e.to_string(),
    })
}

fn build_class(
    repo: &str,
    source_path: &str,
    c: &StmtClassDef,
    line_index: &LineIndex,
) -> Result<Class, LanguageIndexError> {
    let name = c.name.as_str();
    let source_range = range_to_source_range(c.range, line_index)?;
    let visibility = visibility_from_name(name);
    Class::new(repo, source_path, name, source_range, visibility).map_err(|e| {
        LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        }
    })
}

fn build_global_from_assign(
    repo: &str,
    source_path: &str,
    a: &StmtAssign,
    line_index: &LineIndex,
) -> Result<Option<GlobalVariable>, LanguageIndexError> {
    // Only handle simple `NAME = ...` patterns. Tuple unpacking, attr
    // assignment, etc. are skipped in v1.
    let Some(name) = a.targets.first().and_then(expr_name) else {
        return Ok(None);
    };
    let source_range = range_to_source_range(a.range, line_index)?;
    GlobalVariable::new(repo, source_path, name, None, source_range)
        .map(Some)
        .map_err(|e| LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        })
}

fn build_global_from_ann_assign(
    repo: &str,
    source_path: &str,
    a: &StmtAnnAssign,
    line_index: &LineIndex,
) -> Result<Option<GlobalVariable>, LanguageIndexError> {
    let Some(name) = expr_name(&a.target) else {
        return Ok(None);
    };
    let source_range = range_to_source_range(a.range, line_index)?;
    GlobalVariable::new(repo, source_path, name, None, source_range)
        .map(Some)
        .map_err(|e| LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        })
}

fn expr_name(expr: &ruff_python_ast::Expr) -> Option<&str> {
    if let ruff_python_ast::Expr::Name(n) = expr {
        Some(n.id.as_str())
    } else {
        None
    }
}

fn decorator_name(d: &ruff_python_ast::Decorator) -> Option<&str> {
    // Best-effort: handle `@staticmethod` (bare name) — common cases.
    // Attribute-decorators like @some.module.thing are not unwrapped.
    if let ruff_python_ast::Expr::Name(n) = &d.expression {
        Some(n.id.as_str())
    } else {
        None
    }
}

fn parameter_signatures(params: &ruff_python_ast::Parameters) -> Vec<ParameterSignature> {
    let mut out = Vec::new();
    for p in params.iter_non_variadic_params() {
        let name: &str = p.parameter.name.as_str();
        let type_str = p.parameter.annotation.as_ref().map(|_| {
            // We don't render expressions back to source in v1.
            // Carry a placeholder so consumers know an annotation
            // exists. Subsequent commits can use source-text to
            // reproduce the annotation faithfully.
            "<annotated>".into()
        });
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
    let prefix = if is_async { "async def" } else { "def" };
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

fn range_to_source_range(
    range: TextRange,
    line_index: &LineIndex,
) -> Result<SourceRange, LanguageIndexError> {
    let start = line_index.line_at(u32::from(range.start()) as usize);
    let end = line_index.line_at(u32::from(range.end()) as usize);
    SourceRange::new(start, end).map_err(|e| LanguageIndexError::NodeConstruction {
        message: format!("source range: {e}"),
    })
}

fn visibility_from_name(name: &str) -> Visibility {
    // Python convention:
    // - `__name__` (dunder) = special builtin-style; treat as Public
    // - `_name` (single leading underscore) = "internal by
    //   convention" → Private in our schema
    // - everything else = Public
    if name.starts_with("__") && name.ends_with("__") {
        Visibility::Public
    } else if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

fn structural_edge(attributes: EdgeAttributes, src: NodeId, dst: NodeId) -> Edge {
    Edge::new(src, dst, attributes, Source::Structure, Confidence::FULL)
}

// ─── Imports (Phase 4.4) ─────────────────────────────────────────────
//
// For each `import X[.Y]` form we emit one UnresolvedSymbol per top-
// level imported name (or its alias) plus an Imports edge from the
// file node. The edge's `import_path` carries the dotted source-side
// path; the `UnresolvedSymbol.name` carries the *binding* name that
// other code in this file will reference. Examples:
//   `import numpy`            → name="numpy",  import_path="numpy"
//   `import numpy as np`      → name="np",     import_path="numpy"
//   `import foo.bar`          → name="foo",    import_path="foo.bar"
//   `import foo.bar as fb`    → name="fb",     import_path="foo.bar"
//
// Why first-segment for `import foo.bar`? Python's runtime binds only
// the top-level package name into the local namespace: code that
// later writes `foo.bar.thing()` looks up `foo`, then attribute-
// accesses `bar` and `thing` on it. The placeholder must therefore
// be named `foo` so the future linker pass can connect those
// references back to the import statement.
//
// `is_namespace=true` for plain `import` statements because the whole
// module is brought into scope. `expected_kind=Module` is set because
// the right-hand side of a plain `import` is unambiguously a module.

fn emit_import_placeholders(
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    imp: &StmtImport,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    for alias in &imp.names {
        let import_path = alias.name.as_str();
        // When unaliased, the binding is the *first* segment of the
        // dotted name (Python's runtime semantics, not the full path).
        let binding = match alias.asname.as_ref() {
            Some(n) => n.as_str(),
            None => import_path.split('.').next().unwrap_or(import_path),
        };
        let placeholder = UnresolvedSymbol::new(
            repo,
            source_path,
            binding,
            Some(NodeKind::Module),
            file_id.clone(),
        )
        .map_err(|e| LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        })?;
        let placeholder_id = placeholder.id().clone();
        nodes.push(Node::UnresolvedSymbol(placeholder));
        edges.push(Edge::new(
            file_id.clone(),
            placeholder_id,
            EdgeAttributes::Imports {
                import_path: import_path.into(),
                is_namespace: true,
                is_default: false,
                is_named: false,
            },
            Source::Structure,
            Confidence::FULL,
        ));
    }
    Ok(())
}

// For `from M import X[, Y as Z, …]` we emit one placeholder + edge
// per imported name. The edge's `import_path` is the fully qualified
// source path (`M.X`); the placeholder's `name` is the binding (`Z`
// when aliased, else `X`). Relative imports (`from . import x`,
// `from ..pkg import y`) prepend `level` dots to the module path so
// the linker pass can later distinguish `from os import path` from
// `from . import path`.
//
// `from M import *` star-imports are emitted as a single placeholder
// named `*` with `is_namespace=true`. The linker pass treats this as
// a wildcard hint rather than a single resolvable symbol.

fn emit_import_from_placeholders(
    repo: &str,
    source_path: &str,
    file_id: &NodeId,
    imp: &StmtImportFrom,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    let module_part = imp.module.as_ref().map(|m| m.as_str()).unwrap_or("");
    let dots: String = ".".repeat(imp.level as usize);
    for alias in &imp.names {
        let imported = alias.name.as_str();
        let is_star = imported == "*";
        let binding = if is_star {
            "*"
        } else {
            alias
                .asname
                .as_ref()
                .map(|n| n.as_str())
                .unwrap_or(imported)
        };
        let import_path = match (module_part.is_empty(), is_star) {
            (true, true) => format!("{dots}*"),
            (true, false) => format!("{dots}{imported}"),
            (false, true) => format!("{dots}{module_part}.*"),
            (false, false) => format!("{dots}{module_part}.{imported}"),
        };
        let placeholder = UnresolvedSymbol::new(
            repo,
            source_path,
            binding,
            None, // can't infer kind from `from … import …`
            file_id.clone(),
        )
        .map_err(|e| LanguageIndexError::NodeConstruction {
            message: e.to_string(),
        })?;
        let placeholder_id = placeholder.id().clone();
        nodes.push(Node::UnresolvedSymbol(placeholder));
        edges.push(Edge::new(
            file_id.clone(),
            placeholder_id,
            EdgeAttributes::Imports {
                import_path: import_path.into(),
                is_namespace: is_star,
                is_default: false,
                is_named: !is_star,
            },
            Source::Structure,
            Confidence::FULL,
        ));
    }
    Ok(())
}
