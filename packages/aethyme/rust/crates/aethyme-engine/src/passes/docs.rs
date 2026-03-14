use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::model::doc::DocNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::passes::code::CodePass;
use crate::passes::configs::ConfigsPass;
use crate::passes::structure::StructurePass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsPass {
    pub docs: Vec<DocNode>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsStageProfile {
    pub read_docs_ms: u128,
    pub link_areas_ms: u128,
    pub link_files_ms: u128,
    pub link_configs_ms: u128,
    pub link_symbols_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDoc {
    file_id: String,
    path: String,
    name: String,
    area_id: Option<String>,
    contents: String,
    title: String,
    doc_type: String,
    tokens: BTreeSet<String>,
    explicit_mentions: BTreeSet<String>,
}

type AreaTokenKey = (String, String);

pub fn build(
    root: &Path,
    structure: &StructurePass,
    code: &CodePass,
    configs: Option<&ConfigsPass>,
) -> DocsPass {
    build_with_profile(root, structure, code, configs).0
}

pub fn build_with_profile(
    root: &Path,
    structure: &StructurePass,
    code: &CodePass,
    configs: Option<&ConfigsPass>,
) -> (DocsPass, DocsStageProfile) {
    build_with_profile_and_progress(root, structure, code, configs, |_, _| {})
}

pub fn build_with_profile_and_progress<F>(
    root: &Path,
    structure: &StructurePass,
    code: &CodePass,
    configs: Option<&ConfigsPass>,
    mut progress: F,
) -> (DocsPass, DocsStageProfile)
where
    F: FnMut(&str, u128),
{
    let read_started = Instant::now();
    let mut parsed_docs = structure
        .files
        .par_iter()
        .filter(|file| matches!(file.role, crate::model::file::FileRole::Doc))
        .map(|file| parse_doc(root, file.path.as_str(), file.id.as_str(), file.name.as_str(), file.area_id.clone()))
        .collect::<Vec<_>>();
    parsed_docs.sort_by(|left, right| left.path.cmp(&right.path));
    let read_docs_ms = read_started.elapsed().as_millis();
    progress("docs_read", read_docs_ms);

    let mut docs = parsed_docs
        .iter()
        .map(|parsed| {
            DocNode::new(
                &structure.repo_name,
                &parsed.file_id,
                &parsed.path,
                &parsed.title,
                &parsed.doc_type,
                parsed.area_id.clone(),
            )
        })
        .collect::<Vec<_>>();

    let mut edges = Vec::new();
    for doc in &docs {
        edges.push(Edge::new(&doc.file_id, &doc.id, EdgeKind::Defines, 1000, "docs"));
        if let Some(area_id) = &doc.area_id {
            edges.push(Edge::new(&doc.id, area_id, EdgeKind::Documents, 800, "docs"));
        } else {
            edges.push(Edge::new(&doc.id, &structure.repo_id, EdgeKind::Documents, 800, "docs"));
        }
    }

    let file_path_index = structure
        .files
        .iter()
        .filter(|file| !matches!(file.role, crate::model::file::FileRole::Doc))
        .map(|file| (file.path.to_ascii_lowercase(), file.id.clone()))
        .collect::<HashMap<_, _>>();
    let file_token_index = build_file_token_index(structure);
    let config_token_index = build_config_token_index(configs);
    let area_token_index = build_area_token_index(structure);
    let class_token_index = build_symbol_token_index(
        code.classes
            .iter()
            .map(|class| (class.name.as_str(), class.id.as_str(), class.area_id.as_deref())),
    );
    let function_token_index = build_symbol_token_index(
        code.functions
            .iter()
            .map(|function| (function.name.as_str(), function.id.as_str(), function.area_id.as_deref())),
    );

    let areas_started = Instant::now();
    for (doc, parsed) in docs.iter().zip(parsed_docs.iter()) {
        edges.extend(link_doc_to_areas(doc, &parsed.tokens, &area_token_index));
    }
    let link_areas_ms = areas_started.elapsed().as_millis();
    progress("docs_link_areas", link_areas_ms);

    let files_started = Instant::now();
    for (doc, parsed) in docs.iter().zip(parsed_docs.iter()) {
        edges.extend(link_doc_to_files(doc, &parsed.tokens, &file_path_index, &file_token_index));
    }
    let link_files_ms = files_started.elapsed().as_millis();
    progress("docs_link_files", link_files_ms);

    let configs_started = Instant::now();
    for (doc, parsed) in docs.iter().zip(parsed_docs.iter()) {
        edges.extend(link_doc_to_configs(doc, &parsed.tokens, &config_token_index));
    }
    let link_configs_ms = configs_started.elapsed().as_millis();
    progress("docs_link_configs", link_configs_ms);

    let symbols_started = Instant::now();
    for (doc, parsed) in docs.iter().zip(parsed_docs.iter()) {
        edges.extend(link_doc_to_symbols(
            doc,
            &parsed.tokens,
            &parsed.explicit_mentions,
            &class_token_index,
            &function_token_index,
        ));
    }
    let link_symbols_ms = symbols_started.elapsed().as_millis();
    progress("docs_link_symbols", link_symbols_ms);

    docs.sort();
    edges.sort();
    edges.dedup();

    (
        DocsPass { docs, edges },
        DocsStageProfile {
            read_docs_ms,
            link_areas_ms,
            link_files_ms,
            link_configs_ms,
            link_symbols_ms,
        },
    )
}

fn parse_doc(
    root: &Path,
    path: &str,
    file_id: &str,
    name: &str,
    area_id: Option<String>,
) -> ParsedDoc {
    let contents = fs::read_to_string(root.join(path)).unwrap_or_default();
    let title = read_title(&contents).unwrap_or_else(|| name.to_string());
    let doc_type = classify_doc_type(path, &title);
    let tokens = identifier_tokens(&contents);
    let explicit_mentions = explicit_symbol_mentions(&contents);

    ParsedDoc {
        file_id: file_id.to_string(),
        path: path.to_string(),
        name: name.to_string(),
        area_id,
        contents,
        title,
        doc_type,
        tokens,
        explicit_mentions,
    }
}

fn build_file_token_index(structure: &StructurePass) -> HashMap<AreaTokenKey, Vec<String>> {
    let mut index = HashMap::new();
    for file in &structure.files {
        if matches!(file.role, crate::model::file::FileRole::Doc) {
            continue;
        }
        let area_key = area_key(file.area_id.as_deref());
        for token in [file.name.to_ascii_lowercase(), file_stem(&file.name)] {
            index.entry((area_key.clone(), token)).or_insert_with(Vec::new).push(file.id.clone());
        }
    }
    index
}

fn build_config_token_index(configs: Option<&ConfigsPass>) -> HashMap<String, Vec<String>> {
    let mut index = HashMap::new();
    let Some(configs) = configs else {
        return index;
    };
    for config in &configs.configs {
        for token in [config.path.to_ascii_lowercase(), file_stem(&config.path)] {
            index.entry(token).or_insert_with(Vec::new).push(config.id.clone());
        }
    }
    index
}

fn build_area_token_index(structure: &StructurePass) -> HashMap<String, Vec<String>> {
    let mut index = HashMap::new();
    for area in &structure.areas {
        for token in identifier_tokens(&area.name) {
            index.entry(token).or_insert_with(Vec::new).push(area.id.clone());
        }
        for token in identifier_tokens(&area.path_prefix) {
            index.entry(token).or_insert_with(Vec::new).push(area.id.clone());
        }
    }
    index
}

fn build_symbol_token_index<'a>(
    items: impl Iterator<Item = (&'a str, &'a str, Option<&'a str>)>,
) -> HashMap<AreaTokenKey, Vec<String>> {
    let mut index = HashMap::new();
    for (name, id, area_id) in items {
        let token = name.to_ascii_lowercase();
        index
            .entry((area_key(area_id), token.clone()))
            .or_insert_with(Vec::new)
            .push(id.to_string());
        index
            .entry((String::new(), token))
            .or_insert_with(Vec::new)
            .push(id.to_string());
    }
    index
}

fn area_key(area: Option<&str>) -> String {
    area.unwrap_or("").to_string()
}

fn read_title(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("# ") {
            return Some(title.trim().to_string());
        }
    }
    None
}

fn classify_doc_type(path: &str, title: &str) -> String {
    let lower_path = path.to_ascii_lowercase();
    let lower_title = title.to_ascii_lowercase();
    if lower_path.ends_with("readme.md") {
        "readme".to_string()
    } else if lower_path.contains("architecture") || lower_title.contains("architecture") {
        "architecture".to_string()
    } else if lower_path.contains("guide") || lower_title.contains("guide") {
        "guide".to_string()
    } else if lower_path.contains("spec") || lower_title.contains("spec") {
        "spec".to_string()
    } else {
        "documentation".to_string()
    }
}

fn link_doc_to_areas(
    doc: &DocNode,
    tokens: &BTreeSet<String>,
    area_token_index: &HashMap<String, Vec<String>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    let doc_area_id = doc.area_id.as_deref();
    for token in tokens {
        if let Some(area_ids) = area_token_index.get(token) {
            for area_id in area_ids {
                if doc_area_id == Some(area_id.as_str()) || !seen.insert(area_id.clone()) {
                    continue;
                }
                edges.push(Edge::new(&doc.id, area_id, EdgeKind::Documents, 700, "docs"));
            }
        }
    }
    edges
}

fn link_doc_to_files(
    doc: &DocNode,
    tokens: &BTreeSet<String>,
    file_path_index: &HashMap<String, String>,
    file_token_index: &HashMap<AreaTokenKey, Vec<String>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    let area_key = area_key(doc.area_id.as_deref());

    for token in tokens {
        if let Some(file_id) = file_path_index.get(token)
            && seen.insert(file_id.clone())
        {
            edges.push(Edge::new(&doc.id, file_id, EdgeKind::Documents, 750, "docs"));
        }

        for key in [(area_key.clone(), token.clone()), (String::new(), token.clone())] {
            if let Some(file_ids) = file_token_index.get(&key) {
                for file_id in file_ids {
                    if seen.insert(file_id.clone()) {
                        edges.push(Edge::new(&doc.id, file_id, EdgeKind::References, 500, "docs"));
                    }
                }
            }
        }
    }

    edges
}

fn link_doc_to_configs(
    doc: &DocNode,
    tokens: &BTreeSet<String>,
    config_token_index: &HashMap<String, Vec<String>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    for token in tokens {
        if let Some(config_ids) = config_token_index.get(token) {
            for config_id in config_ids {
                if seen.insert(config_id.clone()) {
                    edges.push(Edge::new(&doc.id, config_id, EdgeKind::Documents, 650, "docs"));
                }
            }
        }
    }
    edges
}

fn link_doc_to_symbols(
    doc: &DocNode,
    tokens: &BTreeSet<String>,
    explicit_mentions: &BTreeSet<String>,
    class_token_index: &HashMap<AreaTokenKey, Vec<String>>,
    function_token_index: &HashMap<AreaTokenKey, Vec<String>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    let area_key = area_key(doc.area_id.as_deref());

    for token in tokens {
        let key = (area_key.clone(), token.clone());
        if let Some(class_ids) = class_token_index.get(&key) {
            for class_id in class_ids {
                if seen.insert(class_id.clone()) {
                    edges.push(Edge::new(&doc.id, class_id, EdgeKind::References, 550, "docs"));
                }
            }
        }
        if let Some(function_ids) = function_token_index.get(&key) {
            for function_id in function_ids {
                if seen.insert(function_id.clone()) {
                    edges.push(Edge::new(&doc.id, function_id, EdgeKind::References, 550, "docs"));
                }
            }
        }
    }

    for token in explicit_mentions {
        for key in [(area_key.clone(), token.clone()), (String::new(), token.clone())] {
            if let Some(class_ids) = class_token_index.get(&key) {
                for class_id in class_ids {
                    if seen.insert(class_id.clone()) {
                        edges.push(Edge::new(&doc.id, class_id, EdgeKind::References, 550, "docs"));
                    }
                }
            }
            if let Some(function_ids) = function_token_index.get(&key) {
                for function_id in function_ids {
                    if seen.insert(function_id.clone()) {
                        edges.push(Edge::new(&doc.id, function_id, EdgeKind::References, 550, "docs"));
                    }
                }
            }
        }
    }

    edges
}

fn identifier_tokens(contents: &str) -> BTreeSet<String> {
    contents
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '.' && character != '/'
        })
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn explicit_symbol_mentions(contents: &str) -> BTreeSet<String> {
    let mut mentions = BTreeSet::new();
    let mut remainder = contents;
    while let Some(start) = remainder.find('`') {
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        let token = after_start[..end].trim();
        if !token.is_empty() {
            mentions.insert(token.to_ascii_lowercase());
        }
        remainder = &after_start[end + 1..];
    }
    mentions
}

fn file_stem(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build;
    use crate::model::edge::EdgeKind;
    use crate::passes::{code, structure};
    use crate::repo::discover_repo;

    #[test]
    fn docs_link_to_documented_files_and_functions() {
        let root = std::env::temp_dir().join("aethyme_engine_docs_link_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("src/main.py"), "def main():\n    return 1\n").expect("write source");
        fs::write(
            root.join("architecture.md"),
            "# Architecture\n\nThe entrypoint is `src/main.py` and `main`.\n",
        )
        .expect("write doc");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = code::build(&root, &structure);
        let docs = build(&root, &structure, &code, None);

        assert!(docs.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::Documents) && edge.to.contains("src/main.py")));
        assert!(docs.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::References) && edge.to.contains("main")));

        let _ = fs::remove_dir_all(&root);
    }
}
