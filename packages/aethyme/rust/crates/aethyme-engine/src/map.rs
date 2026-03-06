use std::path::Path;

use crate::area::AreaNode;
use crate::class::ClassNode;
use crate::config::ConfigNode;
use crate::directory::DirectoryNode;
use crate::doc::DocNode;
use crate::edge::Edge;
use crate::file::FileNode;
use crate::function::FunctionNode;
use crate::graph::{GraphNode, GraphNodeKind, NormalizedGraph};
use crate::passes;
use crate::repo::{discover_repo, RepoSnapshot};
use crate::risk::RiskFlag;
use crate::symbol::Symbol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMap {
    pub snapshot: RepoSnapshot,
    pub areas: Vec<AreaNode>,
    pub directories: Vec<DirectoryNode>,
    pub files: Vec<FileNode>,
    pub classes: Vec<ClassNode>,
    pub functions: Vec<FunctionNode>,
    pub docs: Vec<DocNode>,
    pub configs: Vec<ConfigNode>,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    pub risk_flags: Vec<RiskFlag>,
    pub graph: NormalizedGraph,
}

impl RepositoryMap {
    pub fn build(root: &Path) -> Result<Self, String> {
        let snapshot = discover_repo(root)?;
        let structure = passes::structure::build(&snapshot);
        let code = passes::code::build(root, &structure);
        let docs = passes::docs::build(root, &structure, &code);
        let configs = passes::configs::build(root, &structure, &code);

        let mut edges = Vec::new();
        edges.extend(structure.edges.clone());
        edges.extend(code.edges.clone());
        edges.extend(docs.edges.clone());
        edges.extend(configs.edges.clone());
        edges.sort();
        edges.dedup();

        let mut map = Self {
            snapshot,
            areas: structure.areas,
            directories: structure.directories,
            files: structure.files,
            classes: code.classes,
            functions: code.functions,
            docs: docs.docs,
            configs: configs.configs,
            symbols: code.compatibility_symbols,
            edges,
            risk_flags: Vec::new(),
            graph: NormalizedGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                annotations: Vec::new(),
            },
        };

        map.risk_flags = passes::overlays::detect_risks(&map);
        map.graph = build_graph(&map);
        Ok(map)
    }

    pub fn matching_target_ids(&self, target: &str) -> Vec<String> {
        let lowered = target.to_ascii_lowercase();
        let mut matches = Vec::new();

        if self.graph.nodes.iter().any(|node| node.id == target) {
            matches.push(target.to_string());
        }

        for area in &self.areas {
            if area.id == target || area.name.eq_ignore_ascii_case(&lowered) || area.path_prefix.eq_ignore_ascii_case(&lowered) || area.name.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, area.id.clone());
            }
        }
        for directory in &self.directories {
            if directory.id == target || directory.path.eq_ignore_ascii_case(target) || directory.name.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, directory.id.clone());
            }
        }
        for file in &self.files {
            if file.id == target || file.path.eq_ignore_ascii_case(target) || file.name.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, file.id.clone());
            }
        }
        for class in &self.classes {
            if class.id == target || class.name.eq_ignore_ascii_case(target) || class.qualified_name.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, class.id.clone());
            }
        }
        for function in &self.functions {
            if function.id == target || function.name.eq_ignore_ascii_case(target) || function.qualified_name.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, function.id.clone());
            }
        }
        for doc in &self.docs {
            if doc.id == target || doc.path.eq_ignore_ascii_case(target) || doc.title.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, doc.id.clone());
            }
        }
        for config in &self.configs {
            if config.id == target || config.path.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, config.id.clone());
            }
        }

        if matches.is_empty() {
            matches.push(target.to_string());
        }
        matches
    }

    pub fn display_for(&self, value: &str) -> String {
        if let Some(file) = self.files.iter().find(|file| file.id == value) {
            return file.path.clone();
        }
        if let Some(function) = self.functions.iter().find(|function| function.id == value) {
            return format!("{}::{}", function.file_path, function.name);
        }
        if let Some(class) = self.classes.iter().find(|class| class.id == value) {
            return format!("{}::{}", class.file_path, class.name);
        }
        if let Some(area) = self.areas.iter().find(|area| area.id == value) {
            return area.name.clone();
        }
        if let Some(doc) = self.docs.iter().find(|doc| doc.id == value) {
            return doc.path.clone();
        }
        if let Some(config) = self.configs.iter().find(|config| config.id == value) {
            return config.path.clone();
        }
        value.to_string()
    }
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.contains(&candidate) {
        values.push(candidate);
    }
}

