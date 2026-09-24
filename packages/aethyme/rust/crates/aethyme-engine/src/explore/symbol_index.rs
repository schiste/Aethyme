//! On-demand definitions index for the graph-free source search.
//!
//! Definitions are extracted with the workspace's language indexers (Ruff for
//! Python, Oxc for TypeScript/JavaScript, `ra_ap_syntax` for Rust and
//! tree-sitter for PHP). Only files the content search already matched are
//! parsed, and each result is cached under the host cache directory (never
//! inside the repository), keyed by the canonical repository path and
//! invalidated per file by modification time and size. The index therefore
//! fills in as Explore is used and is reused by every later call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use aethyme_graph_indexer::{IndexedFile, IndexerContext, LanguageRegistry, default_registry};
use aethyme_graph_schema::{File, Node};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_SCHEMA_VERSION: u32 = 1;
/// Larger sources are still content-searched, only not parsed.
const MAX_PARSE_BYTES: u64 = 512 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Definition {
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedFile {
    mtime_ns: u128,
    size: u64,
    definitions: Vec<Definition>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    engine_version: String,
    files: HashMap<String, CachedFile>,
}

/// Per-file stamp used for invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    pub mtime_ns: u128,
    pub size: u64,
}

impl Stamp {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        Stamp {
            mtime_ns,
            size: metadata.len(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub(super) struct IndexStats {
    pub cached_files: usize,
    pub hits: usize,
    pub parsed: usize,
    pub persisted: bool,
}

pub(super) struct SymbolIndex {
    location: Option<PathBuf>,
    context: Option<IndexerContext>,
    loaded: HashMap<String, CachedFile>,
    updates: Mutex<HashMap<String, CachedFile>>,
    hits: std::sync::atomic::AtomicUsize,
}

fn registry() -> &'static LanguageRegistry {
    static REGISTRY: OnceLock<LanguageRegistry> = OnceLock::new();
    REGISTRY.get_or_init(default_registry)
}

/// The indexer language that parses `path`, if any. JavaScript is parsed by
/// the TypeScript indexer, which selects its source type from the extension.
fn parser_language(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "py" | "pyi" => Some("python"),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Some("typescript"),
        "rs" => Some("rust"),
        "php" => Some("php"),
        _ => None,
    }
}

pub(super) fn supports(path: &str) -> bool {
    parser_language(path).is_some() || super::path_role::classify(path).is_code
}

const DEFINITION_MODIFIERS: &[&str] = &[
    "pub",
    "export",
    "default",
    "async",
    "static",
    "public",
    "private",
    "protected",
    "abstract",
    "final",
    "override",
    "internal",
    "open",
    "unsafe",
    "extern",
    "declare",
    "readonly",
    "sealed",
    "inline",
    "virtual",
    "data",
    "partial",
];

fn keyword_kind(word: &str) -> Option<&'static str> {
    Some(match word {
        "fn" | "def" | "function" | "func" | "fun" | "sub" => "function",
        "class" | "object" | "record" => "class",
        "struct" | "union" => "struct",
        "enum" => "enum",
        "trait" | "protocol" => "trait",
        "interface" => "interface",
        "type" | "typedef" => "type",
        "const" => "variable",
        "module" | "mod" | "namespace" => "module",
        "macro_rules!" => "macro",
        "impl" | "extension" => "impl",
        _ => return None,
    })
}

/// Language-agnostic "this line introduces a named definition" check:
/// optional modifiers (`pub(crate)`, `export default`, `async`), a
/// definition keyword, then the defined name. Returns `(kind, name)`.
pub(super) fn line_definition(line: &str) -> Option<(&'static str, &str)> {
    let mut rest = line.trim_start();
    for _ in 0..6 {
        let end = rest
            .find(|ch: char| ch.is_whitespace() || ch == '(')
            .unwrap_or(rest.len());
        let word = &rest[..end];
        if let Some(kind) = keyword_kind(word) {
            let mut after = rest[end..].trim_start();
            if word == "func" && after.starts_with('(') {
                // Go method receiver: `func (s *Server) Name(`.
                after = after[after.find(')')? + 1..].trim_start();
            }
            let after = after.trim_start_matches(['*', '&']);
            let name_end = after
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                .unwrap_or(after.len());
            let name = &after[..name_end];
            return (!name.is_empty()).then_some((kind, name));
        }
        if !DEFINITION_MODIFIERS.contains(&word) {
            return None;
        }
        let mut next = &rest[end..];
        if next.starts_with('(') {
            // `pub(crate)`
            next = &next[next.find(')')? + 1..];
        }
        rest = next.trim_start();
    }
    None
}

