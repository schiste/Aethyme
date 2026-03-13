use std::collections::HashMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::edge::{Edge, EdgeKind};
use crate::json::escape;
use crate::symbol::{Symbol, SymbolKind};

const ENGINE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-cache-v2-treesitter");

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
    pub import_edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct ParseCache {
    pub engine_version: String,
    pub entries: HashMap<String, CacheEntry>,
}

pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl ParseCache {
    pub fn new() -> Self {
        Self {
            engine_version: ENGINE_VERSION.to_string(),
            entries: HashMap::new(),
        }
    }

    pub fn load(repo_root: &Path) -> Option<Self> {
        let cache_path = cache_file_path(repo_root);
        let contents = fs::read_to_string(&cache_path).ok()?;
        let cache = deserialize_cache(&contents)?;
        if cache.engine_version != ENGINE_VERSION {
            return None;
        }
        Some(cache)
    }

    pub fn save(&self, repo_root: &Path) {
        let cache_path = cache_file_path(repo_root);
        if let Some(parent) = cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        ensure_aethyme_gitignore(repo_root);
        let serialized = serialize_cache(self);
        let _ = fs::write(&cache_path, serialized);
    }

    pub fn lookup(&self, file_path: &str, content_hash: &str) -> Option<&CacheEntry> {
        self.entries
            .get(file_path)
            .filter(|entry| entry.content_hash == content_hash)
    }

    pub fn insert(&mut self, file_path: String, entry: CacheEntry) {
        self.entries.insert(file_path, entry);
    }
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_file_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(".aethyme").join("cache").join("parse-cache.json")
}

/// Create a `.gitignore` inside `.aethyme/` so the cache is automatically
/// ignored in any repo the engine indexes — no manual `.gitignore` edit needed.
fn ensure_aethyme_gitignore(repo_root: &Path) {
    let gitignore = repo_root.join(".aethyme").join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "# Auto-generated — Aethyme graph cache (recomputed locally)\n*\n");
    }
}

fn serialize_cache(cache: &ParseCache) -> String {
    let mut entries_json = Vec::new();
    for (path, entry) in &cache.entries {
        let symbols_json: Vec<String> = entry
            .symbols
            .iter()
            .map(|s| {
                format!(
                    "{{\"id\":\"{}\",\"name\":\"{}\",\"kind\":\"{}\",\"file\":\"{}\",\"line\":{},\"signature\":\"{}\"}}",
                    escape(&s.id),
                    escape(&s.name),
                    symbol_kind_str(&s.kind),
                    escape(&s.file),
                    s.line,
                    escape(&s.signature)
                )
            })
            .collect();
        let edges_json: Vec<String> = entry
            .import_edges
            .iter()
            .map(|e| {
                format!(
                    "{{\"from\":\"{}\",\"to\":\"{}\",\"kind\":\"{}\",\"confidence\":{},\"source\":\"{}\"}}",
                    escape(&e.from),
                    escape(&e.to),
                    edge_kind_str(&e.kind),
                    e.confidence,
                    escape(&e.source)
                )
            })
            .collect();
        entries_json.push(format!(
            "\"{}\":{{\"content_hash\":\"{}\",\"symbols\":[{}],\"import_edges\":[{}]}}",
            escape(path),
            escape(&entry.content_hash),
            symbols_json.join(","),
            edges_json.join(",")
        ));
    }
    format!(
        "{{\"engine_version\":\"{}\",\"entries\":{{{}}}}}",
        escape(&cache.engine_version),
        entries_json.join(",")
    )
}

