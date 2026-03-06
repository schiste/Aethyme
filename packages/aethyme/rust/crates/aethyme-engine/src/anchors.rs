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
                anchors.push(Anchor::new(
                    AnchorKind::File,
                    readme,
                    Some(readme),
                    "repository readme",
                ));
            }
            for file in &map.snapshot.files {
                let lower = file.path.to_ascii_lowercase();
                if lower.ends_with("main.py")
                    || lower.ends_with("app.py")
                    || lower.ends_with("cli.py")
                    || lower.ends_with("index.ts")
                    || lower.ends_with("main.ts")
                    || lower.ends_with("app.ts")
                {
                    anchors.push(Anchor::new(
                        AnchorKind::File,
                        &file.path,
                        Some(&file.path),
                        "likely entrypoint",
                    ));
                }
            }
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
                        hit.name,
                        Some(hit.file),
                        format!("{} via {}", hit.reason, query),
                    ));
                }
            }
        }
    }

    anchors.sort();
    anchors.dedup();
    anchors.truncate(limit);
    anchors
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
        fs::write(
            root.join("src/auth.py"),
            "def validate_token():\n    return True\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let task = TaskInput::from_task_text("Update validate_token flow");
        let anchors = resolve_anchors(&map, &task, 3);

        assert!(anchors.iter().any(|anchor| anchor.id == "validate_token"));

        let _ = fs::remove_dir_all(&root);
    }
}
