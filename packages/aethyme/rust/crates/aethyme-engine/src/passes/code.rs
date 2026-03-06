use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::class::ClassNode;
use crate::edge::{Edge, EdgeKind};
use crate::file::{FileNode, FileRole};
use crate::function::FunctionNode;
use crate::indexer;
use crate::passes::structure::StructurePass;
use crate::symbol::{Symbol, SymbolKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePass {
    pub classes: Vec<ClassNode>,
    pub functions: Vec<FunctionNode>,
    pub compatibility_symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFile {
    file: FileNode,
    contents: String,
    language: String,
    symbols: Vec<Symbol>,
    import_edges: Vec<Edge>,
}

pub fn build(root: &Path, structure: &StructurePass) -> CodePass {
    let mut parsed_files = Vec::new();
    let mut classes = Vec::new();
    let mut functions = Vec::new();
    let mut compatibility_symbols = Vec::new();
    let mut edges = Vec::new();

    for file in &structure.files {
        if file.role != FileRole::Source {
            continue;
        }

        let absolute_path = root.join(&file.path);
        let contents = fs::read_to_string(&absolute_path).unwrap_or_default();
        let language = file.language.clone().unwrap_or_default();
        let extracted_symbols = extract_symbols(&language, &file.path, &contents)
            .into_iter()
            .map(|symbol| symbol.with_context(Some(language.clone()), file.area_id.clone()))
            .collect::<Vec<_>>();
        let import_edges = extract_import_edges(&language, &file.path, &contents);

        for symbol in &extracted_symbols {
            match symbol.kind {
                SymbolKind::Class => {
                    let class = ClassNode::new(
                        &structure.repo_name,
                        &file.id,
                        &file.path,
                        file.area_id.clone(),
                        &language,
                        &symbol.name,
                        symbol.line,
                        &symbol.signature,
                    );
                    edges.push(Edge::new(&file.id, &class.id, EdgeKind::Defines, 1000, &language));
                    classes.push(class);
                }
                SymbolKind::Function => {
                    let function = FunctionNode::new(
                        &structure.repo_name,
                        &file.id,
                        &file.path,
                        file.area_id.clone(),
                        None,
                        &language,
                        &symbol.name,
                        symbol.line,
                        &symbol.signature,
                    );
                    edges.push(Edge::new(&file.id, &function.id, EdgeKind::Defines, 1000, &language));
                    functions.push(function);
                }
                SymbolKind::Constant => {}
            }
            compatibility_symbols.push(symbol.clone());
        }

        parsed_files.push(ParsedFile {
            file: file.clone(),
            contents,
            language,
            symbols: extracted_symbols,
            import_edges,
        });
    }

    let file_functions = build_file_function_map(&functions);
    let file_classes = build_file_class_map(&classes);
    let all_function_names = build_global_function_name_map(&functions);
    let all_class_names = build_global_class_name_map(&classes);

    for parsed in &parsed_files {
        let resolved_import_targets = resolve_import_edges(&structure.files, parsed);
        edges.extend(resolved_import_targets.iter().cloned());
        edges.extend(resolve_cross_file_calls(
            parsed,
            &file_functions,
            &all_function_names,
            &resolved_import_targets,
        ));
        edges.extend(resolve_references(
            parsed,
            &file_functions,
            &file_classes,
            &all_function_names,
            &all_class_names,
        ));
    }

    classes.sort();
    functions.sort();
    compatibility_symbols.sort();
    edges.sort();
    edges.dedup();

    CodePass {
        classes,
        functions,
        compatibility_symbols,
        edges,
    }
}

fn extract_symbols(language: &str, path: &str, contents: &str) -> Vec<Symbol> {
    match language {
        "python" => indexer::python::extract_symbols(path, contents),
        "typescript" | "javascript" => indexer::typescript::extract_symbols(path, contents),
        "rust" => indexer::rust::extract_symbols(path, contents),
        _ => Vec::new(),
    }
}

fn extract_import_edges(language: &str, path: &str, contents: &str) -> Vec<Edge> {
    match language {
        "python" => indexer::python::extract_import_edges(path, contents),
        "typescript" | "javascript" => indexer::typescript::extract_import_edges(path, contents),
        "rust" => indexer::rust::extract_import_edges(path, contents),
        _ => Vec::new(),
    }
}

fn resolve_import_edges(all_files: &[FileNode], parsed: &ParsedFile) -> Vec<Edge> {
    let mut resolved = Vec::new();
    for edge in &parsed.import_edges {
        let resolved_target = resolve_import_target(all_files, &parsed.file.path, &edge.to, &parsed.language)
            .unwrap_or_else(|| format!("import:{}", edge.to));
        resolved.push(Edge::new(
            &parsed.file.id,
            resolved_target,
            EdgeKind::Imports,
            900,
            &edge.source,
        ));
    }
    resolved
}

fn resolve_import_target(
    all_files: &[FileNode],
    source_file: &str,
    raw_target: &str,
    language: &str,
) -> Option<String> {
    match language {
        "python" => resolve_python_import_target(all_files, raw_target),
        "typescript" | "javascript" => resolve_typescript_import_target(all_files, source_file, raw_target),
        "rust" => resolve_rust_import_target(all_files, source_file, raw_target),
        _ => None,
    }
}

fn resolve_python_import_target(all_files: &[FileNode], raw_target: &str) -> Option<String> {
    let dotted_path = raw_target.replace('.', "/");
    find_matching_file_id(all_files, &format!("{dotted_path}.py"))
        .or_else(|| find_matching_file_id(all_files, &format!("{dotted_path}/__init__.py")))
        .or_else(|| find_last_segment_file(all_files, raw_target, &[".py"]))
}

fn resolve_typescript_import_target(all_files: &[FileNode], source_file: &str, raw_target: &str) -> Option<String> {
    if raw_target.starts_with('.') {
        let source_dir = source_file.rsplit_once('/').map(|value| value.0).unwrap_or("");
        let candidate = normalize_relative_path(source_dir, raw_target);
        return find_matching_file_id(all_files, &candidate)
            .or_else(|| find_matching_file_id(all_files, &format!("{candidate}.ts")))
            .or_else(|| find_matching_file_id(all_files, &format!("{candidate}.tsx")))
            .or_else(|| find_matching_file_id(all_files, &format!("{candidate}.js")))
            .or_else(|| find_matching_file_id(all_files, &format!("{candidate}/index.ts")))
            .or_else(|| find_matching_file_id(all_files, &format!("{candidate}/index.js")));
    }

    find_last_segment_file(all_files, raw_target, &[".ts", ".tsx", ".js", ".jsx"])
}

fn resolve_rust_import_target(all_files: &[FileNode], source_file: &str, raw_target: &str) -> Option<String> {
    let source_dir = source_file.rsplit_once('/').map(|value| value.0).unwrap_or("");
    let target = raw_target.trim_end_matches(';').trim();
    let last_segment = target.split("::").last().unwrap_or(target);

    if target.starts_with("super::") {
        let parent_dir = source_dir.rsplit_once('/').map(|value| value.0).unwrap_or("");
        let normalized = if parent_dir.is_empty() {
            last_segment.to_string()
        } else {
            format!("{parent_dir}/{last_segment}")
        };
        return find_matching_file_id(all_files, &format!("{normalized}.rs"))
            .or_else(|| find_matching_file_id(all_files, &format!("{normalized}/mod.rs")));
    }

    if target.starts_with("self::") {
        let normalized = if source_dir.is_empty() {
            last_segment.to_string()
        } else {
            format!("{source_dir}/{last_segment}")
        };
        return find_matching_file_id(all_files, &format!("{normalized}.rs"))
            .or_else(|| find_matching_file_id(all_files, &format!("{normalized}/mod.rs")));
    }

    if target.starts_with("crate::") {
        let relative = target.trim_start_matches("crate::").replace("::", "/");
        return find_matching_file_id(all_files, &format!("src/{relative}.rs"))
            .or_else(|| find_matching_file_id(all_files, &format!("src/{relative}/mod.rs")))
            .or_else(|| find_last_segment_file(all_files, last_segment, &[".rs"]));
    }

    find_matching_file_id(all_files, &format!("{source_dir}/{target}.rs"))
        .or_else(|| find_matching_file_id(all_files, &format!("{source_dir}/{target}/mod.rs")))
        .or_else(|| find_last_segment_file(all_files, last_segment, &[".rs"]))
}

fn normalize_relative_path(source_dir: &str, raw_target: &str) -> String {
    let mut parts: Vec<&str> = if source_dir.is_empty() {
        Vec::new()
    } else {
        source_dir.split('/').collect()
    };
    for segment in raw_target.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    parts.join("/")
}

fn find_matching_file_id(all_files: &[FileNode], candidate: &str) -> Option<String> {
    all_files
        .iter()
        .find(|file| file.path == candidate)
        .map(|file| file.id.clone())
}

fn find_last_segment_file(all_files: &[FileNode], raw_target: &str, extensions: &[&str]) -> Option<String> {
    let last = raw_target
        .rsplit('/')
        .next()
        .unwrap_or(raw_target)
        .rsplit('.')
        .next()
        .unwrap_or(raw_target)
        .rsplit("::")
        .next()
        .unwrap_or(raw_target);

    all_files
        .iter()
        .find(|file| extensions.iter().any(|ext| file.path.ends_with(&format!("/{last}{ext}"))))
        .map(|file| file.id.clone())
}

fn build_file_function_map(functions: &[FunctionNode]) -> BTreeMap<String, Vec<FunctionNode>> {
    let mut map = BTreeMap::new();
    for function in functions {
        map.entry(function.file_id.clone()).or_insert_with(Vec::new).push(function.clone());
    }
    for items in map.values_mut() {
        items.sort_by(|left, right| left.line.cmp(&right.line).then_with(|| left.name.cmp(&right.name)));
    }
    map
}

fn build_file_class_map(classes: &[ClassNode]) -> BTreeMap<String, Vec<ClassNode>> {
    let mut map = BTreeMap::new();
    for class in classes {
        map.entry(class.file_id.clone()).or_insert_with(Vec::new).push(class.clone());
    }
    for items in map.values_mut() {
        items.sort_by(|left, right| left.line.cmp(&right.line).then_with(|| left.name.cmp(&right.name)));
    }
    map
}

fn build_global_function_name_map(functions: &[FunctionNode]) -> BTreeMap<String, Vec<FunctionNode>> {
    let mut map = BTreeMap::new();
    for function in functions {
        map.entry(function.name.clone())
            .or_insert_with(Vec::new)
            .push(function.clone());
    }
    map
}

fn build_global_class_name_map(classes: &[ClassNode]) -> BTreeMap<String, Vec<ClassNode>> {
    let mut map = BTreeMap::new();
    for class in classes {
        map.entry(class.name.clone())
            .or_insert_with(Vec::new)
            .push(class.clone());
    }
    map
}

fn resolve_cross_file_calls(
    parsed: &ParsedFile,
    file_functions: &BTreeMap<String, Vec<FunctionNode>>,
    all_function_names: &BTreeMap<String, Vec<FunctionNode>>,
    resolved_import_edges: &[Edge],
) -> Vec<Edge> {
    let current_functions = file_functions.get(&parsed.file.id).cloned().unwrap_or_default();
    let imported_targets: BTreeSet<String> = resolved_import_edges
        .iter()
        .map(|edge| edge.to.clone())
        .collect();
    let imported_file_functions: Vec<FunctionNode> = file_functions
        .iter()
        .filter(|(file_id, _)| imported_targets.contains(*file_id))
        .flat_map(|(_, functions)| functions.iter().cloned())
        .collect();

    let mut edges = Vec::new();
    for (index, function) in current_functions.iter().enumerate() {
        let body = function_body(&parsed.contents, function.line, current_functions.get(index + 1).map(|item| item.line));
        for target in &current_functions {
            if target.id == function.id {
                continue;
            }
            if body.contains(&format!("{}(", target.name)) {
                edges.push(Edge::new(&function.id, &target.id, EdgeKind::Calls, 800, &parsed.language));
            }
        }
        for target in &imported_file_functions {
            if body.contains(&format!("{}(", target.name)) || body.contains(&format!("::{}(", target.name)) {
                edges.push(Edge::new(&function.id, &target.id, EdgeKind::Calls, 750, &parsed.language));
            }
        }
        for target in body_call_candidates(&body, all_function_names) {
            if target.file_id != function.file_id {
                edges.push(Edge::new(&function.id, &target.id, EdgeKind::Calls, 600, &parsed.language));
            }
        }
    }
    edges.sort();
    edges.dedup();
    edges
}

fn resolve_references(
    parsed: &ParsedFile,
    file_functions: &BTreeMap<String, Vec<FunctionNode>>,
    file_classes: &BTreeMap<String, Vec<ClassNode>>,
    all_function_names: &BTreeMap<String, Vec<FunctionNode>>,
    all_class_names: &BTreeMap<String, Vec<ClassNode>>,
) -> Vec<Edge> {
    let current_functions = file_functions.get(&parsed.file.id).cloned().unwrap_or_default();
    let current_classes = file_classes.get(&parsed.file.id).cloned().unwrap_or_default();
    let mut edges = Vec::new();

    for (index, function) in current_functions.iter().enumerate() {
        let body = function_body(&parsed.contents, function.line, current_functions.get(index + 1).map(|item| item.line));

        for class in &current_classes {
            if body.contains(&class.name) {
                edges.push(Edge::new(&function.id, &class.id, EdgeKind::References, 700, &parsed.language));
            }
        }

        for class_matches in all_class_names.values() {
            for class in class_matches {
                if class.file_id != function.file_id
                    && (body.contains(&format!("{}::", class.name)) || body.contains(&format!("{}(", class.name)))
                {
                    edges.push(Edge::new(&function.id, &class.id, EdgeKind::References, 650, &parsed.language));
                }
            }
        }

        for function_matches in all_function_names.values() {
            for target in function_matches {
                if target.id != function.id
                    && body.contains(&target.name)
                    && !body.contains(&format!("{}(", target.name))
                {
                    edges.push(Edge::new(&function.id, &target.id, EdgeKind::References, 500, &parsed.language));
                }
            }
        }
    }

    edges.sort();
    edges.dedup();
    edges
}

fn body_call_candidates(
    body: &str,
    all_function_names: &BTreeMap<String, Vec<FunctionNode>>,
) -> Vec<FunctionNode> {
    let mut matches = Vec::new();
    for (name, candidates) in all_function_names {
        if body.contains(&format!("{name}(")) && candidates.len() == 1 {
            matches.push(candidates[0].clone());
        }
    }
    matches
}

fn function_body(contents: &str, start_line: usize, next_line: Option<usize>) -> String {
    let start = start_line.saturating_sub(1);
    let take = next_line
        .map(|line| line.saturating_sub(start_line).max(1))
        .unwrap_or(40)
        .min(80);
    contents.lines().skip(start).take(take).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build;
    use crate::edge::EdgeKind;
    use crate::passes::structure;
    use crate::repo::discover_repo;

    #[test]
    fn rust_extraction_finds_structs_and_functions() {
        let root = std::env::temp_dir().join("aethyme_engine_rust_extract_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/lib.rs"),
            "pub struct Engine;\n\npub fn make_engine() -> Engine { Engine }\n",
        )
        .expect("write rust source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = build(&root, &structure);

        assert!(code.classes.iter().any(|class| class.name == "Engine"));
        assert!(code.functions.iter().any(|function| function.name == "make_engine"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_file_python_call_resolution_links_imported_function() {
        let root = std::env::temp_dir().join("aethyme_engine_cross_file_call_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "from auth import validate_token\n\ndef run():\n    return validate_token()\n",
        )
        .expect("write source");
        fs::write(root.join("src/auth.py"), "def validate_token():\n    return True\n").expect("write source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = build(&root, &structure);

        assert!(code.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::Calls) && edge.from.contains("run") && edge.to.contains("validate_token")));

        let _ = fs::remove_dir_all(&root);
    }
}
