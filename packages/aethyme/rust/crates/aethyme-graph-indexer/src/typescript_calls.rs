//! TypeScript and JavaScript call-site extraction.
//!
//! Calls are recorded against the nearest indexed top-level function or
//! class method. The linker resolves imported bindings and unambiguous
//! same-file names; calls through inferred receiver types are deliberately
//! left unresolved rather than guessed.

use std::collections::BTreeMap;

use oxc_ast::ast::{
    ArrowFunctionExpression, CallExpression, Class, Expression, Function, FunctionBody,
    NewExpression,
};
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

use aethyme_graph_schema::{
    Confidence, Edge, EdgeAttributes, EdgeSite, Node, NodeId, Source, UnresolvedSymbol,
};

use crate::language::{LanguageIndexError, LineIndex};

/// Collect direct and constructor call sites in one indexed callable body.
///
/// Nested functions, arrows, and classes own their own execution context;
/// their calls must not be attributed to the enclosing callable.
pub(super) fn collect_calls(
    body: &FunctionBody,
    line_index: &LineIndex,
) -> BTreeMap<String, Vec<EdgeSite>> {
    let mut collector = CallCollector {
        line_index,
        calls: BTreeMap::new(),
    };
    for statement in &body.statements {
        collector.visit_statement(statement);
    }
    collector.calls
}

struct CallCollector<'line> {
    line_index: &'line LineIndex,
    calls: BTreeMap<String, Vec<EdgeSite>>,
}

impl<'a> Visit<'a> for CallCollector<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Some(target) = callee_name(&call.callee) {
            self.record(target, call.span.start as usize, "direct");
        }
        walk::walk_call_expression(self, call);
    }

    fn visit_new_expression(&mut self, call: &NewExpression<'a>) {
        if let Some(target) = callee_name(&call.callee) {
            self.record(target, call.span.start as usize, "constructor");
        }
        walk::walk_new_expression(self, call);
    }

    fn visit_function(&mut self, _function: &Function<'a>, _flags: ScopeFlags) {}

    fn visit_arrow_function_expression(&mut self, _function: &ArrowFunctionExpression<'a>) {}

    fn visit_class(&mut self, _class: &Class<'a>) {}
}

impl CallCollector<'_> {
    fn record(&mut self, target: String, byte_offset: usize, kind_tag: &str) {
        let line = self.line_index.line_at(byte_offset);
        self.calls.entry(target).or_default().push(EdgeSite {
            line,
            is_in_branch: false,
            is_in_loop: false,
            kind_tag: kind_tag.into(),
        });
    }
}

/// Reduce an identifier or static member chain to the name understood by
/// the linker. Computed properties and instance-relative roots do not
/// identify a repository symbol without type/scope analysis.
fn callee_name(expression: &Expression<'_>) -> Option<String> {
    match expression.without_parentheses() {
        Expression::Identifier(identifier) => {
            let name = identifier.name.as_str();
            (!matches!(name, "this" | "super")).then(|| name.to_string())
        }
        Expression::StaticMemberExpression(member) => {
            let base = callee_name(&member.object)?;
            Some(format!("{base}.{}", member.property.name.as_str()))
        }
        _ => None,
    }
}

/// Emit one unresolved placeholder and Calls edge per syntactic target.
pub(super) fn emit_call_placeholders(
    repo: &str,
    source_path: &str,
    caller_id: &NodeId,
    calls: BTreeMap<String, Vec<EdgeSite>>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Result<(), LanguageIndexError> {
    for (target, sites) in calls {
        let placeholder =
            UnresolvedSymbol::new(repo, source_path, &target, None, caller_id.clone()).map_err(
                |error| LanguageIndexError::NodeConstruction {
                    message: error.to_string(),
                },
            )?;
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
