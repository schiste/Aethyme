use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;

use crate::model::config::ConfigNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::passes::code::CodePass;
use crate::passes::structure::StructurePass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigsPass {
    pub configs: Vec<ConfigNode>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigsStageProfile {
    pub read_configs_ms: u128,
    pub link_configs_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedConfig {
    file_id: String,
    path: String,
    area_id: Option<String>,
    config_type: String,
    contents: String,
    tokens: BTreeSet<String>,
}

pub fn build(root: &Path, structure: &StructurePass, code: &CodePass) -> ConfigsPass {
    build_with_profile(root, structure, code).0
}

pub fn build_with_profile(
    root: &Path,
    structure: &StructurePass,
    code: &CodePass,
) -> (ConfigsPass, ConfigsStageProfile) {
    build_with_profile_and_progress(root, structure, code, |_, _| {})
}

pub fn build_with_profile_and_progress<F>(
    root: &Path,
    structure: &StructurePass,
    code: &CodePass,
    mut progress: F,
) -> (ConfigsPass, ConfigsStageProfile)
where
    F: FnMut(&str, u128),
{
    let read_started = Instant::now();
    let mut parsed_configs = structure
        .files
        .par_iter()
        .filter(|file| matches!(file.role, crate::model::file::FileRole::Config))
        .map(|file| ParsedConfig {
            file_id: file.id.clone(),
            path: file.path.clone(),
            area_id: file.area_id.clone(),
            config_type: classify_config_type(&file.path),
            contents: fs::read_to_string(root.join(&file.path)).unwrap_or_default(),
            tokens: BTreeSet::new(),
        })
        .collect::<Vec<_>>();
    for parsed in &mut parsed_configs {
        parsed.tokens = config_tokens(&parsed.contents);
    }
    parsed_configs.sort_by(|left, right| left.path.cmp(&right.path));
    let read_configs_ms = read_started.elapsed().as_millis();
    progress("configs_read", read_configs_ms);

    let files_by_path = structure
        .files
        .iter()
        .map(|file| (file.path.clone(), file.id.clone()))
        .collect::<HashMap<_, _>>();
    let file_token_index = build_file_token_index(structure);

    let link_started = Instant::now();
    let mut configs = Vec::new();
    let mut edges = Vec::new();

    for parsed in &parsed_configs {
        let config = ConfigNode::new(
            &structure.repo_name,
            &parsed.file_id,
            &parsed.path,
            &parsed.config_type,
            parsed.area_id.clone(),
        );

        edges.push(Edge::new(
            &parsed.file_id,
            &config.id,
            EdgeKind::Defines,
            1000,
            "config",
        ));

        if let Some(area_id) = &parsed.area_id {
            edges.push(Edge::new(
                &config.id,
                area_id,
                EdgeKind::Configures,
                800,
                "config",
            ));
        }

        edges.extend(link_config_to_entrypoints(
            &config,
            &parsed.contents,
            structure,
            code,
        ));
        edges.extend(link_config_to_referenced_files(
            &config,
            &parsed.tokens,
            &files_by_path,
            &file_token_index,
        ));

        configs.push(config);
    }

    let link_configs_ms = link_started.elapsed().as_millis();
    progress("configs_link", link_configs_ms);

    configs.sort();
    edges.sort();
    edges.dedup();

    (
        ConfigsPass { configs, edges },
        ConfigsStageProfile {
            read_configs_ms,
            link_configs_ms,
        },
    )
}

fn classify_config_type(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.ends_with("pyproject.toml")
    {
        "manifest".to_string()
    } else if lower.ends_with("project.godot") {
        "project".to_string()
    } else if lower.ends_with("dockerfile")
        || lower.ends_with("docker-compose.yml")
        || lower.ends_with("docker-compose.yaml")
    {
        "runtime".to_string()
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "yaml".to_string()
    } else if lower.ends_with(".toml") {
        "toml".to_string()
    } else if lower.ends_with(".json") {
        "json".to_string()
    } else {
        "config".to_string()
    }
}

fn link_config_to_entrypoints(
    config: &ConfigNode,
    contents: &str,
    structure: &StructurePass,
    code: &CodePass,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let area_id = config
        .area_id
        .clone()
        .unwrap_or_else(|| structure.repo_id.clone());
    let config_dir = config
        .path
        .rsplit_once('/')
        .map(|value| value.0)
        .unwrap_or("");

    match config.config_type.as_str() {
        "manifest" => {
            edges.push(Edge::new(
                &config.id,
                &area_id,
                EdgeKind::EntrypointFor,
                700,
                "config",
            ));
            for entry_path in manifest_entrypoint_paths(contents) {
                let normalized = normalize_config_relative_path(config_dir, &entry_path);
                if let Some(file) = structure.files.iter().find(|file| file.path == normalized) {
                    edges.push(Edge::new(
                        &config.id,
                        &file.id,
                        EdgeKind::EntrypointFor,
                        900,
                        "config",
                    ));
                }
            }

            for function in &code.functions {
                if function.area_id.as_deref() == config.area_id.as_deref()
                    && function.name == "main"
                {
                    edges.push(Edge::new(
                        &config.id,
                        &function.id,
                        EdgeKind::EntrypointFor,
                        800,
                        "config",
                    ));
                }
            }
        }
        "project" => {
            edges.push(Edge::new(
                &config.id,
                &area_id,
                EdgeKind::EntrypointFor,
                800,
                "config",
            ));
            for entry_path in godot_entrypoint_paths(contents) {
                let normalized = normalize_godot_path(&entry_path);
                if let Some(file) = structure.files.iter().find(|file| file.path == normalized) {
                    edges.push(Edge::new(
                        &config.id,
                        &file.id,
                        EdgeKind::EntrypointFor,
                        900,
                        "config",
                    ));
                }
            }
            for file in &structure.files {
                if (file.path.ends_with(".tscn") || file.path.ends_with(".gd"))
                    && (contents.contains(&file.path)
                        || file.area_id.as_deref() == config.area_id.as_deref())
                {
                    edges.push(Edge::new(
                        &config.id,
                        &file.id,
                        EdgeKind::Configures,
                        700,
                        "config",
                    ));
                }
            }
        }
        "runtime" => {
            edges.push(Edge::new(
                &config.id,
                &area_id,
                EdgeKind::Configures,
                700,
                "config",
            ));
        }
        _ => {}
    }

    edges
}

fn link_config_to_referenced_files(
    config: &ConfigNode,
    tokens: &BTreeSet<String>,
    files_by_path: &HashMap<String, String>,
    file_token_index: &HashMap<(String, String), Vec<String>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    let area_key = config.area_id.clone().unwrap_or_default();

    for token in tokens {
        for key in [
            (area_key.clone(), token.clone()),
            (String::new(), token.clone()),
        ] {
            if let Some(file_ids) = file_token_index.get(&key) {
                for file_id in file_ids {
                    if file_id != &config.file_id && seen.insert(file_id.clone()) {
                        edges.push(Edge::new(
                            &config.id,
                            file_id,
                            EdgeKind::Configures,
                            750,
                            "config",
                        ));
                    }
                }
            }
        }
    }

    if config.path.ends_with("Cargo.toml") {
        for candidate in ["src/main.rs", "src/lib.rs"] {
            if let Some(file_id) = files_by_path.get(candidate) {
                edges.push(Edge::new(
                    &config.id,
                    file_id,
                    EdgeKind::EntrypointFor,
                    850,
                    "config",
                ));
            } else {
                let candidate_token = candidate
                    .rsplit('/')
                    .next()
                    .unwrap_or(candidate)
                    .to_ascii_lowercase();
                if let Some(file_ids) = file_token_index.get(&(area_key.clone(), candidate_token)) {
                    for file_id in file_ids {
                        edges.push(Edge::new(
                            &config.id,
                            file_id,
                            EdgeKind::EntrypointFor,
                            850,
                            "config",
                        ));
                    }
                }
            }
        }
    }

    if config.path.ends_with("package.json") {
        for candidate in package_entrypoint_paths_from_tokens(tokens) {
            let candidate_token = candidate
                .rsplit('/')
                .next()
                .unwrap_or(candidate.as_str())
                .to_ascii_lowercase();
            if let Some(file_ids) = file_token_index.get(&(area_key.clone(), candidate_token)) {
                for file_id in file_ids {
                    edges.push(Edge::new(
                        &config.id,
                        file_id,
                        EdgeKind::EntrypointFor,
                        850,
                        "config",
                    ));
                }
            }
        }
    }

    edges
}

fn manifest_entrypoint_paths(contents: &str) -> Vec<String> {
    let mut paths: Vec<String> = contents.lines().filter_map(parse_path_assignment).collect();
    // JSON manifests (package.json): extract "main", "module", and root export paths.
    for line in contents.lines() {
        let trimmed = line.trim();
        for key in &["\"main\"", "\"module\""] {
            if trimmed.starts_with(key) {
                if let Some(value) = trimmed.split(':').nth(1) {
                    let cleaned = clean_value(value);
                    if !cleaned.is_empty() {
                        paths.push(cleaned);
                    }
                }
            }
        }
        // "." export: `".": "./src/index.ts"` or `".": { "import": "..." }`
        if trimmed.starts_with("\".\"") {
            if let Some(value) = trimmed.split(':').nth(1) {
                let cleaned = clean_value(value);
                if !cleaned.is_empty()
                    && (cleaned.ends_with(".ts")
                        || cleaned.ends_with(".js")
                        || cleaned.ends_with(".mjs"))
                {
                    paths.push(cleaned);
                }
            }
        }
    }
    paths
}

fn package_entrypoint_paths_from_tokens(tokens: &BTreeSet<String>) -> Vec<String> {
    tokens
        .iter()
        .filter(|token| {
            token.ends_with(".js")
                || token.ends_with(".mjs")
                || token.ends_with(".cjs")
                || token.ends_with(".ts")
        })
        .cloned()
        .collect()
}

fn godot_entrypoint_paths(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("run/main_scene=")
                || trimmed.starts_with("application/run/main_scene=")
            {
                return trimmed.split('=').nth(1).map(clean_value);
            }
            None
        })
        .collect()
}

