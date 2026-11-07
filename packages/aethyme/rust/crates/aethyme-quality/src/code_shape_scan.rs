//! Syntax-based source measurements for code-shape analysis.
use std::collections::BTreeSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMetrics {
    pub source_lines: usize,
    pub callables: usize,
    pub exports: usize,
    pub internal_callables: usize,
    pub max_callable_lines: usize,
    pub max_cyclomatic_complexity: usize,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn mark_lines(node: Node<'_>, lines: &mut BTreeSet<usize>) {
    if node.kind() == "comment" {
        return;
    }
    if node.child_count() == 0 {
        for line in node.start_position().row..=node.end_position().row {
            lines.insert(line);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        mark_lines(child, lines);
    }
}

fn complexity(node: Node<'_>, source: &str, root: bool) -> usize {
    let kind = node.kind();
    if !root
        && matches!(
            kind,
            "function_declaration"
                | "function_definition"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "lambda"
                | "class_definition"
        )
    {
        return 0;
    }
    let mut count = usize::from(matches!(
        kind,
        "if_statement"
            | "for_statement"
            | "for_in_statement"
            | "while_statement"
            | "do_statement"
            | "elif_clause"
            | "catch_clause"
            | "except_clause"
            | "conditional_expression"
            | "ternary_expression"
            | "case_clause"
            | "for_in_clause"
            | "if_clause"
    ));
    if matches!(kind, "member_expression" | "subscript_expression")
        && children(node)
            .iter()
            .any(|child| child.kind() == "optional_chain")
    {
        count += 1;
    }
    if kind == "case_clause"
        && (text(node, source).trim_start().starts_with("case _:")
            || text(node, source).trim_start().starts_with("case _ if "))
    {
        count = count.saturating_sub(1);
    }
    if matches!(kind, "binary_expression" | "boolean_operator") {
        if let Some(operator) = node.child_by_field_name("operator") {
            count += usize::from(matches!(
                text(operator, source),
                "&&" | "||" | "??" | "and" | "or"
            ));
        }
    }
    count
        + children(node)
            .into_iter()
            .map(|n| complexity(n, source, false))
            .sum::<usize>()
}

fn callable(node: Node<'_>, internal: bool, m: &mut SourceMetrics, source: &str) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    m.callables += 1;
    m.internal_callables += usize::from(internal);
    let mut body_lines = BTreeSet::new();
    mark_lines(body, &mut body_lines);
    let lines = body_lines.len();
    m.max_callable_lines = m.max_callable_lines.max(lines);
    m.max_cyclomatic_complexity = m
        .max_cyclomatic_complexity
        .max(1 + complexity(body, source, true));
}

fn is_private(node: Node<'_>, source: &str, python: bool) -> bool {
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, source))
        .unwrap_or("");
    if python {
        name.starts_with('_') && !(name.starts_with("__") && name.ends_with("__"))
    } else {
        name.starts_with('#')
            || children(node).iter().any(|n| {
                n.kind() == "accessibility_modifier"
                    && matches!(text(*n, source), "private" | "protected")
            })
    }
}

fn class_methods(
    node: Node<'_>,
    source: &str,
    python: bool,
    exported: bool,
    m: &mut SourceMetrics,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    for entry in children(body) {
        let item = if entry.kind() == "decorated_definition" {
            entry.child_by_field_name("definition").unwrap_or(entry)
        } else {
            entry
        };
        if (python && item.kind() == "function_definition")
            || (!python && item.kind() == "method_definition")
        {
            callable(
                item,
                !exported || is_private(item, source, python),
                m,
                source,
            );
        }
    }
}

fn variable_functions(
    node: Node<'_>,
    source: &str,
    exported: bool,
    exported_names: &BTreeSet<String>,
    m: &mut SourceMetrics,
) {
    for variable in children(node)
        .into_iter()
        .filter(|n| n.kind() == "variable_declarator")
    {
        if let Some(value) = variable.child_by_field_name("value") {
            if matches!(
                value.kind(),
                "arrow_function" | "function_expression" | "generator_function"
            ) {
                let name = variable
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or("");
                callable(
                    value,
                    !exported && !exported_names.contains(name),
                    m,
                    source,
                );
            }
        }
    }
}

