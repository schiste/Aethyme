use crate::map::RepositoryMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub score: i32,
    pub reason: String,
}

pub fn symbol_search(map: &RepositoryMap, query: &str, limit: usize) -> Vec<SearchHit> {
    let lowered_query = query.to_ascii_lowercase();
    let mut hits = Vec::new();

    for class in &map.classes {
        if let Some(score) = score_name(&class.name, &lowered_query) {
            hits.push(SearchHit {
                id: class.id.clone(),
                name: class.name.clone(),
                kind: "class".to_string(),
                file: class.file_path.clone(),
                line: class.line,
                score,
                reason: "class-name-match".to_string(),
            });
        }
    }

    for function in &map.functions {
        if let Some(score) = score_name(&function.name, &lowered_query) {
            hits.push(SearchHit {
                id: function.id.clone(),
                name: function.name.clone(),
                kind: "function".to_string(),
                file: function.file_path.clone(),
                line: function.line,
                score,
                reason: "function-name-match".to_string(),
            });
        }
    }

    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    hits.truncate(limit);
    hits
}

fn score_name(name: &str, lowered_query: &str) -> Option<i32> {
    let lowered_name = name.to_ascii_lowercase();
    if lowered_name == lowered_query {
        Some(300)
    } else if lowered_name.starts_with(lowered_query) {
        Some(200)
    } else if lowered_name.contains(lowered_query) {
        Some(100)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::symbol_search;
    use crate::map::RepositoryMap;

    #[test]
    fn symbol_search_prefers_exact_matches() {
        let root = std::env::temp_dir().join("aethyme_engine_search_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(
            root.join("src/main.py"),
            "def main():\n    return 1\n\ndef main_helper():\n    return 2\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let hits = symbol_search(&map, "main", 10);

        assert_eq!(hits.first().map(|item| item.name.as_str()), Some("main"));

        let _ = fs::remove_dir_all(&root);
    }
}