fn parse_path_assignment(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("path") {
        return None;
    }
    trimmed.split('=').nth(1).map(clean_value)
}

fn clean_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(',')
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn config_tokens(contents: &str) -> BTreeSet<String> {
    contents
        .split(|character: char| {
            !character.is_alphanumeric()
                && character != '_'
                && character != '.'
                && character != '/'
                && character != '-'
        })
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn build_file_token_index(structure: &StructurePass) -> HashMap<(String, String), Vec<String>> {
    let mut index = HashMap::new();
    for file in &structure.files {
        let area_key = file.area_id.clone().unwrap_or_default();
        for token in [
            file.path.to_ascii_lowercase(),
            file.name.to_ascii_lowercase(),
            file_stem(&file.path),
        ] {
            index
                .entry((area_key.clone(), token.clone()))
                .or_insert_with(Vec::new)
                .push(file.id.clone());
            index
                .entry((String::new(), token))
                .or_insert_with(Vec::new)
                .push(file.id.clone());
        }
    }
    index
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

fn normalize_config_relative_path(config_dir: &str, value: &str) -> String {
    let value = value.strip_prefix("./").unwrap_or(value);
    if config_dir.is_empty() {
        return value.to_string();
    }
    format!("{config_dir}/{value}")
}

fn normalize_godot_path(value: &str) -> String {
    value.strip_prefix("res://").unwrap_or(value).to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build;
    use crate::model::edge::EdgeKind;
    use crate::passes::{code, structure};
    use crate::repo::discover_repo;

    #[test]
    fn cargo_manifest_links_to_rust_entrypoint() {
        let root = std::env::temp_dir().join("aethyme_engine_config_link_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("Cargo.toml"), "[package]\nname='demo'\n").expect("write config");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write source");

        let snapshot = discover_repo(&root).expect("discover repo");
        let structure = structure::build(&snapshot);
        let code = code::build(&root, &structure);
        let configs = build(&root, &structure, &code);

        assert!(
            configs
                .edges
                .iter()
                .any(|edge| matches!(edge.kind, EdgeKind::EntrypointFor)
                    && edge.to.contains("src/main.rs"))
        );

        let _ = fs::remove_dir_all(&root);
    }
}
