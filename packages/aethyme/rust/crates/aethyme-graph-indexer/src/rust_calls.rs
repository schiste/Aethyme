//! Rust call-site extraction.
//!
//! Emits one `Calls` edge from a callable body to an
//! `UnresolvedSymbol` placeholder per syntactic callee, mirroring the
//! Python indexer. The generic linker later rewrites a placeholder to a
//! concrete node when the repository provides enough unambiguous
//! context; an unresolvable placeholder stays in the graph as an
//! `UnresolvedSymbol` rather than being dropped.
//!
//! Scope and deliberate omissions:
//!
//! - Path callees (`foo()`, `foo::bar()`, `self::helper()`) are
//!   extracted. `self`/`super`/`Self` roots are skipped: they name the
//!   enclosing item, not a repository symbol, and a placeholder for them
//!   would be noise the linker can never resolve.
//! - Method calls (`receiver.method()`) are **not** extracted.
//!   Resolving them needs the receiver's inferred type, which is a
//!   type-inference problem this syntax-only indexer does not attempt.
//!   Emitting `method` as a bare placeholder would instead let the
//!   linker's name-only fallback bind it to an unrelated same-named
//!   symbol elsewhere in the repository — a confident wrong edge is
//!   worse than an absent one.
//! - Macro invocations (`println!(...)`, `vec![...]`) are not calls
//!   against indexed symbols and are skipped.

use std::collections::BTreeMap;

use ra_ap_syntax::{AstNode, SyntaxKind, ast};

use aethyme_graph_schema::{
    Confidence, Edge, EdgeAttributes, EdgeSite, Node, NodeId, Source, UnresolvedSymbol,
};

use crate::language::LanguageIndexError;

/// Roots that refer to the enclosing item or a language intrinsic rather
/// than a repository symbol.
const NON_REFERENCE_ROOTS: &[&str] = &["self", "super", "Self", "crate", "std", "core", "alloc"];

/// Prelude constructors and intrinsics that are syntactically
/// indistinguishable from a bare path call (`Ok(x)`, `drop(x)`) but are
/// defined in `core`/`std`, never in the repository under test. Without
/// this filter every `Result`- or `Option`-returning Rust function emits
/// placeholders that can only fail to resolve, burying real call sites
/// in noise.
///
/// Deliberately a short, explicit list of names that cannot plausibly be
/// a repository-level call, rather than a heuristic on identifier shape:
/// guessing from casing or arity would also drop genuine calls, which is
/// the failure mode this module exists to avoid. Only single-segment
/// paths are matched, so a qualified `Foo::new()` is never filtered.
const PRELUDE_CALLS: &[&str] = &[
    // `Result` / `Option` constructors, by far the most frequent.
    "Ok", "Err", "Some", "None", // Core intrinsics.
    "drop", "todo", "unimplemented", "unreachable", "panic",
];

/// Collect call targets in `body`, keyed by the callee name the linker
/// will attempt to resolve, with every source site for that callee.
pub(super) fn collect_calls(
    body: &ast::BlockExpr,
    line_at: impl Fn(usize) -> u32,
) -> BTreeMap<String, Vec<EdgeSite>> {
    let mut calls: BTreeMap<String, Vec<EdgeSite>> = BTreeMap::new();
    for element in body.syntax().descendants() {
        if element.kind() != SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(call) = ast::CallExpr::cast(element) else {
            continue;
        };
        let Some(name) = callee_name(call.expr()) else {
            continue;
        };
        let line = line_at(call.syntax().text_range().start().into());
        calls.entry(name).or_default().push(EdgeSite {
            line,
            is_in_branch: false,
            is_in_loop: false,
            kind_tag: "direct".into(),
        });
    }
    calls
}

/// Reduce a call's callee expression to the dotted name the linker
/// resolves, or `None` when the callee is not a nameable reference.
fn callee_name(expr: Option<ast::Expr>) -> Option<String> {
    match expr? {
        // `foo()` / `foo::bar()` / `crate::foo::bar()`
        ast::Expr::PathExpr(path_expr) => path_expr
            .path()
            .and_then(|path| path.syntax().text().to_string().into())
            .map(|text| text.to_string()),
        // `Type::method()` is a call on a path to an associated
        // function; `Foo::new()` keeps the full path so the linker can
        // use the module-qualified fallback.
        ast::Expr::FieldExpr(field) => {
            let base = callee_name(field.expr())?;
            let field_name = field.name_ref()?;
            Some(format!("{base}.{}", field_name.text()))
        }
        _ => None,
    }
}

/// Normalize a `::`-separated Rust path into the dotted form the
/// linker expects, dropping roots that never denote a repository symbol.
///
/// `crate::store::helper` → `store.helper`
/// `self::helper` → `None`
fn normalize_rust_path(raw: &str) -> Option<String> {
    let segments: Vec<&str> = raw
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return None;
    }
    if NON_REFERENCE_ROOTS.contains(&segments[0]) {
        return None;
    }
    // A path with an unresolved glob cannot name a concrete symbol.
    if segments.contains(&"*") {
        return None;
    }
    // Prelude filter applies only to bare calls: a qualified
    // `mymod::Ok()` names a repository symbol even if the last segment
    // collides with a prelude name.
    if segments.len() == 1 && PRELUDE_CALLS.contains(&segments[0]) {
        return None;
    }
    Some(segments.join("."))
}

/// Emit one placeholder plus `Calls` edge per collected callee.
pub(super) fn emit_call_placeholders(
    repo: &str,
    source_path: &str,
    caller_id: &NodeId,
    calls: BTreeMap<String, Vec<EdgeSite>>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    for (target_name, sites) in calls {
        let Some(normalized) = normalize_rust_path(&target_name) else {
            continue;
        };
        let placeholder =
            UnresolvedSymbol::new(repo, source_path, &normalized, None, caller_id.clone())
                .map_err(|error| LanguageIndexError::NodeConstruction {
                    message: error.to_string(),
                })?;
        let placeholder_id = placeholder.id().clone();
        nodes.push(Node::UnresolvedSymbol(placeholder));
        edges.push(
            Edge::new(
                caller_id.clone(),
                placeholder_id,
                EdgeAttributes::Calls,
                Source::Code,
                Confidence::from_milli(850).expect("850 is within confidence range"),
            )
            .with_sites(sites),
        );
    }
    Ok(())
}