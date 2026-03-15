use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::edge::{Edge, EdgeKind};
use crate::json::escape;
use crate::model::symbol::{Symbol, SymbolKind};

const ENGINE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-cache-v3-dir");

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
    pub import_edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct ParseCache {
    pub engine_version: String,
    cache_dir: PathBuf,
}

pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl ParseCache {
    pub fn new() -> Self {
        Self {
            engine_version: ENGINE_VERSION.to_string(),
            cache_dir: PathBuf::new(),
        }
    }

    pub fn for_repo(repo_root: &Path) -> Self {
        let cache_dir = cache_dir_path(repo_root);
        Self {
            engine_version: ENGINE_VERSION.to_string(),
            cache_dir,
        }
    }

    pub fn load(repo_root: &Path) -> Option<Self> {
        let cache_dir = cache_dir_path(repo_root);
        let version_path = cache_dir.join("version");
        let version = fs::read_to_string(&version_path).ok()?;
        if version.trim() != ENGINE_VERSION {
            // Version mismatch — clear old cache
            let _ = fs::remove_dir_all(&cache_dir);
            return None;
        }
        Some(Self {
            engine_version: ENGINE_VERSION.to_string(),
            cache_dir,
        })
    }

    pub fn save(&self, repo_root: &Path) {
        let cache_dir = cache_dir_path(repo_root);
        let _ = fs::create_dir_all(&cache_dir);
        ensure_aethyme_gitignore(repo_root);
        let version_path = cache_dir.join("version");
        let _ = fs::write(&version_path, ENGINE_VERSION);
    }

    pub fn lookup(&self, file_path: &str, content_hash: &str) -> Option<CacheEntry> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }
        let entry_path = self.entry_path(file_path);
        let contents = fs::read_to_string(&entry_path).ok()?;
        let entry = deserialize_entry(&contents)?;
        if entry.content_hash != content_hash {
            return None;
        }
        Some(entry)
    }

    pub fn insert(&mut self, file_path: String, entry: CacheEntry) {
        if self.cache_dir.as_os_str().is_empty() {
            return;
        }
        let entry_path = self.entry_path(&file_path);
        if let Some(parent) = entry_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let serialized = serialize_entry(&entry);
        let _ = fs::write(&entry_path, serialized);
    }

    fn entry_path(&self, file_path: &str) -> PathBuf {
        let hash = &sha256_hex(file_path)[..16];
        self.cache_dir.join(format!("{}.json", hash))
    }
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_dir_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".aethyme").join("cache").join("v3")
}

/// Create a `.gitignore` inside `.aethyme/` so the cache is automatically
/// ignored in any repo the engine indexes — no manual `.gitignore` edit needed.
fn ensure_aethyme_gitignore(repo_root: &Path) {
    let gitignore = repo_root.join(".aethyme").join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "# Auto-generated — Aethyme graph cache (recomputed locally)\n*\n");
    }
}

fn serialize_entry(entry: &CacheEntry) -> String {
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
    format!(
        "{{\"content_hash\":\"{}\",\"symbols\":[{}],\"import_edges\":[{}]}}",
        escape(&entry.content_hash),
        symbols_json.join(","),
        edges_json.join(",")
    )
}

fn deserialize_entry(json: &str) -> Option<CacheEntry> {
    let json = json.trim();
    if !json.starts_with('{') || !json.ends_with('}') {
        return None;
    }
    let inner = &json[1..json.len() - 1];
    let content_hash = extract_string_field(inner, "content_hash")?;
    let symbols = parse_symbols_array(inner)?;
    let import_edges = parse_edges_array(inner)?;
    Some(CacheEntry {
        content_hash,
        symbols,
        import_edges,
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
    fn round_trip_entry() {
        let entry = CacheEntry {
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
        };
        let serialized = serialize_entry(&entry);
        let deserialized = deserialize_entry(&serialized).expect("should deserialize");
        assert_eq!(deserialized.content_hash, "abc123");
        assert_eq!(deserialized.symbols.len(), 1);
        assert_eq!(deserialized.symbols[0].name, "run");
        assert_eq!(deserialized.import_edges.len(), 1);
    }

    #[test]
    fn round_trip_empty_entry() {
        let entry = CacheEntry {
            content_hash: "deadbeef".to_string(),
            symbols: vec![],
            import_edges: vec![],
        };
        let serialized = serialize_entry(&entry);
        let deserialized = deserialize_entry(&serialized).expect("should deserialize");
        assert_eq!(deserialized.content_hash, "deadbeef");
        assert!(deserialized.symbols.is_empty());
        assert!(deserialized.import_edges.is_empty());
    }

    #[test]
    fn directory_cache_round_trip() {
        let root = std::env::temp_dir().join("aethyme_dir_cache_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp dir");

        let mut cache = ParseCache::for_repo(&root);
        cache.save(&root);

        let entry = CacheEntry {
            content_hash: "hash123".to_string(),
            symbols: vec![Symbol {
                id: "fn:test:src/lib.rs:foo".to_string(),
                name: "foo".to_string(),
                kind: SymbolKind::Function,
                file: "src/lib.rs".to_string(),
                line: 5,
                signature: "fn foo()".to_string(),
                language: None,
                area: None,
            }],
            import_edges: vec![],
        };
        cache.insert("src/lib.rs".to_string(), entry);

        // Reload from disk
        let loaded = ParseCache::load(&root).expect("should load");
        let found = loaded.lookup("src/lib.rs", "hash123").expect("should find entry");
        assert_eq!(found.symbols.len(), 1);
        assert_eq!(found.symbols[0].name, "foo");

        // Wrong hash should miss
        assert!(loaded.lookup("src/lib.rs", "wrong_hash").is_none());

        // Unknown file should miss
        assert!(loaded.lookup("src/unknown.rs", "hash123").is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sha256_hex_produces_consistent_output() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(sha256_hex("hello world"), hash);
        assert_ne!(sha256_hex("different"), hash);
    }
}