fn export_count(node: Node<'_>, source: &str) -> usize {
    if let Some(clause) = children(node)
        .into_iter()
        .find(|n| n.kind() == "export_clause")
    {
        return children(clause)
            .into_iter()
            .filter(|n| n.kind() == "export_specifier")
            .count();
    }
    if text(node, source)
        .trim_start()
        .starts_with("export default")
    {
        return 1;
    }
    if let Some(declaration) = node.child_by_field_name("declaration") {
        if matches!(
            declaration.kind(),
            "lexical_declaration" | "variable_declaration"
        ) {
            return children(declaration)
                .into_iter()
                .filter(|n| n.kind() == "variable_declarator")
                .count();
        }
        return 1;
    }
    if children(node).iter().any(|n| n.kind() == "string") {
        return 1;
    }
    0
}

// A static __all__ determines the Python public surface. A dynamic __all__
// deliberately falls back to public declarations, as in the original audit.
fn python_static_names(node: Node<'_>, source: &str) -> Option<BTreeSet<String>> {
    match node.kind() {
        "list" | "tuple" | "set" => {
            let mut names = BTreeSet::new();
            for item in children(node) {
                if item.kind() != "string" {
                    return None;
                }
                let literal = text(item, source);
                if literal.len() < 2 || !matches!(literal.as_bytes()[0], b'\'' | b'"') {
                    return None;
                }
                names.insert(literal[1..literal.len() - 1].to_string());
            }
            Some(names)
        }
        "binary_operator"
            if node
                .child_by_field_name("operator")
                .map(|n| text(n, source))
                == Some("+") =>
        {
            let mut left = python_static_names(node.child_by_field_name("left")?, source)?;
            left.extend(python_static_names(
                node.child_by_field_name("right")?,
                source,
            )?);
            Some(left)
        }
        _ => None,
    }
}

fn python_exports(root: Node<'_>, source: &str) -> Option<BTreeSet<String>> {
    let mut exports = None::<BTreeSet<String>>;
    for statement in children(root) {
        let node = if statement.kind() == "expression_statement" {
            children(statement).into_iter().next().unwrap_or(statement)
        } else {
            statement
        };
        if node.kind() != "assignment" {
            continue;
        }
        let Some(left) = node.child_by_field_name("left") else {
            continue;
        };
        if text(left, source) != "__all__" {
            continue;
        }
        if let Some(names) = node
            .child_by_field_name("right")
            .and_then(|v| python_static_names(v, source))
        {
            exports.get_or_insert_with(BTreeSet::new).extend(names);
        }
    }
    exports
}

