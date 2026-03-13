use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;

use crate::deps::{extract_external_deps, ExternalDep};
use crate::map::RepositoryMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRepo {
    pub root: String,
    pub name: String,
    pub map: RepositoryMap,
    pub external_deps: Vec<ExternalDep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGraph {
    pub repos: Vec<WorkspaceRepo>,
    pub cross_edges: Vec<CrossRepoEdge>,
    pub shared_dependencies: Vec<SharedDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CrossRepoEdge {
    pub from_repo: String,
    pub from_id: String,
    pub to_repo: String,
    pub to_id: String,
    pub kind: CrossEdgeKind,
    pub confidence: u16,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CrossEdgeKind {
    DependsOn,
    SharedDependency,
    DirectReference,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SharedDependency {
    pub name: String,
    pub repos: Vec<String>,
    pub dep_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlastRadiusItem {
    pub repo: String,
    pub id: String,
    pub display: String,
    pub reason: String,
}

pub fn build_workspace_graph(repo_paths: &[&Path]) -> Result<WorkspaceGraph, String> {
    // Phase 1: Index each repo independently (parallel)
    let indexed: Vec<Result<WorkspaceRepo, String>> = repo_paths
        .par_iter()
        .map(|path| {
            let map = RepositoryMap::build(path)?;
            let name = map.snapshot.repo_name();
            let external_deps = extract_external_deps(path, &map.configs);
            Ok(WorkspaceRepo {
                root: path.to_string_lossy().to_string(),
                name,
                map,
                external_deps,
            })
        })
        .collect();

    let mut repos = Vec::new();
    for result in indexed {
        repos.push(result?);
    }

    // Phase 2: Detect shared dependencies
    let shared_dependencies = detect_shared_dependencies(&repos);

    // Phase 3: Detect cross-repo edges
    let mut cross_edges = Vec::new();
    cross_edges.extend(shared_dependency_edges(&shared_dependencies));
    cross_edges.extend(detect_direct_references(&repos));
    cross_edges.sort();
    cross_edges.dedup();

    Ok(WorkspaceGraph {
        repos,
        cross_edges,
        shared_dependencies,
    })
}

pub fn cross_repo_blast_radius(
    workspace: &WorkspaceGraph,
    repo_name: &str,
    file_path: &str,
) -> Vec<BlastRadiusItem> {
    let mut items = Vec::new();

    let source_repo = workspace.repos.iter().find(|r| r.name == repo_name);
    let source_repo = match source_repo {
        Some(r) => r,
        None => return items,
    };

    // Find symbols defined in the target file
    let file_symbols: Vec<&str> = source_repo
        .map
        .functions
        .iter()
        .filter(|f| f.file_path == file_path)
        .map(|f| f.id.as_str())
        .chain(
            source_repo
                .map
                .classes
                .iter()
                .filter(|c| c.file_path == file_path)
                .map(|c| c.id.as_str()),
        )
        .collect();

    let file_id = source_repo
        .map
        .files
        .iter()
        .find(|f| f.path == file_path)
        .map(|f| f.id.as_str());

    // Follow cross-repo edges from this repo
    for edge in &workspace.cross_edges {
        if edge.from_repo != repo_name {
            continue;
        }
        let is_relevant = file_symbols.iter().any(|s| edge.from_id == *s)
            || file_id.is_some_and(|id| edge.from_id == id);
        if !is_relevant {
            continue;
        }

        // Find display name in target repo
        let display = workspace
            .repos
            .iter()
            .find(|r| r.name == edge.to_repo)
            .map(|r| r.map.display_for(&edge.to_id))
            .unwrap_or_else(|| edge.to_id.clone());

        items.push(BlastRadiusItem {
            repo: edge.to_repo.clone(),
            id: edge.to_id.clone(),
            display,
            reason: edge.evidence.clone(),
        });
    }

    // Also check shared dependency edges targeting this repo
    for edge in &workspace.cross_edges {
        if edge.to_repo != repo_name {
            continue;
        }
        let is_relevant = file_symbols.iter().any(|s| edge.to_id == *s)
            || file_id.is_some_and(|id| edge.to_id == id);
        if !is_relevant {
            continue;
        }

        let display = workspace
            .repos
            .iter()
            .find(|r| r.name == edge.from_repo)
            .map(|r| r.map.display_for(&edge.from_id))
            .unwrap_or_else(|| edge.from_id.clone());

        items.push(BlastRadiusItem {
            repo: edge.from_repo.clone(),
            id: edge.from_id.clone(),
            display,
            reason: format!("reverse: {}", edge.evidence),
        });
    }

    items.sort();
    items.dedup();
    items
}

fn detect_shared_dependencies(repos: &[WorkspaceRepo]) -> Vec<SharedDependency> {
    let mut dep_repos: HashMap<(&str, &str), Vec<String>> = HashMap::new();
    for repo in repos {
        for dep in &repo.external_deps {
            dep_repos
                .entry((&dep.name, &dep.dep_type))
                .or_default()
                .push(repo.name.clone());
        }
    }

    let mut shared = Vec::new();
    for ((name, dep_type), repos) in &dep_repos {
        if repos.len() >= 2 {
            let mut sorted_repos = repos.clone();
            sorted_repos.sort();
            sorted_repos.dedup();
            if sorted_repos.len() >= 2 {
                shared.push(SharedDependency {
                    name: name.to_string(),
                    repos: sorted_repos,
                    dep_type: dep_type.to_string(),
                });
            }
        }
    }
    shared.sort();
    shared
}

fn shared_dependency_edges(shared: &[SharedDependency]) -> Vec<CrossRepoEdge> {
    let mut edges = Vec::new();
    for dep in shared {
        for i in 0..dep.repos.len() {
            for j in (i + 1)..dep.repos.len() {
                edges.push(CrossRepoEdge {
                    from_repo: dep.repos[i].clone(),
                    from_id: format!("dep:{}", dep.name),
                    to_repo: dep.repos[j].clone(),
                    to_id: format!("dep:{}", dep.name),
                    kind: CrossEdgeKind::SharedDependency,
                    confidence: 600,
                    evidence: format!("shared {} dependency: {}", dep.dep_type, dep.name),
                });
            }
        }
    }
    edges
}

fn detect_direct_references(repos: &[WorkspaceRepo]) -> Vec<CrossRepoEdge> {
    let mut edges = Vec::new();

    // Build a lookup of module paths per repo
    let mut module_paths: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for repo in repos {
        let mut paths = HashMap::new();
        for file in &repo.map.files {
            // Index by file path stem (e.g., "auth" from "src/auth.py")
            let stem = file
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&file.path)
                .rsplit('.')
                .last()
                .unwrap_or(&file.path);
            paths.insert(stem, file.id.as_str());
        }
        module_paths.insert(&repo.name, paths);
    }

    // Check unresolved imports against other repos' module paths
    for repo in repos {
        for edge in &repo.map.edges {
            if !edge.to.starts_with("import:") {
                continue;
            }
            let import_name = &edge.to["import:".len()..];
            let last_segment = import_name
                .rsplit('.')
                .next()
                .or_else(|| import_name.rsplit('/').next())
                .or_else(|| import_name.rsplit("::").next())
                .unwrap_or(import_name);

            for other_repo in repos {
                if other_repo.name == repo.name {
                    continue;
                }
                if let Some(paths) = module_paths.get(other_repo.name.as_str()) {
                    if let Some(target_id) = paths.get(last_segment) {
                        edges.push(CrossRepoEdge {
                            from_repo: repo.name.clone(),
                            from_id: edge.from.clone(),
                            to_repo: other_repo.name.clone(),
                            to_id: target_id.to_string(),
                            kind: CrossEdgeKind::DirectReference,
                            confidence: 500,
                            evidence: format!("unresolved import '{}' matches module in {}", import_name, other_repo.name),
                        });
                    }
                }
            }
        }
    }

    edges
}