fn build_graph(map: &RepositoryMap) -> NormalizedGraph {
    let mut nodes = Vec::new();
    let repo_name = map.snapshot.repo_name();
    nodes.push(GraphNode {
        id: format!("repo:{repo_name}"),
        kind: GraphNodeKind::Repo,
        label: repo_name,
        path: Some(map.snapshot.root.clone()),
        language: None,
        confidence: 1000,
        source: "structure".to_string(),
        metadata: std::collections::BTreeMap::new(),
    });

    for area in &map.areas {
        nodes.push(GraphNode {
            id: area.id.clone(),
            kind: GraphNodeKind::Area,
            label: area.name.clone(),
            path: Some(area.path_prefix.clone()),
            language: None,
            confidence: 1000,
            source: "structure".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for directory in &map.directories {
        nodes.push(GraphNode {
            id: directory.id.clone(),
            kind: GraphNodeKind::Directory,
            label: directory.name.clone(),
            path: Some(directory.path.clone()),
            language: None,
            confidence: 1000,
            source: "structure".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for file in &map.files {
        nodes.push(GraphNode {
            id: file.id.clone(),
            kind: GraphNodeKind::File,
            label: file.name.clone(),
            path: Some(file.path.clone()),
            language: file.language.clone(),
            confidence: 1000,
            source: "structure".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for class in &map.classes {
        nodes.push(GraphNode {
            id: class.id.clone(),
            kind: GraphNodeKind::Class,
            label: class.name.clone(),
            path: Some(class.file_path.clone()),
            language: Some(class.language.clone()),
            confidence: 1000,
            source: "code".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for function in &map.functions {
        nodes.push(GraphNode {
            id: function.id.clone(),
            kind: GraphNodeKind::Function,
            label: function.name.clone(),
            path: Some(function.file_path.clone()),
            language: Some(function.language.clone()),
            confidence: 1000,
            source: "code".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for doc in &map.docs {
        nodes.push(GraphNode {
            id: doc.id.clone(),
            kind: GraphNodeKind::Doc,
            label: doc.title.clone(),
            path: Some(doc.path.clone()),
            language: None,
            confidence: 900,
            source: "docs".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for config in &map.configs {
        nodes.push(GraphNode {
            id: config.id.clone(),
            kind: GraphNodeKind::Config,
            label: config.config_type.clone(),
            path: Some(config.path.clone()),
            language: None,
            confidence: 900,
            source: "config".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }

    let annotations = passes::overlays::graph_annotations(map);
    let mut graph = NormalizedGraph {
        nodes,
        edges: map.edges.clone(),
        annotations,
    };
    graph.sort();
    graph
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::RepositoryMap;

    #[test]
    fn build_map_creates_graph_layers() {
        let root = std::env::temp_dir().join("aethyme_engine_map_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/auth")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(
            root.join("src/auth/service.py"),
            "from app.core import token\nclass AuthService:\n    pass\n\ndef validate_token():\n    return True\n",
        )
        .expect("write source file");
        fs::write(root.join("src/auth/architecture.md"), "# Auth\n").expect("write docs");
        fs::write(root.join("Cargo.toml"), "[package]\nname = 'demo'\n").expect("write config");

        let map = RepositoryMap::build(&root).expect("build repository map");

        assert!(!map.areas.is_empty());
        assert!(!map.directories.is_empty());
        assert!(map.functions.iter().any(|function| function.name == "validate_token"));
        assert!(map.classes.iter().any(|class| class.name == "AuthService"));
        assert!(!map.docs.is_empty());
        assert!(!map.configs.is_empty());
        assert!(!map.graph.nodes.is_empty());
        assert!(map.risk_flags.iter().any(|flag| flag.scope == "src/auth/service.py"));

        let _ = fs::remove_dir_all(&root);
    }
}