/// Parse one supported file. Invalid syntax fails instead of emitting a clean report.
pub fn measure(path: &Path, source: &str) -> Result<SourceMetrics, String> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let python = extension == "py";
    let lang = match extension {
        "py" => tree_sitter_python::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        _ => return Err(format!("unsupported language: {}", path.display())),
    };
    let mut parser = Parser::new();
    parser.set_language(&lang).map_err(|e| e.to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or("parser did not produce a syntax tree")?;
    if tree.root_node().has_error() {
        return Err(format!("could not parse {}", path.display()));
    }
    // Export lists can make declarations public even when they are not declared
    // with the export modifier. Re-exports from other modules do not do so.
    let mut exported_names = BTreeSet::new();
    if !python {
        for entry in children(tree.root_node()) {
            if entry.kind() != "export_statement" || entry.child_by_field_name("source").is_some() {
                continue;
            }
            if let Some(clause) = children(entry)
                .into_iter()
                .find(|n| n.kind() == "export_clause")
            {
                for specifier in children(clause) {
                    if let Some(name) = specifier.child_by_field_name("name") {
                        exported_names.insert(text(name, source).to_string());
                    }
                }
            }
            if let Some(value) = entry.child_by_field_name("value") {
                if value.kind() == "identifier" {
                    exported_names.insert(text(value, source).to_string());
                }
            }
        }
    }
    let python_export_names = if python {
        python_exports(tree.root_node(), source)
    } else {
        None
    };
    let mut lines = BTreeSet::new();
    mark_lines(tree.root_node(), &mut lines);
    let mut m = SourceMetrics {
        source_lines: lines.len(),
        callables: 0,
        exports: python_export_names.as_ref().map_or(0, BTreeSet::len),
        internal_callables: 0,
        max_callable_lines: 0,
        max_cyclomatic_complexity: 0,
    };
    for outer in children(tree.root_node()) {
        let exported = !python && outer.kind() == "export_statement";
        if exported {
            m.exports += export_count(outer, source);
        }
        let item = if exported {
            outer.child_by_field_name("declaration").unwrap_or(outer)
        } else if outer.kind() == "decorated_definition" {
            outer.child_by_field_name("definition").unwrap_or(outer)
        } else {
            outer
        };
        match item.kind() {
            "function_definition" if python => {
                let name = item
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or("");
                let private = python_export_names.as_ref().map_or_else(
                    || is_private(item, source, true),
                    |names| !names.contains(name),
                );
                if python_export_names.is_none() {
                    m.exports += usize::from(!name.starts_with('_'));
                }
                callable(item, private, &mut m, source);
            }
            "class_definition" if python => {
                m.exports += usize::from(!is_private(item, source, true));
                class_methods(item, source, true, !is_private(item, source, true), &mut m);
            }
            "function_declaration" | "generator_function_declaration" if !python => {
                let name = item
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or("");
                callable(
                    item,
                    !exported && !exported_names.contains(name),
                    &mut m,
                    source,
                )
            }
            "class_declaration" | "abstract_class_declaration" if !python => {
                let name = item
                    .child_by_field_name("name")
                    .map(|n| text(n, source))
                    .unwrap_or("");
                class_methods(
                    item,
                    source,
                    false,
                    exported || exported_names.contains(name),
                    &mut m,
                )
            }
            "lexical_declaration" | "variable_declaration" if !python => {
                variable_functions(item, source, exported, &exported_names, &mut m)
            }
            _ => {}
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typescript_counts_exports_functions_and_complexity() {
        let source = "// comment\nexport function run(a: boolean) {\n  if (a) return 1;\n  return 0;\n}\nconst helper = () => 1;\n";
        let result = measure(Path::new("src/test.ts"), source).unwrap();
        assert_eq!(result.source_lines, 5);
        assert_eq!(result.callables, 2);
        assert_eq!(result.exports, 1);
        assert_eq!(result.internal_callables, 1);
        assert_eq!(result.max_cyclomatic_complexity, 2);
    }
    #[test]
    fn python_counts_methods_and_reports_parse_failures() {
        let source =
            "def public():\n    return 1\n\nclass A:\n    def _internal(self):\n        return 2\n";
        let result = measure(Path::new("x.py"), source).unwrap();
        assert_eq!(
            (result.callables, result.exports, result.internal_callables),
            (2, 2, 1)
        );
        assert!(measure(Path::new("x.py"), "def bad(:").is_err());
    }
    #[test]
    fn export_lists_and_private_classes_are_accounted_for() {
        let source = "function visible() {}\nexport { visible };\nclass Hidden { run() {} }\n";
        let result = measure(Path::new("x.ts"), source).unwrap();
        assert_eq!(
            (result.callables, result.exports, result.internal_callables),
            (2, 1, 1)
        );
    }

    #[test]
    fn body_size_excludes_blank_lines_and_comments() {
        let source = "export function run() {\n  // explanation\n\n  return 1;\n}\n";
        let result = measure(Path::new("x.ts"), source).unwrap();
        assert_eq!(result.max_callable_lines, 3);
    }

    #[test]
    fn python_static_all_controls_exports_and_visibility() {
        let source =
            "__all__ = ['exposed']\ndef exposed():\n    return 1\ndef hidden():\n    return 2\n";
        let result = measure(Path::new("x.py"), source).unwrap();
        assert_eq!((result.exports, result.internal_callables), (1, 1));
    }
    #[test]
    fn python_comprehensions_and_match_wildcards_count_decisions() {
        let source = "def f(values):\n    xs = [x for x in values if x]\n    match xs:\n        case [x]: return x\n        case _: return None\n";
        let result = measure(Path::new("x.py"), source).unwrap();
        assert_eq!(result.max_cyclomatic_complexity, 4);
    }
}