/// Keyword-introduced definitions for any language, including the ones no
/// parser covers and declarations a parser skips.
fn keyword_definitions(content: &str) -> Vec<Definition> {
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (kind, name) = line_definition(line)?;
            (kind != "impl").then(|| Definition {
                name: name.to_string(),
                kind: kind.into(),
                start_line: index as u32 + 1,
                end_line: index as u32 + 1,
            })
        })
        .collect()
}

/// Default host cache location for this repository's index, or `None` when
/// the repository is ephemeral (under the system temp directory) and no cache
/// directory was named explicitly: throwaway checkouts should not accumulate
/// host state.
pub(super) fn default_location(root: &Path) -> Option<PathBuf> {
    let explicit = std::env::var_os("AETHYME_HOST_CACHE_DIR").filter(|path| !path.is_empty());
    if explicit.is_none() {
        let temp = std::env::temp_dir();
        let temp = temp.canonicalize().unwrap_or(temp);
        if root.starts_with(&temp) {
            return None;
        }
    }
    let base = explicit
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(path).join("aethyme"))
        })
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Caches/Aethyme")
            } else {
                home.join(".cache/aethyme")
            })
        })?;
    Some(location_under(&base, root))
}

pub(super) fn location_under(base: &Path, root: &Path) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    base.join("symbol-index")
        .join(format!("v{CACHE_SCHEMA_VERSION}"))
        .join(format!("{}.bin", &digest[..32]))
}

impl SymbolIndex {
    pub fn open(root: &Path, location: Option<PathBuf>) -> Self {
        let context = IndexerContext::new("explore", root, env!("CARGO_PKG_VERSION")).ok();
        let loaded = location.as_deref().map(load).unwrap_or_default();
        SymbolIndex {
            location,
            context,
            loaded,
            updates: Mutex::new(HashMap::new()),
            hits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Definitions for one file, from the cache when its stamp still matches,
    /// otherwise parsed now and queued for persistence.
    pub fn definitions(&self, path: &str, stamp: Stamp, content: &str) -> Vec<Definition> {
        if !supports(path) {
            return Vec::new();
        }
        if let Some(cached) = self.loaded.get(path)
            && cached.mtime_ns == stamp.mtime_ns
            && cached.size == stamp.size
        {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return cached.definitions.clone();
        }
        let mut definitions = match parser_language(path) {
            Some(language) if stamp.size <= MAX_PARSE_BYTES => self.parse(language, path, content),
            _ => Vec::new(),
        };
        for keyword in keyword_definitions(content) {
            if !definitions.iter().any(|parsed| {
                parsed.name == keyword.name
                    && parsed.start_line <= keyword.start_line
                    && keyword.start_line <= parsed.end_line
            }) {
                definitions.push(keyword);
            }
        }
        definitions.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| a.name.cmp(&b.name))
        });
        if let Ok(mut updates) = self.updates.lock() {
            updates.insert(
                path.to_string(),
                CachedFile {
                    mtime_ns: stamp.mtime_ns,
                    size: stamp.size,
                    definitions: definitions.clone(),
                },
            );
        }
        definitions
    }

    fn parse(&self, language: &str, path: &str, content: &str) -> Vec<Definition> {
        let (Some(context), Some(indexer)) = (&self.context, registry().get(language)) else {
            return Vec::new();
        };
        let Ok(file) = File::new(
            context.repo_name(),
            path,
            language,
            content.len() as u64,
            "unhashed",
        ) else {
            return Vec::new();
        };
        let indexed = IndexedFile {
            source_path: path.into(),
            top_node: Node::File(file),
            language: language.into(),
        };
        // A parser bug on one odd file must not take Explore down with it.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            indexer.index_file(context, &indexed, content)
        }));
        let Ok(Ok(result)) = result else {
            return Vec::new();
        };
        let mut definitions = result
            .additional_nodes
            .iter()
            .filter_map(definition_of)
            .collect::<Vec<_>>();
        definitions.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| a.name.cmp(&b.name))
        });
        definitions.dedup();
        definitions
    }

    /// Persist new entries. `seen` (when the scan was complete) prunes entries
    /// for files that no longer exist. Best effort: a read-only or missing
    /// cache directory only costs the next call a re-parse.
    pub fn finish(self, seen: Option<&std::collections::HashSet<String>>) -> IndexStats {
        let updates = self.updates.into_inner().unwrap_or_default();
        let hits = self.hits.into_inner();
        let mut files = self.loaded;
        let before = files.len();
        if let Some(seen) = seen {
            files.retain(|path, _| seen.contains(path));
        }
        let pruned = files.len() != before;
        let parsed = updates.len();
        files.extend(updates);
        let mut stats = IndexStats {
            cached_files: files.len(),
            hits,
            parsed,
            persisted: false,
        };
        if let Some(location) = self.location
            && (parsed > 0 || pruned)
        {
            stats.persisted = store(&location, files).is_ok();
        }
        stats
    }
}