fn deserialize_cache(json: &str) -> Option<ParseCache> {
    let json = json.trim();
    if !json.starts_with('{') || !json.ends_with('}') {
        return None;
    }
    let inner = &json[1..json.len() - 1];

    let engine_version = extract_string_field(inner, "engine_version")?;
    let entries_start = inner.find("\"entries\"")?;
    let colon_pos = inner[entries_start..].find(':')? + entries_start;
    let entries_obj = extract_object(&inner[colon_pos + 1..])?;

    let mut entries = HashMap::new();
    let mut remaining = entries_obj.trim();
    if remaining.starts_with('{') {
        remaining = &remaining[1..remaining.len() - 1];
    }

    while !remaining.trim().is_empty() {
        remaining = remaining.trim().trim_start_matches(',').trim();
        if remaining.is_empty() {
            break;
        }
        let (key, after_key) = extract_json_string(remaining)?;
        let after_colon = after_key.trim().strip_prefix(':')?;
        let entry_obj = extract_object(after_colon.trim())?;
        let entry_inner = &entry_obj[1..entry_obj.len() - 1];

        let content_hash = extract_string_field(entry_inner, "content_hash")?;
        let symbols = parse_symbols_array(entry_inner)?;
        let import_edges = parse_edges_array(entry_inner)?;

        entries.insert(
            key,
            CacheEntry {
                content_hash,
                symbols,
                import_edges,
            },
        );

        let consumed = after_colon.trim().find(entry_obj.as_str())? + entry_obj.len();
        remaining = &after_colon.trim()[consumed..];
    }

    Some(ParseCache {
        engine_version,
        entries,
    })
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim();
    let (value, _) = extract_json_string(after_colon)?;
    Some(value)
}

fn extract_json_string(json: &str) -> Option<(String, &str)> {
    let json = json.trim();
    if !json.starts_with('"') {
        return None;
    }
    let mut escaped = false;
    let mut end = 1;
    let bytes = json.as_bytes();
    while end < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[end] == b'\\' {
            escaped = true;
        } else if bytes[end] == b'"' {
            let raw = &json[1..end];
            let unescaped = raw.replace("\\\"", "\"").replace("\\\\", "\\").replace("\\n", "\n");
            return Some((unescaped, &json[end + 1..]));
        }
        end += 1;
    }
    None
}

