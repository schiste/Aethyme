use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::edge::Edge;
use crate::indexer;
use crate::repo::{discover_repo, RepoSnapshot};
use crate::risk::{RiskArea, RiskFlag, RiskLevel};
use crate::symbol::Symbol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMap {
    pub snapshot: RepoSnapshot,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    pub risk_flags: Vec<RiskFlag>,
}

impl RepositoryMap {
    pub fn build(root: &Path) -> Result<Self, String> {
        let snapshot = discover_repo(root)?;
        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        let mut risk_flags = BTreeSet::new();

        for file in &snapshot.files {
            let absolute_path = root.join(&file.path);
            let contents = fs::read_to_string(&absolute_path).unwrap_or_default();
            if let Some(language) = &file.language {
                match language.as_str() {
                    "python" => {
                        symbols.extend(indexer::python::extract_symbols(&file.path, &contents));
                        edges.extend(indexer::python::extract_import_edges(&file.path, &contents));
                    }
                    "typescript" | "javascript" => {
                        symbols.extend(indexer::typescript::extract_symbols(&file.path, &contents));
                        edges.extend(indexer::typescript::extract_import_edges(&file.path, &contents));
                    }
                    _ => {}
                }
            }
            for risk in detect_risks(&file.path) {
                risk_flags.insert(risk);
            }
        }

        symbols.sort();
        edges.sort();
        let mut risks: Vec<RiskFlag> = risk_flags.into_iter().collect();
        risks.sort();

        Ok(Self {
            snapshot,
            symbols,
            edges,
            risk_flags: risks,
        })
    }
}

fn detect_risks(path: &str) -> Vec<RiskFlag> {
    let lower = path.to_ascii_lowercase();
    let mut risks = Vec::new();
    if lower.contains("auth") {
        risks.push(RiskFlag::new(path, RiskArea::Auth, RiskLevel::High, "authentication boundary"));
    }
    if lower.contains("permission") || lower.contains("rbac") {
        risks.push(RiskFlag::new(path, RiskArea::Permissions, RiskLevel::High, "permission boundary"));
    }
    if lower.contains("secret") || lower.contains("token") || lower.contains("credential") {
        risks.push(RiskFlag::new(path, RiskArea::Secrets, RiskLevel::High, "sensitive credential surface"));
    }
    if lower.contains("migration") {
        risks.push(RiskFlag::new(path, RiskArea::Migrations, RiskLevel::High, "schema change area"));
    }
    if lower.contains("deploy") || lower.contains("infra") || lower.contains("terraform") {
        risks.push(RiskFlag::new(path, RiskArea::Infra, RiskLevel::High, "infrastructure surface"));
    }
    if lower.contains("billing") || lower.contains("invoice") {
        risks.push(RiskFlag::new(path, RiskArea::Billing, RiskLevel::High, "billing logic"));
    }
    risks
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::RepositoryMap;

    #[test]
    fn build_map_extracts_symbols_and_risks() {
        let root = std::env::temp_dir().join("aethyme_engine_map_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/auth")).expect("create temp repo");
        fs::write(
            root.join("src/auth/service.py"),
            "from app.core import token\nclass AuthService:\n    pass\n\ndef validate_token():\n    return True\n",
        )
        .expect("write source file");

        let map = RepositoryMap::build(&root).expect("build repository map");

        assert!(map.symbols.iter().any(|symbol| symbol.name == "AuthService"));
        assert!(map.symbols.iter().any(|symbol| symbol.name == "validate_token"));
        assert!(map.risk_flags.iter().any(|flag| flag.scope == "src/auth/service.py"));

        let _ = fs::remove_dir_all(&root);
    }
}
