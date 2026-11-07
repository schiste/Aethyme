//! Conservative runtime dependency measurements for code-shape analysis.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use tree_sitter::{Node, Parser};

#[derive(Debug, Default)]
pub struct Graph {
    pub edges: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    pub incomplete: Vec<String>,
    fan_in: BTreeMap<PathBuf, usize>,
    cycle_members: BTreeSet<PathBuf>,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}
fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}
fn unquote(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[bytes.len() - 1] {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => (),
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}
fn resolve(base: &Path, known: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    let base = normalize(base)?;
    let mut candidates = vec![base.clone()];
    if matches!(
        base.extension().and_then(|s| s.to_str()),
        Some("js" | "jsx" | "mjs" | "cjs")
    ) {
        for ext in ["ts", "tsx"] {
            candidates.push(base.with_extension(ext));
        }
    }
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "py"] {
        let mut p = base.as_os_str().to_os_string();
        p.push(format!(".{ext}"));
        candidates.push(PathBuf::from(p));
        candidates.push(base.join(format!("index.{ext}")));
    }
    candidates.push(base.join("__init__.py"));
    candidates.into_iter().find(|p| known.contains(p))
}

fn add_js(
    node: Node<'_>,
    source: &str,
    file: &Path,
    known: &BTreeSet<PathBuf>,
    aliases: &BTreeMap<String, String>,
    edges: &mut BTreeSet<PathBuf>,
    incomplete: &mut Vec<String>,
) {
    let kind = node.kind();
    let mut module = None;
    if matches!(kind, "import_statement" | "export_statement") {
        let raw = text(node, source).trim_start();
        if raw.starts_with("import type ") || raw.starts_with("export type ") {
            return;
        }
        module = node.child_by_field_name("source");
        // A list made entirely of type specifiers does not add a runtime edge.
        if module.is_some()
            && (raw.starts_with("import {") || raw.starts_with("export {"))
            && let Some((_, rest)) = raw.split_once('{')
            && let Some((list, _)) = rest.split_once('}')
            && !list.trim().is_empty()
            && list.split(',').all(|s| s.trim().starts_with("type "))
        {
            return;
        }
    } else if kind == "call_expression" {
        let func = node
            .child_by_field_name("function")
            .map(|n| text(n, source));
        if matches!(func, Some("require" | "import")) {
            module = node
                .child_by_field_name("arguments")
                .and_then(|n| children(n).into_iter().next());
            if module.is_none() || module.and_then(|n| unquote(text(n, source))).is_none() {
                incomplete.push(format!(
                    "{}: dynamic import cannot be resolved",
                    file.display()
                ));
            }
        }
    }
    if let Some(module) = module
        && let Some(spec) = unquote(text(module, source))
    {
        if spec.starts_with('.') {
            match resolve(&file.parent().unwrap_or(Path::new("")).join(spec), known) {
                Some(target) if target != file => {
                    edges.insert(target);
                }
                Some(_) => (),
                None => incomplete.push(format!(
                    "{}: unresolved relative import {spec}",
                    file.display()
                )),
            }
        } else if let Some((prefix, target)) = aliases
            .iter()
            .filter(|(prefix, _)| spec.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
        {
            match resolve(&Path::new(target).join(&spec[prefix.len()..]), known) {
                Some(target) if target != file => {
                    edges.insert(target);
                }
                Some(_) => (),
                None => incomplete.push(format!(
                    "{}: unresolved mapped alias {spec}",
                    file.display()
                )),
            }
        } else if !spec.starts_with("node:") {
            // Bare names can be workspace packages or tsconfig aliases; assuming
            // all of them external would create false passes.
            incomplete.push(format!(
                "{}: unresolved package or alias {spec}",
                file.display()
            ));
        }
    }
    for child in children(node) {
        add_js(child, source, file, known, aliases, edges, incomplete);
    }
}
fn add_python(
    node: Node<'_>,
    source: &str,
    file: &Path,
    known: &BTreeSet<PathBuf>,
    edges: &mut BTreeSet<PathBuf>,
    incomplete: &mut Vec<String>,
    type_only: bool,
) {
    if matches!(node.kind(), "function_definition" | "lambda") {
        return;
    }
    if node.kind() == "if_statement" {
        let condition = node
            .child_by_field_name("condition")
            .map(|n| text(n, source))
            .unwrap_or("");
        let is_type = condition == "TYPE_CHECKING" || condition.ends_with(".TYPE_CHECKING");
        for child in children(node) {
            let only = type_only
                || (is_type
                    && child.kind() == "block"
                    && node.child_by_field_name("consequence") == Some(child));
            add_python(child, source, file, known, edges, incomplete, only);
        }
        return;
    }
    if !type_only && node.kind() == "call" {
        let function = node
            .child_by_field_name("function")
            .map(|n| text(n, source))
            .unwrap_or("");
        if function == "__import__" || function == "importlib.import_module" {
            incomplete.push(format!(
                "{}: dynamic Python import cannot be resolved",
                file.display()
            ));
        }
    }
    if !type_only && matches!(node.kind(), "import_statement" | "import_from_statement") {
        let raw = text(node, source);
        if node.kind() == "import_statement" && raw.contains(',') {
            incomplete.push(format!(
                "{}: multiple Python imports require resolution",
                file.display()
            ));
        }
        let module = if let Some(rest) = raw.strip_prefix("from ") {
            rest.split_whitespace().next().unwrap_or("")
        } else {
            raw.strip_prefix("import ")
                .unwrap_or("")
                .split(',')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("")
        };
        let level = module.chars().take_while(|c| *c == '.').count();
        let suffix = module[level..].replace('.', "/");
        let mut bases = Vec::new();
        if level > 0 {
            let mut base = file.parent().unwrap_or(Path::new("")).to_path_buf();
            for _ in 1..level {
                if !base.pop() {
                    incomplete.push(format!(
                        "{}: invalid relative import {module}",
                        file.display()
                    ));
                    return;
                }
            }
            bases.push(base.join(&suffix));
        } else {
            bases.push(PathBuf::from(&suffix));
            bases.push(Path::new("backend").join(&suffix));
        }
        if node.kind() == "import_from_statement"
            && let Some((_, names)) = raw.split_once(" import ")
        {
            for name in names.trim_matches(|c| c == '(' || c == ')').split(',') {
                let name = name.split_whitespace().next().unwrap_or("");
                if name == "*" {
                    continue;
                }
                for base in &bases {
                    if let Some(target) = resolve(&base.join(name), known)
                        && target != file
                    {
                        edges.insert(target);
                    }
                }
            }
        }
        let target = bases.iter().find_map(|p| resolve(p, known));
        if let Some(target) = target {
            if target != file {
                edges.insert(target);
            }
        } else if level > 0 {
            incomplete.push(format!(
                "{}: unresolved relative import {module}",
                file.display()
            ));
        } else if !module.is_empty() {
            incomplete.push(format!(
                "{}: unresolved Python module {module}",
                file.display()
            ));
        }
        return;
    }
    for child in children(node) {
        add_python(child, source, file, known, edges, incomplete, type_only);
    }
}

pub fn build(
    sources: &BTreeMap<PathBuf, String>,
    external: &[String],
    aliases: &BTreeMap<String, String>,
) -> Result<Graph, String> {
    let known: BTreeSet<_> = sources.keys().cloned().collect();
    let mut graph = Graph::default();
    for (file, source) in sources {
        let extension = file.extension().and_then(|s| s.to_str()).unwrap_or("");
        let language = match extension {
            "py" => tree_sitter_python::LANGUAGE.into(),
            "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
            _ => continue,
        };
        let mut parser = Parser::new();
        parser.set_language(&language).map_err(|e| e.to_string())?;
        let tree = parser
            .parse(source, None)
            .ok_or("parser did not return a tree")?;
        if tree.root_node().has_error() {
            return Err(format!("could not parse {}", file.display()));
        }
        let mut edges = BTreeSet::new();
        if extension == "py" {
            add_python(
                tree.root_node(),
                source,
                file,
                &known,
                &mut edges,
                &mut graph.incomplete,
                false,
            );
        } else {
            add_js(
                tree.root_node(),
                source,
                file,
                &known,
                aliases,
                &mut edges,
                &mut graph.incomplete,
            );
        }
        graph.edges.insert(file.clone(), edges);
    }
    graph.incomplete.retain(|message| {
        !external.iter().any(|prefix| {
            message.strip_suffix(prefix).is_some_and(|head| {
                head.ends_with("unresolved package or alias ")
                    || head.ends_with("unresolved Python module ")
            })
        })
    });
    graph.incomplete.sort();
    graph.incomplete.dedup();
    graph.index();
    Ok(graph)
}
impl Graph {
    // Compute incoming edges and strongly connected components once rather
    // than traversing the entire graph for every file.
    fn index(&mut self) {
        let mut reverse: BTreeMap<PathBuf, BTreeSet<PathBuf>> = self
            .edges
            .keys()
            .map(|p| (p.clone(), BTreeSet::new()))
            .collect();
        for (from, targets) in &self.edges {
            for target in targets {
                reverse
                    .entry(target.clone())
                    .or_default()
                    .insert(from.clone());
            }
        }
        self.fan_in = reverse.iter().map(|(p, v)| (p.clone(), v.len())).collect();
        let mut visited = BTreeSet::new();
        let mut order = Vec::new();
        for start in self.edges.keys() {
            let mut stack = vec![(start.clone(), false)];
            while let Some((node, done)) = stack.pop() {
                if done {
                    order.push(node);
                    continue;
                }
                if !visited.insert(node.clone()) {
                    continue;
                }
                stack.push((node.clone(), true));
                stack.extend(
                    self.edges
                        .get(&node)
                        .into_iter()
                        .flatten()
                        .map(|n| (n.clone(), false)),
                );
            }
        }
        visited.clear();
        while let Some(start) = order.pop() {
            if visited.contains(&start) {
                continue;
            }
            let mut component = Vec::new();
            let mut stack = vec![start];
            while let Some(node) = stack.pop() {
                if !visited.insert(node.clone()) {
                    continue;
                }
                stack.extend(reverse.get(&node).into_iter().flatten().cloned());
                component.push(node);
            }
            if component.len() > 1 {
                self.cycle_members.extend(component);
            }
        }
    }
    pub fn fan_in(&self, file: &Path) -> usize {
        self.fan_in.get(file).copied().unwrap_or(0)
    }
    pub fn in_cycle(&self, file: &Path) -> bool {
        self.cycle_members.contains(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_counts_runtime_edges_and_cycles_but_not_type_imports() {
        let sources = BTreeMap::from([
            (
                PathBuf::from("a.ts"),
                "import { b } from './b'; import type { C } from './c';".to_string(),
            ),
            (
                PathBuf::from("b.ts"),
                "import './a'; export const b = 1;".to_string(),
            ),
            (PathBuf::from("c.ts"), "export type C = string;".to_string()),
        ]);
        let graph = build(&sources, &[], &BTreeMap::new()).unwrap();
        assert!(graph.incomplete.is_empty(), "{:?}", graph.incomplete);
        assert_eq!(graph.fan_in(Path::new("b.ts")), 1);
        assert!(graph.in_cycle(Path::new("a.ts")));
        assert!(!graph.in_cycle(Path::new("c.ts")));
    }
    #[test]
    fn unresolved_imports_are_not_silent() {
        let graph = build(
            &BTreeMap::from([(
                PathBuf::from("a.ts"),
                "import x from '@app/missing';".to_string(),
            )]),
            &[],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(graph.incomplete.len(), 1);
    }
    #[test]
    fn cycle_members_do_not_include_bridges_or_dependents() {
        let sources = BTreeMap::from([
            (PathBuf::from("a.ts"), "import './b';".to_string()),
            (
                PathBuf::from("b.ts"),
                "import './a'; import './c';".to_string(),
            ),
            (PathBuf::from("c.ts"), "import './d';".to_string()),
            (PathBuf::from("d.ts"), "export const d = 1;".to_string()),
            (PathBuf::from("e.ts"), "import './a';".to_string()),
        ]);
        let graph = build(&sources, &[], &BTreeMap::new()).unwrap();
        assert!(graph.in_cycle(Path::new("a.ts")));
        assert!(graph.in_cycle(Path::new("b.ts")));
        for file in ["c.ts", "d.ts", "e.ts"] {
            assert!(!graph.in_cycle(Path::new(file)), "{file}");
        }
        assert_eq!(graph.fan_in(Path::new("a.ts")), 2);
    }
    #[test]
    fn configured_aliases_resolve_to_local_edges_and_do_not_mask_missing_targets() {
        let aliases = BTreeMap::from([("@/".into(), "src/".into())]);
        let sources = BTreeMap::from([
            (
                PathBuf::from("src/a.ts"),
                "import '@/b'; import '@/missing';".to_string(),
            ),
            (PathBuf::from("src/b.ts"), "export const b = 1;".to_string()),
        ]);
        let graph = build(&sources, &["@/missing".into()], &aliases).unwrap();
        assert_eq!(graph.fan_in(Path::new("src/b.ts")), 1);
        assert_eq!(graph.incomplete.len(), 1);
        assert!(graph.incomplete[0].contains("unresolved mapped alias"));
    }
}
