use std::fs;
use std::path::Path;

use crate::config::ConfigNode;
use crate::edge::{Edge, EdgeKind};
use crate::passes::code::CodePass;
use crate::passes::structure::StructurePass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigsPass {
    pub configs: Vec<ConfigNode>,
    pub edges: Vec<Edge>,
}

pub fn build(root: &Path, structure: &StructurePass, code: &CodePass) -> ConfigsPass {
    let mut configs = Vec::new();
    let mut edges = Vec::new();

    for file in &structure.files {
        if !matches!(file.role, crate::file::FileRole::Config) {
            continue;
        }

        let contents = fs::read_to_string(root.join(&file.path)).unwrap_or_default();
        let config_type = classify_config_type(&file.path);
        let config = ConfigNode::new(
            &structure.repo_name,
            &file.id,
            &file.path,
            &config_type,
            file.area_id.clone(),
        );

        edges.push(Edge::new(&file.id, &config.id, EdgeKind::Defines, 1000, "config"));

        if let Some(area_id) = &file.area_id {
            edges.push(Edge::new(&config.id, area_id, EdgeKind::Configures, 800, "config"));
        }

        edges.extend(link_config_to_entrypoints(&config, &contents, structure, code));
        edges.extend(link_config_to_referenced_files(&config, &contents, structure));

        configs.push(config);
    }

    configs.sort();
    edges.sort();
    edges.dedup();
    ConfigsPass { configs, edges }
}

fn classify_config_type(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with("cargo.toml") || lower.ends_with("package.json") || lower.ends_with("pyproject.toml") {
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
    let area_id = config.area_id.clone().unwrap_or_else(|| structure.repo_id.clone());

    match config.config_type.as_str() {
        "manifest" => {
            edges.push(Edge::new(&config.id, &area_id, EdgeKind::EntrypointFor, 700, "config"));

            for function in &code.functions {
                if function.area_id.as_deref() == config.area_id.as_deref() && function.name == "main" {
                    edges.push(Edge::new(&config.id, &function.id, EdgeKind::EntrypointFor, 800, "config"));
                }
            }
        }
        "project" => {
            edges.push(Edge::new(&config.id, &area_id, EdgeKind::EntrypointFor, 800, "config"));
            for file in &structure.files {
                if file.path.ends_with(".tscn") || file.path.ends_with(".gd") {
                    if contents.contains(&file.path) || file.area_id.as_deref() == config.area_id.as_deref() {
                        edges.push(Edge::new(&config.id, &file.id, EdgeKind::Configures, 700, "config"));
                    }
                }
            }
        }
        "runtime" => {
            edges.push(Edge::new(&config.id, &area_id, EdgeKind::Configures, 700, "config"));
        }
        _ => {}
    }

    edges
}

fn link_config_to_referenced_files(config: &ConfigNode, contents: &str, structure: &StructurePass) -> Vec<Edge> {
    let mut edges = Vec::new();

    for file in &structure.files {
        if file.path == config.path {
            continue;
        }
        if contents.contains(&file.path)
            || contents.contains(&format!("\"{}\"", file.path))
            || contents.contains(&format!("'{}'", file.path))
        {
            edges.push(Edge::new(&config.id, &file.id, EdgeKind::Configures, 750, "config"));
        }
    }

    if config.path.ends_with("Cargo.toml") {
        for file in &structure.files {
            if (file.path == "src/main.rs"
                || file.path == "src/lib.rs"
                || file.path.ends_with("/src/main.rs")
                || file.path.ends_with("/src/lib.rs"))
                && (config.area_id.is_none() || file.area_id.as_deref() == config.area_id.as_deref())
            {
                edges.push(Edge::new(&config.id, &file.id, EdgeKind::EntrypointFor, 850, "config"));
            }
        }
    }

    if config.path.ends_with("package.json") {
        for line in contents.lines() {
            let trimmed = line.trim();
            if (trimmed.starts_with("\"main\"") || trimmed.starts_with("\"module\""))
                && let Some(path_value) = trimmed.split(':').nth(1)
            {
                let candidate = path_value
                    .trim()
                    .trim_matches(',')
                    .trim_matches('"')
                    .trim_matches('\'');
                for file in &structure.files {
                    if file.path.ends_with(candidate) {
                        edges.push(Edge::new(&config.id, &file.id, EdgeKind::EntrypointFor, 850, "config"));
                    }
                }
            }
        }
    }

    edges
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::build;
    use crate::edge::EdgeKind;
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

        assert!(configs.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::EntrypointFor) && edge.to.contains("src/main.rs")));

        let _ = fs::remove_dir_all(&root);
    }
}
