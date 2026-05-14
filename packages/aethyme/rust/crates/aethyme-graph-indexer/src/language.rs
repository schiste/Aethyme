//! `LanguageIndexer` trait and registry.
//!
//! Each language gets its own indexer that consumes a file's content
//! and produces additional nodes + edges to attach to the per-file
//! Fragment. The trait is the seam between the language-agnostic
//! pipeline (commit 3.2) and the per-language AST work (commits
//! 3.3+).

use std::collections::BTreeMap;

use aethyme_graph_schema::{Edge, Node};

use crate::context::IndexerContext;
use crate::filesystem::IndexedFile;

/// Additional graph payload produced by a language indexer.
///
/// Returned alongside (not replacing) the IndexedFile's top-level
/// File node. The pipeline merges these with the top-level node to
/// build the final per-file Fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageIndexResult {
    pub additional_nodes: Vec<Node>,
    pub additional_edges: Vec<Edge>,
}

/// A language-specific indexer.
///
/// Implementations must be `Send + Sync` so the future rayon-based
/// pipeline can dispatch files across cores. The trait stays
/// minimal — content in, nodes + edges out — so future languages
/// (TypeScript, Rust, PHP) plug in without touching the pipeline
/// code.
pub trait LanguageIndexer: Send + Sync {
    /// Canonical language tag this indexer handles (matches the
    /// values from `language_map::infer_language_from_extension`).
    fn language(&self) -> &'static str;

    /// Parse `content` and produce additional graph payload to
    /// attach to `indexed_file`'s fragment.
    ///
    /// Implementations should treat a syntax error as a structured
    /// error, not a panic. The caller is responsible for reading
    /// the file from disk (so this trait stays I/O-free and easy
    /// to test).
    fn index_file(
        &self,
        ctx: &IndexerContext,
        indexed_file: &IndexedFile,
        content: &str,
    ) -> Result<LanguageIndexResult, LanguageIndexError>;
}

/// Boxed-trait registry mapping language tag → indexer. BTreeMap
/// rather than HashMap so iteration order is deterministic — not
/// load-bearing yet (registry lookups are by exact key), but it
/// matches the rest of the crate's determinism discipline.
#[derive(Default)]
pub struct LanguageRegistry {
    indexers: BTreeMap<&'static str, Box<dyn LanguageIndexer>>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an indexer. Panics if a different indexer is
    /// already registered for the same language — the registry
    /// is built once at startup and silent overrides would be a
    /// configuration bug.
    pub fn register<I: LanguageIndexer + 'static>(&mut self, indexer: I) {
        let language = indexer.language();
        let prev = self.indexers.insert(language, Box::new(indexer));
        assert!(
            prev.is_none(),
            "LanguageRegistry: a different indexer was already \
             registered for language {language:?}; the registry \
             should be built once at startup"
        );
    }

    pub fn get(&self, language: &str) -> Option<&dyn LanguageIndexer> {
        self.indexers.get(language).map(|b| &**b)
    }

    pub fn languages(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.indexers.keys().copied()
    }
}

#[derive(Debug)]
pub enum LanguageIndexError {
    /// Source did not parse. `message` is the parser's diagnostic
    /// (formatted; specific format varies by parser).
    Parse { message: String },
    /// A schema node construction failed (typically because a
    /// required field was empty after extraction).
    NodeConstruction { message: String },
}

impl std::fmt::Display for LanguageIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { message } => write!(f, "language parse: {message}"),
            Self::NodeConstruction { message } => {
                write!(f, "language node construction: {message}")
            }
        }
    }
}

impl std::error::Error for LanguageIndexError {}

/// Helper for converting byte offsets to 1-based line numbers.
///
/// Built once per source file (O(n) over content size). Query is
/// O(log n) via binary search. Schema's SourceRange expects 1-based
/// inclusive lines.
pub struct LineIndex {
    newline_offsets: Vec<usize>,
    content_len: usize,
}

impl LineIndex {
    pub fn new(content: &str) -> Self {
        let newline_offsets = content
            .bytes()
            .enumerate()
            .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
            .collect();
        Self {
            newline_offsets,
            content_len: content.len(),
        }
    }

    /// Convert a byte offset to a 1-based line number.
    pub fn line_at(&self, byte_offset: usize) -> u32 {
        let clamped = byte_offset.min(self.content_len);
        let line = self
            .newline_offsets
            .partition_point(|&pos| pos < clamped)
            + 1;
        line as u32
    }
}
