pub mod heuristic;
pub mod resolve;
pub mod tree_sitter;

use crate::edge::Edge;
use crate::symbol::Symbol;

use self::tree_sitter::GrammarRegistry;

pub fn extract_symbols(
    registry: Option<&GrammarRegistry>,
    language: &str,
    path: &str,
    contents: &str,
) -> Vec<Symbol> {
    if let Some(reg) = registry {
        if let Some((symbols, _)) = tree_sitter::extract(reg, language, path, contents) {
            if !symbols.is_empty() {
                return symbols;
            }
        }
    }
    heuristic::extract_symbols(path, contents)
}

pub fn extract_import_edges(
    registry: Option<&GrammarRegistry>,
    language: &str,
    path: &str,
    contents: &str,
) -> Vec<Edge> {
    if let Some(reg) = registry {
        if let Some((_, edges)) = tree_sitter::extract(reg, language, path, contents) {
            if !edges.is_empty() {
                return edges;
            }
        }
    }
    heuristic::extract_import_edges(path, contents)
}
