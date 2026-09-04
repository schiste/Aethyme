//! Glue between the filesystem walker and the storage crate.
//!
//! Phase 3.2 layer: turns `IndexedFile` records (from the
//! filesystem walker, commit 3.1) into committed-ready `Fragment`s
//! and writes them through the storage layer. Also builds the
//! per-module index shards that mirror the source structure.
//!
//! At this layer there's still no AST parsing — every fragment
//! contains just the top-level File / NonCodeFile node. Language
//! indexers (commit 3.3+) will hook in here by enriching the
//! IndexedFile records before they reach `build_fragment`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use rayon::prelude::*;

use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::{
    Fragment, FragmentBuildError, FragmentWriteError, IndexShardWriteError, SymbolRecord,
    write_fragment, write_index_shard,
};

use crate::context::IndexerContext;
use crate::filesystem::{FilesystemIndexerError, IndexedFile, WalkOptions, walk_source_tree};
use crate::language::{LanguageIndexError, LanguageIndexResult, LanguageRegistry};
use crate::php::PhpIndexer;
use crate::python::PythonIndexer;
use crate::rust_lang::RustIndexer;
use crate::surface_flow;
use crate::typescript::TypeScriptIndexer;

/// One indexed file's full storage footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltFragment {
    pub source_path: Box<str>,
    pub fragment: Fragment,
}

/// Build a Fragment from a single IndexedFile, optionally enriched
/// by a language-specific indexer.
///
/// If `language_indexer_output` is Some, its nodes + edges are merged
/// with the IndexedFile's top-level node. Pass None for non-code
/// files (NonCodeFile) and for code files whose language has no
/// registered indexer.
pub fn build_fragment(
    indexed: &IndexedFile,
    language_indexer_output: Option<crate::language::LanguageIndexResult>,
) -> Result<BuiltFragment, BuildFragmentError> {
    let (additional_nodes, additional_edges) = match language_indexer_output {
        Some(r) => (r.additional_nodes, r.additional_edges),
        None => (Vec::new(), Vec::new()),
    };
    let mut nodes = Vec::with_capacity(additional_nodes.len() + 1);
    nodes.push(indexed.top_node.clone());
    nodes.extend(additional_nodes);
    let fragment = Fragment::new(&indexed.source_path, nodes, additional_edges)
        .map_err(BuildFragmentError::Fragment)?;
    Ok(BuiltFragment {
        source_path: indexed.source_path.clone(),
        fragment,
    })
}

/// Default registry: includes every language indexer the crate
/// ships. Called by `index_repo_to_disk`; callers who want
/// per-call control can build their own registry and pass it.
pub fn default_registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(PythonIndexer::new());
    registry.register(TypeScriptIndexer::new());
    registry.register(RustIndexer::new());
    // PhpIndexer construction can fail (tree-sitter `set_language`
    // returns Result). For the default registry we ignore the
    // error — if PHP support is broken, files just fall through
    // to the filesystem-only path, which is more graceful than
    // failing the whole indexer.
    if let Ok(php) = PhpIndexer::new() {
        registry.register(php);
    }
    registry
}

/// Build SymbolRecord entries from a list of built fragments,
/// grouped by module.
///
/// For each fragment, emit one record per named node (Function,
/// Class, Method, Interface, etc.). The module name is synthesized
/// from the source path (currently `/` → `.` with extension
/// stripped). Container-only kinds without names (Directory,
/// Statement, Expression, untagged Comments) are skipped — they
/// have nothing useful to look up by name.
///
/// Returns a `BTreeMap<module_name, Vec<SymbolRecord>>` so the
/// caller can write one shard per module.
pub fn build_index_records(
    built_fragments: &[BuiltFragment],
) -> BTreeMap<String, Vec<SymbolRecord>> {
    let mut records_by_module: BTreeMap<String, Vec<SymbolRecord>> = BTreeMap::new();
    for built in built_fragments {
        let module = synthesize_module_name(&built.source_path);
        for node in built.fragment.nodes() {
            let Some(symbol_name) = node.name() else {
                // Unnamed kinds (Statement, Expression, anon
                // comments) don't go into the symbol index.
                continue;
            };
            // For the top-level File node, the basename without
            // extension is more useful as the "symbol" than the
            // path-derived synthesized name (which is already in
            // the module field).
            let symbol_text = if matches!(
                node.kind(),
                aethyme_graph_schema::NodeKind::File | aethyme_graph_schema::NodeKind::NonCodeFile
            ) {
                continue; // File/NonCodeFile name() returns None anyway
            } else {
                symbol_name.to_string()
            };
            records_by_module
                .entry(module.clone())
                .or_default()
                .push(SymbolRecord {
                    module: module.clone().into(),
                    symbol: symbol_text.into(),
                    kind: node.kind(),
                    node_id: node.id().clone(),
                    file: built.source_path.clone(),
                });
        }
    }
    records_by_module
}