fn extract_object(json: &str) -> Option<String> {
    let json = json.trim();
    if !json.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in json.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(json[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_array(json: &str) -> Option<String> {
    let json = json.trim();
    if !json.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in json.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(json[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn split_array_elements(array_str: &str) -> Vec<String> {
    let inner = array_str.trim();
    if !inner.starts_with('[') || !inner.ends_with(']') {
        return Vec::new();
    }
    let inner = &inner[1..inner.len() - 1].trim();
    if inner.is_empty() {
        return Vec::new();
    }
    let mut elements = Vec::new();
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0;
    for (i, ch) in inner.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' | '[' if !in_string => depth += 1,
            '}' | ']' if !in_string => depth -= 1,
            ',' if !in_string && depth == 0 => {
                elements.push(inner[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = inner[start..].trim();
    if !last.is_empty() {
        elements.push(last.to_string());
    }
    elements
}

fn parse_symbols_array(json: &str) -> Option<Vec<Symbol>> {
    let pattern = "\"symbols\"";
    let start = json.find(pattern)?;
    let after = &json[start + pattern.len()..];
    let colon = after.find(':')?;
    let arr = extract_array(after[colon + 1..].trim())?;
    let elements = split_array_elements(&arr);
    let mut symbols = Vec::new();
    for elem in elements {
        let inner = &elem[1..elem.len() - 1];
        let id = extract_string_field(inner, "id")?;
        let name = extract_string_field(inner, "name")?;
        let kind_str = extract_string_field(inner, "kind")?;
        let file = extract_string_field(inner, "file")?;
        let line = extract_number_field(inner, "line")?;
        let signature = extract_string_field(inner, "signature")?;
        let kind = match kind_str.as_str() {
            "function" => SymbolKind::Function,
            "class" => SymbolKind::Class,
            "constant" => SymbolKind::Constant,
            _ => continue,
        };
        symbols.push(Symbol {
            id,
            name,
            kind,
            file,
            line,
            signature,
            language: None,
            area: None,
        });
    }
    Some(symbols)
}

fn parse_edges_array(json: &str) -> Option<Vec<Edge>> {
    let pattern = "\"import_edges\"";
    let start = json.find(pattern)?;
    let after = &json[start + pattern.len()..];
    let colon = after.find(':')?;
    let arr = extract_array(after[colon + 1..].trim())?;
    let elements = split_array_elements(&arr);
    let mut edges = Vec::new();
    for elem in elements {
        let inner = &elem[1..elem.len() - 1];
        let from = extract_string_field(inner, "from")?;
        let to = extract_string_field(inner, "to")?;
        let kind_str = extract_string_field(inner, "kind")?;
        let confidence = extract_number_field(inner, "confidence")? as u16;
        let source = extract_string_field(inner, "source")?;
        let kind = parse_edge_kind(&kind_str)?;
        edges.push(Edge {
            from,
            to,
            kind,
            confidence,
            source,
        });
    }
    Some(edges)
}

fn extract_number_field(json: &str, field: &str) -> Option<usize> {
    let pattern = format!("\"{}\"", field);
    let start = json.find(&pattern)?;
    let after = &json[start + pattern.len()..];
    let colon = after.find(':')?;
    let after_colon = after[colon + 1..].trim();
    let end = after_colon
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_colon.len());
    after_colon[..end].parse().ok()
}

fn symbol_kind_str(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Constant => "constant",
    }
}

fn edge_kind_str(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::BelongsTo => "belongs_to",
        EdgeKind::Defines => "defines",
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::Documents => "documents",
        EdgeKind::Configures => "configures",
        EdgeKind::EntrypointFor => "entrypoint_for",
    }
}

fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
    match s {
        "contains" => Some(EdgeKind::Contains),
        "belongs_to" => Some(EdgeKind::BelongsTo),
        "defines" => Some(EdgeKind::Defines),
        "imports" => Some(EdgeKind::Imports),
        "calls" => Some(EdgeKind::Calls),
        "references" => Some(EdgeKind::References),
        "documents" => Some(EdgeKind::Documents),
        "configures" => Some(EdgeKind::Configures),
        "entrypoint_for" => Some(EdgeKind::EntrypointFor),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty_cache() {
        let cache = ParseCache::new();
        let serialized = serialize_cache(&cache);
        let deserialized = deserialize_cache(&serialized).expect("should deserialize");
        assert_eq!(deserialized.engine_version, cache.engine_version);
        assert!(deserialized.entries.is_empty());
    }

    #[test]
    fn round_trip_with_entries() {
        let mut cache = ParseCache::new();
        cache.insert(
            "src/main.py".to_string(),
            CacheEntry {
                content_hash: "abc123".to_string(),
                symbols: vec![Symbol {
                    id: "fn:test:src/main.py:run".to_string(),
                    name: "run".to_string(),
                    kind: SymbolKind::Function,
                    file: "src/main.py".to_string(),
                    line: 1,
                    signature: "def run()".to_string(),
                    language: None,
                    area: None,
                }],
                import_edges: vec![Edge::new("file:src/main.py", "file:src/auth.py", EdgeKind::Imports, 900, "python")],
            },
        );
        let serialized = serialize_cache(&cache);
        let deserialized = deserialize_cache(&serialized).expect("should deserialize");
        assert_eq!(deserialized.entries.len(), 1);
        let entry = deserialized.entries.get("src/main.py").expect("entry exists");
        assert_eq!(entry.content_hash, "abc123");
        assert_eq!(entry.symbols.len(), 1);
        assert_eq!(entry.symbols[0].name, "run");
        assert_eq!(entry.import_edges.len(), 1);
    }

    #[test]
    fn sha256_hex_produces_consistent_output() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(sha256_hex("hello world"), hash);
        assert_ne!(sha256_hex("different"), hash);
    }
}