fn definition_of(node: &Node) -> Option<Definition> {
    let kind = match node {
        Node::Function(_) => "function",
        Node::Method(_) => "method",
        Node::Class(_) => "class",
        Node::Struct(_) => "struct",
        Node::Enum(_) => "enum",
        Node::Interface(_) => "interface",
        Node::Trait(_) => "trait",
        Node::TypeAlias(_) => "type",
        Node::GlobalVariable(_) => "variable",
        _ => return None,
    };
    let name = node.name()?;
    let range = node.source_range()?;
    Some(Definition {
        name: name.rsplit(['.', ':']).next().unwrap_or(name).to_string(),
        kind: kind.into(),
        start_line: range.start_line(),
        end_line: range.end_line(),
    })
}

fn load(location: &Path) -> HashMap<String, CachedFile> {
    let Ok(metadata) = std::fs::metadata(location) else {
        return HashMap::new();
    };
    if metadata.len() > MAX_CACHE_BYTES {
        return HashMap::new();
    }
    let Ok(bytes) = std::fs::read(location) else {
        return HashMap::new();
    };
    match bincode::deserialize::<CacheFile>(&bytes) {
        Ok(cache)
            if cache.schema_version == CACHE_SCHEMA_VERSION
                && cache.engine_version == env!("CARGO_PKG_VERSION") =>
        {
            cache.files
        }
        _ => HashMap::new(),
    }
}

