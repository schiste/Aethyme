use crate::model::edge::{Edge, EdgeKind};
use crate::model::symbol::{Symbol, SymbolKind};

const FUNCTION_PREFIXES: &[&str] = &[
    "public static function ",
    "protected static function ",
    "private static function ",
    "public function ",
    "protected function ",
    "private function ",
    "static function ",
    "export function ",
    "export async function ",
    "export default function ",
    "async function ",
    "async def ",
    "function ",
    "def ",
    "pub async fn ",
    "pub fn ",
    "fn ",
    "func ",
    "fun ",
    "sub ",
    "proc ",
];

const CLASS_PREFIXES: &[&str] = &[
    "export abstract class ",
    "export default class ",
    "export class ",
    "abstract class ",
    "data class ",
    "pub struct ",
    "pub enum ",
    "pub trait ",
    "class ",
    "struct ",
    "enum ",
    "trait ",
    "interface ",
];

const IMPORT_PREFIXES: &[&str] = &[
    "from ",
    "import ",
    "require_once ",
    "include_once ",
    "require ",
    "include ",
    "#include ",
    "using ",
    "use ",
];

pub fn extract_symbols(path: &str, contents: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some((name, kind)) = try_extract_symbol(trimmed) {
            if !name.is_empty() {
                symbols.push(Symbol::new(&name, kind, path, index + 1, trimmed));
            }
        }
    }
    symbols
}

pub fn extract_import_edges(path: &str, contents: &str) -> Vec<Edge> {
    let mut edges = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(target) = try_extract_import(trimmed) {
            if !target.is_empty() {
                edges.push(Edge::new(
                    path,
                    &target,
                    EdgeKind::Imports,
                    600,
                    "heuristic",
                ));
            }
        }
    }
    edges
}

fn try_extract_symbol(trimmed: &str) -> Option<(String, SymbolKind)> {
    for prefix in FUNCTION_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name = extract_identifier(rest)?;
            return Some((name, SymbolKind::Function));
        }
    }
    for prefix in CLASS_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name = extract_identifier(rest)?;
            return Some((name, SymbolKind::Class));
        }
    }
    None
}

fn extract_identifier(text: &str) -> Option<String> {
    let name: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn try_extract_import(trimmed: &str) -> Option<String> {
    for prefix in IMPORT_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return extract_import_target(rest);
        }
    }
    // Handle JS/TS: import ... from "target"
    if trimmed.starts_with("import ") {
        if let Some(from_part) = trimmed.split(" from ").nth(1) {
            return extract_quoted_string(from_part);
        }
    }
    None
}

fn extract_import_target(text: &str) -> Option<String> {
    let text = text.trim();
    // Try quoted string first: "foo" or 'foo' or <foo>
    if let Some(target) = extract_quoted_string(text) {
        return Some(target);
    }
    // Otherwise take the first whitespace-delimited token
    let token = text.split_whitespace().next().unwrap_or_default();
    let cleaned = token.trim_end_matches(';').trim_end_matches(',');
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn extract_quoted_string(text: &str) -> Option<String> {
    for delimiter in ['"', '\''] {
        if let Some(start) = text.find(delimiter) {
            if let Some(end) = text[start + 1..].find(delimiter) {
                let value = &text[start + 1..start + 1 + end];
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    // Try angle brackets for #include <foo>
    if let Some(start) = text.find('<') {
        if let Some(end) = text.find('>') {
            if end > start + 1 {
                return Some(text[start + 1..end].to_string());
            }
        }
    }
    None
}
