use crate::context_pack::{Anchor, AnchorKind};
use crate::map::RepositoryMap;
use crate::search::symbol_search;
use crate::task::{TaskInput, TaskKind};

const STOP_WORDS: &[&str] = &[
    "change",
    "changes",
    "update",
    "updates",
    "modify",
    "modifies",
    "the",
    "this",
    "that",
    "repo",
    "repository",
    "component",
    "behavior",
    "flow",
];

pub fn resolve_anchors(map: &RepositoryMap, task: &TaskInput, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();

    match task.kind {
        TaskKind::ExplainRepo => {
            if let Some(readme) = &map.snapshot.readme_path {
                anchors.push(Anchor::new(AnchorKind::File, readme, Some(readme), "repository readme"));
            }
            anchors.extend(explain_repo_doc_anchors(map, 1));
            anchors.extend(explain_repo_area_anchors(map, 2));
            anchors.extend(explain_repo_entrypoint_anchors(map, 1));
            anchors.extend(explain_repo_config_anchors(map, 1));
        }
        _ => {
            let mut queries = candidate_queries(task);
            if queries.is_empty() {
                queries.push(task.normalized.clone());
            }
            for query in queries {
                for hit in symbol_search(map, &query, limit) {
                    anchors.push(Anchor::new(
                        AnchorKind::Symbol,
                        hit.id,
                        Some(hit.file),
                        format!("{} via {}", hit.reason, query),
                    ));
                }
            }
        }
    }

    let mut deduped = Vec::new();
    for anchor in anchors {
        if !deduped.contains(&anchor) {
            deduped.push(anchor);
        }
        if deduped.len() == limit {
            break;
        }
    }
    deduped
}

fn candidate_queries(task: &TaskInput) -> Vec<String> {
    let mut queries = Vec::new();
    for token in task
        .normalized
        .split(|character: char| !character.is_alphanumeric() && character != '_')
    {
        let cleaned = token.trim().to_ascii_lowercase();
        if cleaned.len() < 3 {
            continue;
        }
        if STOP_WORDS.contains(&cleaned.as_str()) {
            continue;
        }
        if !queries.contains(&cleaned) {
            queries.push(cleaned);
        }
    }
    queries
}

fn explain_repo_doc_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut docs: Vec<(i32, Anchor)> = map
        .docs
        .iter()
        .filter(|doc| !doc.path.eq_ignore_ascii_case("README.md") && doc.doc_type != "readme")
        .map(|doc| {
            let lower = doc.path.to_ascii_lowercase();
            let mut score = match doc.doc_type.as_str() {
                "architecture" => 10,
                "guide" => 6,
                _ => 4,
            };
            if lower.contains("documentation/") || lower.contains("docs/") {
                score += 2;
            }
            (
                score,
                Anchor::new(
                    AnchorKind::File,
                    &doc.path,
                    Some(&doc.path),
                    format!("{} document", doc.doc_type),
                ),
            )
        })
        .collect();
    docs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(&right.1.id)));
    docs.truncate(limit);
    docs.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_entrypoint_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for function in &map.functions {
        let lower = function.file_path.to_ascii_lowercase();
        let score = if function.name == "main" {
            10
        } else if lower.ends_with("lib.rs") || lower.ends_with("main.rs") || lower.ends_with("main.py") || lower.ends_with("app.py") || lower.ends_with("cli.py") || lower.ends_with("index.ts") || lower.ends_with("main.ts") {
            7
        } else {
            continue;
        };
        anchors.push((
            score,
            Anchor::new(AnchorKind::File, &function.file_path, Some(&function.file_path), "likely entrypoint"),
        ));
    }
    anchors.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(&right.1.id)));
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_area_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for area in &map.areas {
        let mut score = 2;
        let lower = area.name.to_ascii_lowercase();
        if lower == "src" || lower == "app" || lower == "packages" || lower == "services" || lower == "tools" || lower.contains("engine") {
            score += 5;
        }
        let files = map.files.iter().filter(|file| file.area_id.as_deref() == Some(area.id.as_str())).count() as i32;
        let code = map.functions.iter().filter(|function| function.area_id.as_deref() == Some(area.id.as_str())).count() as i32;
        score += files.min(3) + code.min(3);
        anchors.push((score, Anchor::new(AnchorKind::Folder, &area.name, None::<String>, "top-level area")));
    }
    anchors.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(&right.1.id)));
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

fn explain_repo_config_anchors(map: &RepositoryMap, limit: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for config in &map.configs {
        let score = match config.config_type.as_str() {
            "manifest" | "project" => 6,
            "runtime" => 5,
            _ => 3,
        };
        anchors.push((score, Anchor::new(AnchorKind::File, &config.path, Some(&config.path), format!("{} config", config.config_type))));
    }
    anchors.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(&right.1.id)));
    anchors.truncate(limit);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::resolve_anchors;
    use crate::map::RepositoryMap;
    use crate::task::TaskInput;

    #[test]
    fn change_symbol_task_extracts_useful_symbol_token() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("src/auth.py"), "def validate_token():\n    return True\n").expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text("Update validate_token flow");
        let anchors = resolve_anchors(&map, &task, 3);

        assert!(anchors.iter().any(|anchor| anchor.id.contains("validate_token")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explain_repo_prefers_structural_folder_anchors() {
        let root = std::env::temp_dir().join("aethyme_engine_anchor_repo_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("documentation")).expect("create docs dir");
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::write(root.join("README.md"), "# Demo Repo\n").expect("write readme");
        fs::write(root.join("documentation/technical-architecture.md"), "# Architecture\n").expect("write architecture doc");
        fs::write(root.join("GameEngine/src/main.rs"), "fn main() {}\n").expect("write entrypoint");
        fs::write(root.join("GameEngine/Cargo.toml"), "[package]\nname='demo'\n").expect("write manifest");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text("Explain this repo");
        let anchors = resolve_anchors(&map, &task, 5);

        assert!(anchors.iter().any(|anchor| anchor.id == "README.md"));
        assert!(anchors.iter().any(|anchor| anchor.id == "documentation"));
        assert!(anchors.iter().any(|anchor| anchor.id == "GameEngine"));
        assert!(anchors.iter().any(|anchor| anchor.id.ends_with("technical-architecture.md")));

        let _ = fs::remove_dir_all(&root);
    }
}
