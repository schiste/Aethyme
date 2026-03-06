use crate::edge::{Edge, EdgeKind};
use crate::symbol::{Symbol, SymbolKind};

pub fn extract_symbols(path: &str, contents: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("def ") {
            if let Some(name) = rest.split('(').next() {
                let symbol_name = name.trim();
                if !symbol_name.is_empty() {
                    symbols.push(Symbol::new(symbol_name, SymbolKind::Function, path, index + 1, trimmed));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            if let Some(name) = rest.split(['(', ':']).next() {
                let symbol_name = name.trim();
                if !symbol_name.is_empty() {
                    symbols.push(Symbol::new(symbol_name, SymbolKind::Class, path, index + 1, trimmed));
                }
            }
        }
    }
    symbols
}

pub fn extract_import_edges(path: &str, contents: &str) -> Vec<Edge> {
    let mut edges = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.trim().split_whitespace().next().unwrap_or_default();
                if !module.is_empty() {
                    edges.push(Edge::new(path, module, EdgeKind::Imports, 900, "python-ast"));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("from ") {
            let module = rest.split_whitespace().next().unwrap_or_default();
            if !module.is_empty() {
                edges.push(Edge::new(path, module, EdgeKind::Imports, 900, "python-ast"));
            }
        }
    }
    edges
}
