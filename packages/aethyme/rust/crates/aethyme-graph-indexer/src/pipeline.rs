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

use aethyme_graph_schema::{Node, NodeKind};
use aethyme_graph_storage::{
    write_fragment, write_index_shard, Fragment, FragmentBuildError,
    FragmentWriteError, IndexShardWriteError, SymbolRecord,
};

use crate::context::IndexerContext;
use crate::filesystem::{
    walk_source_tree, FilesystemIndexerError, IndexedFile, WalkOptions,
};

/// One indexed file's full storage footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltFragment {
    pub source_path: Box<str>,
    pub fragment: Fragment,
}

/// Build a Fragment from a single IndexedFile.
///
/// The IndexedFile carries one top-level node (File or NonCodeFile);
/// the resulting Fragment wraps it with no edges. Language indexers
/// (commit 3.3+) will produce IndexedFile records with richer
/// `additional_nodes` / `additional_edges` fields once the AST
/// layer lands.
pub fn build_fragment(
    indexed: &IndexedFile,
) -> Result<BuiltFragment, BuildFragmentError> {
    let fragment = Fragment::new(
        &indexed.source_path,
        vec![indexed.top_node.clone()],
        vec![],
    )
    .map_err(BuildFragmentError::Fragment)?;
    Ok(BuiltFragment {
        source_path: indexed.source_path.clone(),
        fragment,
    })
}

/// Build SymbolRecord entries from an indexed file's nodes, grouped
/// by module. The grouping key is the synthesized module name
/// (currently derived from the source path's directory part — once
/// language indexers land, we'll prefer their module attribution
/// over this fallback).
///
/// Returns a `BTreeMap<module_name, Vec<SymbolRecord>>` so the
/// caller can write one shard per module.
pub fn build_index_records(
    indexed_files: &[IndexedFile],
) -> BTreeMap<String, Vec<SymbolRecord>> {
    let mut records_by_module: BTreeMap<String, Vec<SymbolRecord>> =
        BTreeMap::new();
    for indexed in indexed_files {
        let module = synthesize_module_name(&indexed.source_path);
        // Top-level node: File or NonCodeFile. Only the named-symbol
        // kinds get records; container kinds at the top of a file
        // typically don't need to appear in symbol-search indices,
        // but we include them anyway so "find me the README" works.
        let record = symbol_record_for(&module, &indexed.source_path, &indexed.top_node);
        records_by_module
            .entry(module)
            .or_default()
            .push(record);
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

fn symbol_record_for(
    module: &str,
    source_path: &str,
    node: &Node,
) -> SymbolRecord {
    // Use the file's basename (without extension) as the symbol
    // name for File / NonCodeFile records.
    let symbol = source_path
        .rsplit_once('/')
        .map(|(_, tail)| tail)
        .unwrap_or(source_path);
    let symbol = symbol
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(symbol);

    SymbolRecord {
        module: module.into(),
        symbol: symbol.into(),
        kind: node.kind(),
        node_id: node.id().clone(),
        file: source_path.into(),
    }
}

/// One-shot helper: walk the repo, build all fragments + index
/// shards, write them through the storage layer to their canonical
/// paths. Returns a summary of what was written.
///
/// Intended for the `aethyme index` CLI command (Phase 4+) once
/// the engine wires this in.
pub fn index_repo_to_disk(
    ctx: &IndexerContext,
    options: &WalkOptions,
) -> Result<IndexRepoSummary, IndexRepoError> {
    let walk = walk_source_tree(ctx, options).map_err(IndexRepoError::Walk)?;
    let total_files = walk.files.len();
    let total_skipped = walk.skipped.len();

    let mut fragments_written = Vec::with_capacity(walk.files.len());
    let mut counts_by_kind: BTreeMap<NodeKind, usize> = BTreeMap::new();

    for indexed in &walk.files {
        let built = build_fragment(indexed).map_err(IndexRepoError::Build)?;
        let path = write_fragment(
            ctx.repo_root(),
            &built.source_path,
            &built.fragment,
        )
        .map_err(IndexRepoError::FragmentWrite)?;
        *counts_by_kind.entry(indexed.top_node.kind()).or_default() += 1;
        fragments_written.push(path);
    }

    let mut shards_written = Vec::new();
    let records = build_index_records(&walk.files);
    for (module, recs) in records {
        let path = write_index_shard(ctx.repo_root(), &module, &recs)
            .map_err(IndexRepoError::IndexShardWrite)?;
        shards_written.push(path);
    }

    Ok(IndexRepoSummary {
        total_files,
        total_skipped,
        fragments_written,
        shards_written,
        counts_by_kind,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRepoSummary {
    pub total_files: usize,
    pub total_skipped: usize,
    pub fragments_written: Vec<PathBuf>,
    pub shards_written: Vec<PathBuf>,
    pub counts_by_kind: BTreeMap<NodeKind, usize>,
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
}

impl std::fmt::Display for IndexRepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Walk(e) => write!(f, "index_repo: {e}"),
            Self::Build(e) => write!(f, "index_repo: {e}"),
            Self::FragmentWrite(e) => write!(f, "index_repo: {e}"),
            Self::IndexShardWrite(e) => write!(f, "index_repo: {e}"),
        }
    }
}

impl std::error::Error for IndexRepoError {}
