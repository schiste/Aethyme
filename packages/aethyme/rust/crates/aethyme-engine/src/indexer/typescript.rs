use crate::edge::{Edge, EdgeKind};
use crate::symbol::{Symbol, SymbolKind};

pub fn extract_symbols(path: &str, contents: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        for prefix in ["export function ", "function "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split('(').next() {
                    let symbol_name = name.trim();
                    if !symbol_name.is_empty() {
                        symbols.push(Symbol::new(symbol_name, SymbolKind::Function, path, index + 1, trimmed));
                        break;
                    }
                }
            }
        }
        for prefix in ["export class ", "class "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split(['{', '<', ' ']).next() {
                    let symbol_name = name.trim();
                    if !symbol_name.is_empty() {
                        symbols.push(Symbol::new(symbol_name, SymbolKind::Class, path, index + 1, trimmed));
                        break;
                    }
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("export const ") {
            if let Some(name) = rest.split(['=', ':', ' ']).next() {
                let symbol_name = name.trim();
                if !symbol_name.is_empty() {
                    symbols.push(Symbol::new(symbol_name, SymbolKind::Constant, path, index + 1, trimmed));
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
        if let Some(import_target) = trimmed
            .split(" from ")
            .nth(1)
            .and_then(|value| value.split(['\'', '"']).nth(1))
        {
            if !import_target.is_empty() {
                edges.push(Edge::new(path, import_target, EdgeKind::Imports, 900, "typescript-ast"));
            }
        }
    }
    edges
}
