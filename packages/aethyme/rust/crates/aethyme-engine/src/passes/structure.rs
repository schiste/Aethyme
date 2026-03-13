use std::collections::{BTreeMap, BTreeSet};

use crate::area::AreaNode;
use crate::directory::DirectoryNode;
use crate::edge::{Edge, EdgeKind};
use crate::file::{FileNode, FileRole};
use crate::repo::{RepoFile, RepoSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructurePass {
    pub repo_id: String,
    pub repo_name: String,
    pub areas: Vec<AreaNode>,
    pub directories: Vec<DirectoryNode>,
    pub files: Vec<FileNode>,
    pub edges: Vec<Edge>,
}

pub fn build(snapshot: &RepoSnapshot) -> StructurePass {
    let repo_name = snapshot.repo_name();
    let repo_id = format!("repo:{repo_name}");
    let mut areas = Vec::new();
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut edges = Vec::new();
    let classified_files = snapshot
        .files
        .iter()
        .map(|repo_file| {
            let generated = is_generated(&repo_file.path);
            let role = classify_file(repo_file, generated);
            (repo_file, generated, role)
        })
        .collect::<Vec<_>>();

    for top_level in &snapshot.top_level_dirs {
        let area = AreaNode::new(&repo_name, top_level, false);
        edges.push(Edge::new(&repo_id, &area.id, EdgeKind::Contains, 1000, "structure"));
        areas.push(area);
    }

    for inferred in infer_subareas(&repo_name, &classified_files, &snapshot.top_level_dirs) {
        if let Some(parent_path) = parent_path(&inferred.path_prefix) {
            let parent_id = format!("area:{repo_name}:{parent_path}");
            edges.push(Edge::new(&parent_id, &inferred.id, EdgeKind::Contains, 700, "structure"));
            edges.push(Edge::new(&inferred.id, &parent_id, EdgeKind::BelongsTo, 700, "structure"));
        } else {
            edges.push(Edge::new(&repo_id, &inferred.id, EdgeKind::Contains, 700, "structure"));
        }
        areas.push(inferred);
    }

    let mut directory_paths = BTreeSet::new();
    for file in &snapshot.files {
        for directory in parent_directories(&file.path) {
            directory_paths.insert(directory);
        }
    }

    for path in directory_paths {
        let area_id = area_id_for_path(&repo_name, &path, &snapshot.top_level_dirs);
        let directory = DirectoryNode::new(&repo_name, &path, area_id.clone());

        if let Some(parent_path) = parent_path(&path) {
            let parent_id = format!("dir:{repo_name}:{parent_path}");
            edges.push(Edge::new(&parent_id, &directory.id, EdgeKind::Contains, 1000, "structure"));
        } else if let Some(area_id_value) = &area_id {
            edges.push(Edge::new(area_id_value, &directory.id, EdgeKind::Contains, 1000, "structure"));
        } else {
            edges.push(Edge::new(&repo_id, &directory.id, EdgeKind::Contains, 1000, "structure"));
        }

        if let Some(area_id_value) = &area_id {
            edges.push(Edge::new(&directory.id, area_id_value, EdgeKind::BelongsTo, 1000, "structure"));
        }
        for inferred_area_id in inferred_area_ids_for_path(&directory.path, &areas) {
            edges.push(Edge::new(&directory.id, &inferred_area_id, EdgeKind::BelongsTo, 700, "structure"));
        }

        directories.push(directory);
    }

    for (repo_file, generated, role) in classified_files {
        let area_id = area_id_for_path(&repo_name, &repo_file.path, &snapshot.top_level_dirs);
        let file = FileNode::new(
            &repo_name,
            &repo_file.path,
            repo_file.language.clone(),
            role,
            repo_file.line_count,
            repo_file.size_bytes,
            generated,
            area_id.clone(),
        );

        if let Some(parent_path_value) = parent_path(&repo_file.path) {
            let parent_id = format!("dir:{repo_name}:{parent_path_value}");
            edges.push(Edge::new(&parent_id, &file.id, EdgeKind::Contains, 1000, "structure"));
        } else if let Some(area_id_value) = &area_id {
            edges.push(Edge::new(area_id_value, &file.id, EdgeKind::Contains, 1000, "structure"));
        } else {
            edges.push(Edge::new(&repo_id, &file.id, EdgeKind::Contains, 1000, "structure"));
        }

        if let Some(area_id_value) = &area_id {
            edges.push(Edge::new(&file.id, area_id_value, EdgeKind::BelongsTo, 1000, "structure"));
        }
        for inferred_area_id in inferred_area_ids_for_path(&repo_file.path, &areas) {
            edges.push(Edge::new(&file.id, &inferred_area_id, EdgeKind::BelongsTo, 700, "structure"));
        }

        files.push(file);
    }

    areas.sort();
    directories.sort();
    files.sort();
    edges.sort();

    StructurePass {
        repo_id,
        repo_name,
        areas,
        directories,
        files,
        edges,
    }
}

fn infer_subareas(
    repo_name: &str,
    files: &[(&RepoFile, bool, FileRole)],
    top_level_dirs: &[String],
) -> Vec<AreaNode> {
    let mut candidates = BTreeMap::<String, (usize, usize, usize, usize)>::new();
    for (file, _generated, role) in files {
        let mut segments = file.path.split('/');
        let Some(top) = segments.next() else {
            continue;
        };
        let Some(second) = segments.next() else {
            continue;
        };
        if !top_level_dirs.iter().any(|value| value == top) {
            continue;
        }
        let candidate = format!("{top}/{second}");
        if candidate == top {
            continue;
        }
        let entry = candidates.entry(candidate).or_default();
        match role {
            FileRole::Generated | FileRole::Cache | FileRole::Binary | FileRole::Unknown => {}
            _ => entry.0 += 1,
        }
        match role {
            FileRole::Source => entry.1 += 1,
            FileRole::Doc => entry.2 += 1,
            FileRole::Config => entry.3 += 1,
            FileRole::Generated | FileRole::Cache | FileRole::Binary | FileRole::Unknown | FileRole::Test | FileRole::Asset => {}
        }
    }

    candidates
        .into_iter()
        .filter(|(_candidate, (relevant, source, docs, configs))| {
            *relevant >= 2 && (*source > 0 || *docs > 0 || *configs > 0)
        })
        .map(|(candidate, _)| AreaNode::new(repo_name, &candidate, true))
        .collect()
}

fn inferred_area_ids_for_path(path: &str, areas: &[AreaNode]) -> Vec<String> {
    let mut matches = areas
        .iter()
        .filter(|area| area.inferred)
        .filter(|area| path.starts_with(&format!("{}/", area.path_prefix)) || path == area.path_prefix)
        .map(|area| area.id.clone())
        .collect::<Vec<_>>();
    matches.sort_by_key(String::len);
    matches
}

pub fn area_id_for_path(repo_name: &str, path: &str, top_level_dirs: &[String]) -> Option<String> {
    let first_component = path.split('/').next()?;
    if top_level_dirs.iter().any(|value| value == first_component) {
        Some(format!("area:{repo_name}:{first_component}"))
    } else {
        None
    }
}

fn parent_directories(path: &str) -> Vec<String> {
    let mut directories = Vec::new();
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return directories;
    }
    for index in 0..parts.len() - 1 {
        directories.push(parts[..=index].join("/"));
    }
    directories
}

