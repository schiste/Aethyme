use std::collections::BTreeSet;

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

    for top_level in &snapshot.top_level_dirs {
        let area = AreaNode::new(&repo_name, top_level, false);
        edges.push(Edge::new(&repo_id, &area.id, EdgeKind::Contains, 1000, "structure"));
        areas.push(area);
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

        directories.push(directory);
    }

    for repo_file in &snapshot.files {
        let area_id = area_id_for_path(&repo_name, &repo_file.path, &snapshot.top_level_dirs);
        let generated = is_generated(&repo_file.path);
        let role = classify_file(repo_file, generated);
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
    if lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.ends_with("project.godot")
        || lower.ends_with("pyproject.toml")
        || lower.ends_with("dockerfile")
        || lower.ends_with("docker-compose.yml")
        || lower.ends_with("docker-compose.yaml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".toml")
        || lower.ends_with(".json")
    {
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
