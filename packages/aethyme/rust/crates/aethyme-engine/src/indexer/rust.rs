use crate::edge::{Edge, EdgeKind};
use crate::symbol::{Symbol, SymbolKind};

pub fn extract_symbols(path: &str, contents: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();

        for prefix in ["pub fn ", "fn "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split(['(', '<', ' ']).next() {
                    let symbol_name = name.trim().trim_end_matches(';');
                    if !symbol_name.is_empty() {
                        symbols.push(Symbol::new(
                            symbol_name,
                            SymbolKind::Function,
                            path,
                            index + 1,
                            trimmed,
                        ));
                        break;
                    }
                }
            }
        }

        for prefix in ["pub struct ", "struct ", "pub enum ", "enum ", "pub trait ", "trait "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(name) = rest.split(['{', '<', ' ', '(']).next() {
                    let symbol_name = name.trim().trim_end_matches(';');
                    if !symbol_name.is_empty() {
                        symbols.push(Symbol::new(
                            symbol_name,
                            SymbolKind::Class,
                            path,
                            index + 1,
                            trimmed,
                        ));
                        break;
                    }
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("pub const ").or_else(|| trimmed.strip_prefix("const ")) {
            if let Some(name) = rest.split(['=', ':', ' ']).next() {
                let symbol_name = name.trim().trim_end_matches(';');
                if !symbol_name.is_empty() {
                    symbols.push(Symbol::new(
                        symbol_name,
                        SymbolKind::Constant,
                        path,
                        index + 1,
                        trimmed,
                    ));
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
        if let Some(rest) = trimmed.strip_prefix("use ") {
            let target = rest.trim_end_matches(';').trim();
            if !target.is_empty() {
                edges.push(Edge::new(path, target, EdgeKind::Imports, 900, "rust-ast"));
            }
        } else if let Some(rest) = trimmed.strip_prefix("mod ") {
            let target = rest.trim_end_matches(';').trim();
            if !target.is_empty() {
                edges.push(Edge::new(path, target, EdgeKind::Imports, 900, "rust-ast"));
            }
        }
    }

    edges
}
