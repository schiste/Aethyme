//! Filesystem layout for committed graph artifacts.
//!
//! Schema doc §5.1:
//!
//! ```text
//! <repo>/
//! ├── .aethyme/
//! │   ├── engine-version           ← plain text, committed
//! │   ├── graph/                   ← committed
//! │   │   ├── <source-path>.bin    ← per-file binary fragment
//! │   │   │                          (mirror source tree)
//! │   │   ├── _index/
//! │   │   │   └── <module>.ndjson  ← per-module index shard
//! │   │   └── _overlays/
//! │   │       └── <kind>.bin       ← non-per-file producer output
//! │   │                              (structure, configs, docs,
//! │   │                              risks, areas, …)
//! │   └── cache/                   ← gitignored, daemon-managed
//! │       └── *.redb
//! ```
//!
//! This module owns the path arithmetic for everything in
//! `.aethyme/graph/` and `.aethyme/engine-version`. The `cache/`
//! subtree is daemon-managed and lives outside this crate.

use std::path::{Path, PathBuf};

/// Top-level Aethyme directory within a repo.
pub const AETHYME_DIR: &str = ".aethyme";

/// Subdirectory of `AETHYME_DIR` that holds committed graph data.
pub const GRAPH_SUBDIR: &str = "graph";

/// Subdirectory of `GRAPH_SUBDIR` that holds per-module NDJSON
/// index shards. Underscore prefix keeps shards visually grouped
/// when ls-ing `graph/` and prevents collision with any
/// hypothetical source file or directory named `index`.
pub const INDEX_SUBDIR: &str = "_index";

/// Subdirectory of `GRAPH_SUBDIR` that holds non-per-file overlay
/// fragments produced by indexer passes that synthesize repo-level
/// structure rather than parsing one source file (structure,
/// configs, docs, risks, areas, …). Same underscore-prefix
/// convention as [`INDEX_SUBDIR`] for the same reasons: visual
/// grouping plus collision guard against any hypothetical source
/// directory literally named `overlays`. Per-kind payloads live in
/// `<kind>.bin` files; see [`overlay_path`] and the
/// [`overlay`](crate::overlay) module.
pub const OVERLAYS_SUBDIR: &str = "_overlays";

/// File extension for per-file binary fragments.
pub const FRAGMENT_EXT: &str = "bin";

/// File extension for per-module NDJSON index shards.
pub const INDEX_SHARD_EXT: &str = "ndjson";

/// Plain-text file pinning the engine version. Required for the
/// CI verify step: a fragment written by engine v1 must not be
/// trusted as v2-correct without an explicit migration.
pub const ENGINE_VERSION_FILE: &str = "engine-version";

/// Daemon's gitignored cache subdir. The storage crate doesn't
/// touch it but the constant is exported here so engine consumers
/// can keep the layout coherent.
pub const CACHE_SUBDIR: &str = "cache";

/// Absolute path to a source file's fragment within the repo.
///
/// Example: `repo = "/path/to/repo"`, `source_path = "src/cli.py"`
/// → `/path/to/repo/.aethyme/graph/src/cli.py.bin`.
///
/// The mirror-source-tree layout means a source rename requires a
/// paired fragment rename. The alternative (hash-named flat layout)
/// would make renames free but make the relationship between
/// source and fragment opaque to grep. Mirror-source wins per the
/// design conversation.
pub fn fragment_path(repo_root: &Path, source_path: &str) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(GRAPH_SUBDIR);
    p.push(format!("{source_path}.{FRAGMENT_EXT}"));
    p
}

/// Absolute path to a module's index shard within the repo.
///
/// Example: `repo = "/path/to/repo"`, `module = "src.cli"` →
/// `/path/to/repo/.aethyme/graph/_index/src.cli.ndjson`.
///
/// The module name appears literally in the filename; it is NOT
/// escaped. Modules with `/` or other path-meaningful characters
/// in their names are rejected by [`validate_module_name`].
pub fn index_shard_path(repo_root: &Path, module: &str) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(GRAPH_SUBDIR);
    p.push(INDEX_SUBDIR);
    p.push(format!("{module}.{INDEX_SHARD_EXT}"));
    p
}

/// Absolute path to an overlay fragment within the repo.
///
/// Example: `repo = "/path/to/repo"`, `kind = "structure"` →
/// `/path/to/repo/.aethyme/graph/_overlays/structure.bin`.
///
/// The `kind` is the overlay's logical name (e.g. `"structure"`,
/// `"configs"`, `"docs"`, `"risks"`, `"areas"`). It must pass
/// [`validate_module_name`] — overlay kinds share the same
/// path-safety rules as module names.
pub fn overlay_path(repo_root: &Path, kind: &str) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(GRAPH_SUBDIR);
    p.push(OVERLAYS_SUBDIR);
    p.push(format!("{kind}.{FRAGMENT_EXT}"));
    p
}

/// Absolute path to the engine-version pin file.
pub fn engine_version_path(repo_root: &Path) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(ENGINE_VERSION_FILE);
    p
}

/// Absolute path to the daemon's gitignored cache directory.
pub fn cache_dir(repo_root: &Path) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    p.push(AETHYME_DIR);
    p.push(CACHE_SUBDIR);
    p
}

/// Validate that a source path can be safely turned into a fragment
/// filename. Path-traversal (`..`), absolute paths, and Windows
/// drive prefixes are rejected to defend against pathological
/// indexer inputs from rewriting outside the graph directory.
pub fn validate_source_path(source_path: &str) -> Result<(), InvalidPath> {
    if source_path.is_empty() {
        return Err(InvalidPath::Empty);
    }
    if source_path.starts_with('/')
        || source_path.starts_with('\\')
        || (source_path.len() >= 2 && source_path.as_bytes()[1] == b':')
    {
        return Err(InvalidPath::Absolute {
            given: source_path.into(),
        });
    }
    for component in source_path.split(['/', '\\']) {
        if component == ".." {
            return Err(InvalidPath::Traversal {
                given: source_path.into(),
            });
        }
    }
    Ok(())
}

/// Validate that a module name can be safely turned into a shard
/// filename. Rejects path-meaningful characters that would let the
/// shard escape the `_index/` directory.
pub fn validate_module_name(module: &str) -> Result<(), InvalidPath> {
    if module.is_empty() {
        return Err(InvalidPath::Empty);
    }
    for c in module.chars() {
        if matches!(c, '/' | '\\' | ':' | '\n' | '\r' | '\t' | '\0') {
            return Err(InvalidPath::InvalidChar {
                given: module.into(),
                invalid_char: c,
            });
        }
    }
    if module == "." || module == ".." {
        return Err(InvalidPath::Traversal {
            given: module.into(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidPath {
    Empty,
    Absolute { given: Box<str> },
    Traversal { given: Box<str> },
    InvalidChar { given: Box<str>, invalid_char: char },
}

impl std::fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("path or module name is empty"),
            Self::Absolute { given } => write!(
                f,
                "path {given:?} is absolute; expected a repo-relative path"
            ),
            Self::Traversal { given } => write!(
                f,
                "path {given:?} contains a `..` component, which is \
                 not allowed in fragment paths"
            ),
            Self::InvalidChar {
                given,
                invalid_char,
            } => write!(f, "name {given:?} contains invalid char {invalid_char:?}"),
        }
    }
}

impl std::error::Error for InvalidPath {}