fn store(location: &Path, files: HashMap<String, CachedFile>) -> std::io::Result<()> {
    let parent = location
        .parent()
        .ok_or_else(|| std::io::Error::other("cache location has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let cache = CacheFile {
        schema_version: CACHE_SCHEMA_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").into(),
        files,
    };
    let bytes = bincode::serialize(&cache).map_err(std::io::Error::other)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        location
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, location).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(size: u64) -> Stamp {
        Stamp { mtime_ns: 7, size }
    }

    #[test]
    fn extracts_definitions_across_languages() {
        let root = tempfile::tempdir().unwrap();
        let index = SymbolIndex::open(&root.path().canonicalize().unwrap(), None);
        let python = "class TokenStore:\n    def load_token(self):\n        return 1\n";
        let names = index
            .definitions("app/store.py", stamp(python.len() as u64), python)
            .into_iter()
            .map(|definition| (definition.name, definition.kind, definition.start_line))
            .collect::<Vec<_>>();
        assert!(names.contains(&("TokenStore".into(), "class".into(), 1)));
        assert!(names.contains(&("load_token".into(), "method".into(), 2)));

        let rust = "pub struct Cache;\nimpl Cache {\n    pub fn evict_stale(&self) {}\n}\n";
        let names = index
            .definitions("src/cache.rs", stamp(rust.len() as u64), rust)
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"Cache".to_string()));
        assert!(names.contains(&"evict_stale".to_string()));

        let script = "export function retryWithBackoff(n) { return n; }\n";
        let names = index
            .definitions("web/retry.js", stamp(script.len() as u64), script)
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["retryWithBackoff"]);
        let typescript = "export class SessionStore {\n  refreshAccessToken(): void {}\n}\n";
        let names = index
            .definitions("app/session.ts", stamp(typescript.len() as u64), typescript)
            .into_iter()
            .map(|definition| (definition.name, definition.kind, definition.start_line))
            .collect::<Vec<_>>();
        assert!(
            names.contains(&("refreshAccessToken".into(), "method".into(), 2)),
            "{names:?}"
        );
        // Export-wrapped declarations come from the TypeScript indexer itself,
        // with their original line numbers.
        let exported = "import { x } from './x';\n\
                        export default class Loader {\n  load(): void {}\n}\n\
                        export async function fetchToken(): Promise<void> {}\n\
                        export interface TokenShape { id: string }\n";
        let names = index
            .definitions("app/loader.ts", stamp(exported.len() as u64), exported)
            .into_iter()
            .map(|definition| (definition.name, definition.start_line))
            .collect::<Vec<_>>();
        for expected in [
            ("Loader", 2),
            ("load", 3),
            ("fetchToken", 5),
            ("TokenShape", 6),
        ] {
            assert!(
                names.contains(&(expected.0.to_string(), expected.1)),
                "{expected:?} missing from {names:?}"
            );
        }
        let go = "package q\n\nfunc (q *Queue) Drain() error {\n\treturn nil\n}\n";
        let names = index
            .definitions("q/queue.go", stamp(go.len() as u64), go)
            .into_iter()
            .map(|definition| (definition.name, definition.start_line))
            .collect::<Vec<_>>();
        assert_eq!(names, [("Drain".to_string(), 3)]);
        assert!(index.definitions("notes.md", stamp(3), "# x").is_empty());
    }

    #[test]
    fn keyword_definitions_name_the_defined_symbol() {
        for (line, expected) in [
            ("pub fn load(x: u8) {", Some(("function", "load"))),
            ("def load(self):", Some(("function", "load"))),
            (
                "export default function load() {",
                Some(("function", "load")),
            ),
            ("  async def load():", Some(("function", "load"))),
            ("class Loader:", Some(("class", "Loader"))),
            (
                "func (s *Server) Load() error {",
                Some(("function", "Load")),
            ),
            ("pub(crate) struct Loader;", Some(("struct", "Loader"))),
            (
                "export const MAX_RETRIES = 3;",
                Some(("variable", "MAX_RETRIES")),
            ),
            ("load(x)", None),
            ("return load", None),
            ("// fn in a comment", None),
            ("let x = load();", None),
            ("const run = function () {", Some(("variable", "run"))),
            ("module.exports = load;", None),
        ] {
            assert_eq!(line_definition(line), expected, "{line}");
        }
    }

    #[test]
    fn cache_persists_outside_the_repository_and_invalidates_by_stamp() {
        let root = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let location = location_under(host.path(), &canonical);
        let source = "def load_token():\n    pass\n";

        let index = SymbolIndex::open(&canonical, Some(location.clone()));
        assert_eq!(index.definitions("a.py", stamp(26), source).len(), 1);
        let stats = index.finish(None);
        assert_eq!((stats.parsed, stats.hits), (1, 0));
        assert!(stats.persisted && location.is_file());
        assert!(!location.starts_with(&canonical));

        // Same stamp: served from the cache even though the content differs,
        // which proves no re-parse happened.
        let index = SymbolIndex::open(&canonical, Some(location.clone()));
        assert_eq!(index.definitions("a.py", stamp(26), "").len(), 1);
        let stats = index.finish(None);
        assert_eq!((stats.parsed, stats.hits), (0, 1));

        // A changed stamp re-parses.
        let index = SymbolIndex::open(&canonical, Some(location.clone()));
        let changed = "x = 1\n";
        assert!(
            index
                .definitions("a.py", stamp(changed.len() as u64), changed)
                .iter()
                .all(|definition| definition.name != "load_token")
        );
        let seen = std::collections::HashSet::from(["a.py".to_string()]);
        assert_eq!(index.finish(Some(&seen)).parsed, 1);
    }
}