/// Module name from a source path: replace `/` with `.`, strip the
/// file extension. Example: `src/cli.py` → `src.cli`. Languages
/// with non-path-based module naming (e.g. Rust's `mod` declarations)
/// will override this when their indexers land.
fn synthesize_module_name(source_path: &str) -> String {
    let without_ext = source_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(source_path);
    without_ext.replace('/', ".")
}

/// One-shot helper: walk the repo, dispatch each code file to its
/// registered language indexer, build all fragments + index shards,
/// write them through the storage layer to their canonical paths.
///
/// Uses the default registry (which currently includes only
/// PythonIndexer). For per-call control, use `index_repo_to_disk_with`.
pub fn index_repo_to_disk(
    ctx: &IndexerContext,
    options: &WalkOptions,
) -> Result<IndexRepoSummary, IndexRepoError> {
    let registry = default_registry();
    index_repo_to_disk_with(ctx, options, &registry)
}

/// Same as [`index_repo_to_disk`] but accepts an explicit registry.
///
/// Source discovery, in-memory indexing, and fragment serialization are
/// separate measured phases. The per-file indexing and serialization phases
/// remain parallelized across rayon's thread pool.
pub fn index_repo_to_disk_with(
    ctx: &IndexerContext,
    options: &WalkOptions,
    registry: &LanguageRegistry,
) -> Result<IndexRepoSummary, IndexRepoError> {
    let discovery_started = Instant::now();
    let walk = walk_source_tree(ctx, options).map_err(IndexRepoError::Walk)?;
    let source_discovery_elapsed_us = discovery_started.elapsed().as_micros();
    let total_files = walk.files.len();
    let total_skipped = walk.skipped.len();

    let indexing_started = Instant::now();
    // Build every fragment in memory first so parsing/indexing and serialization
    // have distinct wall-clock boundaries instead of an ambiguous combined time.
    let per_file: Vec<(BTreeMap<NodeKind, usize>, BuiltFragment, u64)> = walk
        .files
        .par_iter()
        .map(
            |indexed| -> Result<(BTreeMap<NodeKind, usize>, BuiltFragment, u64), IndexRepoError> {
                let language_indexer = registry.get(&indexed.language);
                let needs_content =
                    language_indexer.is_some() || surface_flow::should_scan(indexed);
                let content = if needs_content {
                    let abs = ctx.repo_root().join(&*indexed.source_path);
                    Some(
                        std::fs::read_to_string(&abs).map_err(|e| IndexRepoError::ReadSource {
                            source_path: indexed.source_path.clone(),
                            message: e.to_string(),
                        })?,
                    )
                } else {
                    None
                };

                let mut combined = LanguageIndexResult::default();
                if let Some(indexer) = language_indexer {
                    let content = content
                        .as_deref()
                        .expect("language-indexed files are read above");
                    let output = indexer
                        .index_file(ctx, indexed, content)
                        .map_err(IndexRepoError::Language)?;
                    combined.additional_nodes.extend(output.additional_nodes);
                    combined.additional_edges.extend(output.additional_edges);
                }
                if surface_flow::should_scan(indexed) {
                    let content = content
                        .as_deref()
                        .expect("surface-flow-scanned files are read above");
                    let output = surface_flow::index_file(ctx, indexed, content)
                        .map_err(IndexRepoError::Language)?;
                    combined.additional_nodes.extend(output.additional_nodes);
                    combined.additional_edges.extend(output.additional_edges);
                };
                let lang_output = if combined.additional_nodes.is_empty()
                    && combined.additional_edges.is_empty()
                {
                    None
                } else {
                    Some(combined)
                };

                let built = build_fragment(indexed, lang_output).map_err(IndexRepoError::Build)?;

                let mut counts: BTreeMap<NodeKind, usize> = BTreeMap::new();
                for node in built.fragment.nodes() {
                    *counts.entry(node.kind()).or_default() += 1;
                }
                let source_bytes = content.as_ref().map_or(0, |content| content.len() as u64);
                Ok((counts, built, source_bytes))
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let source_indexing_elapsed_us = indexing_started.elapsed().as_micros();

    let mut counts_by_kind: BTreeMap<NodeKind, usize> = BTreeMap::new();
    let mut built_fragments: Vec<BuiltFragment> = Vec::with_capacity(per_file.len());
    let mut source_bytes_read = 0_u64;
    let mut total_edges = 0_usize;
    for (counts, built, source_bytes) in per_file {
        for (kind, count) in counts {
            *counts_by_kind.entry(kind).or_default() += count;
        }
        source_bytes_read += source_bytes;
        total_edges += built.fragment.edge_count();
        built_fragments.push(built);
    }

    let serialization_started = Instant::now();
    let fragments_written: Vec<PathBuf> = built_fragments
        .par_iter()
        .map(|built| {
            write_fragment(ctx.repo_root(), &built.source_path, &built.fragment)
                .map_err(IndexRepoError::FragmentWrite)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Index shards: emit SymbolRecords from each built fragment's
    // FULL node list, not just the file-level walk. This is what
    // makes find_symbols by name work for AST-extracted symbols
    // (Function, Class, Method, etc.).
    let records = build_index_records(&built_fragments);
    let records_vec: Vec<(String, Vec<SymbolRecord>)> = records.into_iter().collect();
    let mut shards_written: Vec<PathBuf> = records_vec
        .par_iter()
        .map(|(module, recs)| -> Result<PathBuf, IndexRepoError> {
            write_index_shard(ctx.repo_root(), module, recs)
                .map_err(IndexRepoError::IndexShardWrite)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Canonical sort by path for deterministic summary output.
    shards_written.sort();
    let fragment_serialization_elapsed_us = serialization_started.elapsed().as_micros();
    let fragment_bytes_written = fragments_written
        .iter()
        .chain(shards_written.iter())
        .filter_map(|path| std::fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum();
    let total_nodes = counts_by_kind.values().sum();

    Ok(IndexRepoSummary {
        total_files,
        total_skipped,
        fragments_written,
        shards_written,
        counts_by_kind,
        total_nodes,
        total_edges,
        observability: IndexRepoObservability {
            source_discovery_elapsed_us,
            source_indexing_elapsed_us,
            fragment_serialization_elapsed_us,
            source_bytes_read,
            fragment_bytes_written,
        },
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexRepoObservability {
    pub source_discovery_elapsed_us: u128,
    pub source_indexing_elapsed_us: u128,
    pub fragment_serialization_elapsed_us: u128,
    pub source_bytes_read: u64,
    pub fragment_bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepoSummary {
    pub total_files: usize,
    pub total_skipped: usize,
    pub fragments_written: Vec<PathBuf>,
    pub shards_written: Vec<PathBuf>,
    pub counts_by_kind: BTreeMap<NodeKind, usize>,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub observability: IndexRepoObservability,
}

#[derive(Debug)]
pub enum BuildFragmentError {
    Fragment(FragmentBuildError),
}

impl std::fmt::Display for BuildFragmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fragment(e) => write!(f, "build_fragment: {e}"),
        }
    }
}

impl std::error::Error for BuildFragmentError {}

#[derive(Debug)]
pub enum IndexRepoError {
    Walk(FilesystemIndexerError),
    Build(BuildFragmentError),
    FragmentWrite(FragmentWriteError),
    IndexShardWrite(IndexShardWriteError),
    /// Reading a source file off disk failed (e.g., file disappeared
    /// between the filesystem walk and the language-indexer read).
    ReadSource {
        source_path: Box<str>,
        message: String,
    },
    /// A language indexer rejected its input. Wraps the underlying
    /// LanguageIndexError so callers can distinguish parse failures
    /// from other failure modes.
    Language(LanguageIndexError),
}

impl std::fmt::Display for IndexRepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Walk(e) => write!(f, "index_repo: {e}"),
            Self::Build(e) => write!(f, "index_repo: {e}"),
            Self::FragmentWrite(e) => write!(f, "index_repo: {e}"),
            Self::IndexShardWrite(e) => write!(f, "index_repo: {e}"),
            Self::ReadSource {
                source_path,
                message,
            } => write!(f, "index_repo: read {source_path:?}: {message}"),
            Self::Language(e) => write!(f, "index_repo: {e}"),
        }
    }
}

impl std::error::Error for IndexRepoError {}
