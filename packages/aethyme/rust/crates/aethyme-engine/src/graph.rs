use std::collections::BTreeMap;

use crate::edge::Edge;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum GraphNodeKind {
    Repo,
    Area,
    Directory,
    File,
    Class,
    Function,
    Doc,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub confidence: u16,
    pub source: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct GraphAnnotation {
    pub target_id: String,
    pub kind: String,
    pub value: String,
    pub confidence: u16,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NormalizedGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<Edge>,
    pub annotations: Vec<GraphAnnotation>,
}

impl NormalizedGraph {
    pub fn sort(&mut self) {
        self.nodes.sort();
        self.edges.sort();
        self.annotations.sort();
    }
}