fn parent_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return None;
    }
    parts.pop();
    Some(parts.join("/"))
}

fn is_generated(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("generated") || lower.ends_with(".min.js") || lower.ends_with(".lock")
}

fn classify_file(repo_file: &RepoFile, generated: bool) -> FileRole {
    if generated {
        return FileRole::Generated;
    }
    let lower = repo_file.path.to_ascii_lowercase();
    if lower.contains("__pycache__") || lower.contains(".pytest_cache") || lower.contains(".mypy_cache") {
        return FileRole::Cache;
    }
    if lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".rst") || lower.ends_with("readme") {
        return FileRole::Doc;
    }
    if is_operational_config_path(&lower) {
        return FileRole::Config;
    }
    if lower.contains("test") || lower.contains("spec") {
        return FileRole::Test;
    }
    if repo_file.language.is_some() {
        return FileRole::Source;
    }
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".svg")
        || lower.ends_with(".db")
        || lower.ends_with(".sqlite")
        || lower.ends_with(".tres")
        || lower.ends_with(".tscn")
    {
        return FileRole::Asset;
    }
    FileRole::Unknown
}

fn is_operational_config_path(lower_path: &str) -> bool {
    let file_name = lower_path.rsplit('/').next().unwrap_or(lower_path);

    if matches!(
        file_name,
        "cargo.toml"
            | "package.json"
            | "pyproject.toml"
            | "project.godot"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "tsconfig.json"
            | "jsconfig.json"
            | "turbo.json"
            | "biome.json"
            | "deno.json"
            | "deno.jsonc"
            | "pnpm-workspace.yaml"
            | "pnpm-workspace.yml"
    ) {
        return true;
    }

    if file_name.starts_with(".env")
        || file_name.starts_with(".gitignore")
        || file_name.starts_with(".dockerignore")
        || file_name.starts_with(".npmrc")
        || file_name.starts_with(".yarnrc")
        || file_name.starts_with(".prettierrc")
        || file_name.starts_with(".eslintrc")
        || file_name.starts_with(".stylelintrc")
        || file_name.starts_with(".editorconfig")
    {
        return true;
    }

    if lower_path.starts_with(".github/workflows/")
        || lower_path.contains("/.github/workflows/")
        || lower_path.starts_with("config/")
        || lower_path.contains("/config/")
        || lower_path.starts_with("configs/")
        || lower_path.contains("/configs/")
        || lower_path.starts_with("deploy/")
        || lower_path.contains("/deploy/")
        || lower_path.starts_with("deployment/")
        || lower_path.contains("/deployment/")
        || lower_path.starts_with("infra/")
        || lower_path.contains("/infra/")
        || lower_path.starts_with("k8s/")
        || lower_path.contains("/k8s/")
        || lower_path.starts_with("helm/")
        || lower_path.contains("/helm/")
    {
        return lower_path.ends_with(".yaml")
            || lower_path.ends_with(".yml")
            || lower_path.ends_with(".toml")
            || lower_path.ends_with(".json");
    }

    lower_path.ends_with(".env")
}

#[cfg(test)]
mod tests {
    use crate::repo::RepoFile;

    use super::{classify_file, FileRole};

    #[test]
    fn classify_file_limits_json_to_operational_config_paths() {
        let package_json = RepoFile {
            path: "frontend/package.json".to_string(),
            language: None,
            line_count: 0,
            size_bytes: 0,
        };
        let content_json = RepoFile {
            path: "content/object-templates.json".to_string(),
            language: None,
            line_count: 0,
            size_bytes: 0,
        };
        let workflow_yaml = RepoFile {
            path: ".github/workflows/ci.yml".to_string(),
            language: None,
            line_count: 0,
            size_bytes: 0,
        };

        assert_eq!(classify_file(&package_json, false), FileRole::Config);
        assert_eq!(classify_file(&workflow_yaml, false), FileRole::Config);
        assert_ne!(classify_file(&content_json, false), FileRole::Config);
    }
}
