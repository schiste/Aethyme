use crate::map::RepositoryMap;
use crate::symbol::SymbolKind;

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

    for symbol in &map.symbols {
        let lowered_symbol = symbol.name.to_ascii_lowercase();
        let score = if lowered_symbol == lowered_query {
            300
        } else if lowered_symbol.starts_with(&lowered_query) {
            200
        } else if lowered_symbol.contains(&lowered_query) {
            100
        } else {
            continue;
        };
        hits.push(SearchHit {
            id: symbol.id.clone(),
            name: symbol.name.clone(),
            kind: match symbol.kind {
                SymbolKind::Function => "function",
                SymbolKind::Class => "class",
                SymbolKind::Constant => "constant",
            }
            .to_string(),
            file: symbol.file.clone(),
            line: symbol.line,
            score,
            reason: "symbol-name-match".to_string(),
        });
    }

    hits.sort_by(|left, right| {
        right.score.cmp(&left.score)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    hits.truncate(limit);
    hits
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
