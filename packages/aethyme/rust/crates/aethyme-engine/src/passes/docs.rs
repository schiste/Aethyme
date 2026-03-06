use std::fs;
use std::path::Path;

use crate::doc::DocNode;
use crate::edge::{Edge, EdgeKind};
use crate::passes::code::CodePass;
use crate::passes::structure::StructurePass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsPass {
    pub docs: Vec<DocNode>,
    pub edges: Vec<Edge>,
}

pub fn build(root: &Path, structure: &StructurePass, code: &CodePass) -> DocsPass {
    let mut docs = Vec::new();
    let mut edges = Vec::new();

    for file in &structure.files {
        if !matches!(file.role, crate::file::FileRole::Doc) {
            continue;
        }

        let contents = fs::read_to_string(root.join(&file.path)).unwrap_or_default();
        let title = read_title(&contents).unwrap_or_else(|| file.name.clone());
        let doc_type = classify_doc_type(&file.path, &title);
        let doc = DocNode::new(
            &structure.repo_name,
            &file.id,
            &file.path,
            &title,
            &doc_type,
            file.area_id.clone(),
        );

        edges.push(Edge::new(&file.id, &doc.id, EdgeKind::Defines, 1000, "docs"));
        if let Some(area_id) = &file.area_id {
            edges.push(Edge::new(&doc.id, area_id, EdgeKind::Documents, 800, "docs"));
        } else {
            edges.push(Edge::new(&doc.id, &structure.repo_id, EdgeKind::Documents, 800, "docs"));
        }

        edges.extend(link_doc_to_areas(&doc, &contents, structure));
        edges.extend(link_doc_to_files(&doc, &contents, structure));
        edges.extend(link_doc_to_symbols(&doc, &contents, code));

        docs.push(doc);
    }

    docs.sort();
    edges.sort();
    edges.dedup();
    DocsPass { docs, edges }
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

fn link_doc_to_areas(doc: &DocNode, contents: &str, structure: &StructurePass) -> Vec<Edge> {
    let mut edges = Vec::new();
    let lower = contents.to_ascii_lowercase();
    for area in &structure.areas {
        if doc.area_id.as_deref() == Some(area.id.as_str()) {
            continue;
        }
        let area_name = area.name.to_ascii_lowercase();
        if lower.contains(&area_name) || doc.path.starts_with(&format!("{}/", area.name)) {
            edges.push(Edge::new(&doc.id, &area.id, EdgeKind::Documents, 700, "docs"));
        }
    }
    edges
}

fn link_doc_to_files(doc: &DocNode, contents: &str, structure: &StructurePass) -> Vec<Edge> {
    let mut edges = Vec::new();
    for file in &structure.files {
        if file.path == doc.path {
            continue;
        }
        if contents.contains(&file.path) || contents.contains(&format!("`{}`", file.path)) {
            edges.push(Edge::new(&doc.id, &file.id, EdgeKind::Documents, 750, "docs"));
        } else if !file.name.is_empty() && contents.contains(&file.name) {
            edges.push(Edge::new(&doc.id, &file.id, EdgeKind::References, 500, "docs"));
        }
    }
    edges
}

fn link_doc_to_symbols(doc: &DocNode, contents: &str, code: &CodePass) -> Vec<Edge> {
    let mut edges = Vec::new();
    for class in &code.classes {
        if contents.contains(&format!("`{}`", class.name)) || contents.contains(&class.name) {
            edges.push(Edge::new(&doc.id, &class.id, EdgeKind::References, 450, "docs"));
        }
    }
    for function in &code.functions {
        if contents.contains(&format!("`{}`", function.name)) || contents.contains(&function.name) {
            edges.push(Edge::new(&doc.id, &function.id, EdgeKind::References, 450, "docs"));
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
        let docs = build(&root, &structure, &code);

        assert!(docs.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::Documents) && edge.to.contains("src/main.py")));
        assert!(docs.edges.iter().any(|edge| matches!(edge.kind, EdgeKind::References) && edge.to.contains("main")));

        let _ = fs::remove_dir_all(&root);
    }
}
