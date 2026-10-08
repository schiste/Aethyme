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
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;

use aethyme_graph_schema::NodeKind;
use aethyme_graph_storage::{
    CoverageArtifactError, CoverageFileStatus, ExclusionReason, Fragment, FragmentBuildError,
    FragmentWriteError, IndexShardWriteError, SymbolRecord, write_coverage_artifacts,
    write_fragment, write_index_shard,
};

use crate::context::IndexerContext;
use crate::coverage::{FileCoverageObservation, IndexCoverage, assemble, observe_file};
use crate::filesystem::{FilesystemIndexerError, IndexedFile, WalkOptions, walk_source_tree};
use crate::language::{LanguageIndexError, LanguageIndexResult, LanguageRegistry};
use crate::php::PhpIndexer;
use crate::python::PythonIndexer;
use crate::relationships;
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
    // The walker classifies `.js`/`.jsx`/`.cjs`/`.mjs` as `javascript`
    // (see `language_map::infer_language_from_extension`), so the oxc
    // indexer has to answer to that tag too or every JavaScript file
    // falls through as `parser_unavailable`.
    registry.register_alias(TypeScriptIndexer::new(), "javascript");
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
    let per_file: Vec<(
        BuiltFragment,
        u64,
        FileCoverageObservation,
        Option<relationships::PendingNonCode>,
    )> = walk
        .files
        .par_iter()
        .map(
            |indexed| -> Result<
                (
                    BuiltFragment,
                    u64,
                    FileCoverageObservation,
                    Option<relationships::PendingNonCode>,
                ),
                IndexRepoError,
            > {
                let language_indexer = registry.get(&indexed.language);
                let needs_relationship_content = relationships::should_read(indexed);
                let needs_content = language_indexer.is_some()
                    || surface_flow::should_scan(indexed)
                    || needs_relationship_content;
                let mut read_error = None;
                let content = if needs_content {
                    let abs = ctx.repo_root().join(&*indexed.source_path);
                    let max_relationship_bytes = options
                        .max_file_size_bytes
                        .unwrap_or(relationships::MAX_NON_CODE_CONTENT_BYTES);
                    let relationship_content_too_large = needs_relationship_content
                        && std::fs::metadata(&abs)
                            .is_ok_and(|metadata| metadata.len() > max_relationship_bytes);
                    if relationship_content_too_large {
                        None
                    } else {
                        match std::fs::read_to_string(&abs) {
                            Ok(content) => Some(content),
                            Err(error) => {
                                read_error = Some(error.to_string());
                                None
                            }
                        }
                    }
                } else {
                    None
                };

                let is_code_file = matches!(&indexed.top_node, aethyme_graph_schema::Node::File(_));
                let mut status = if !is_code_file {
                    CoverageFileStatus::NonCode
                } else if language_indexer.is_some() {
                    CoverageFileStatus::Parsed
                } else {
                    CoverageFileStatus::Unsupported
                };
                let mut parser = language_indexer.map(|indexer| indexer.parser());
                let mut exclusion_reason = if is_code_file && language_indexer.is_none() {
                    Some(ExclusionReason::ParserUnavailable)
                } else {
                    None
                };
                if read_error.is_some() {
                    if is_code_file && language_indexer.is_some() {
                        status = CoverageFileStatus::Partial;
                    }
                    exclusion_reason = Some(ExclusionReason::ReadError);
                }
                let mut combined = LanguageIndexResult::default();
                if let (Some(indexer), Some(content)) = (language_indexer, content.as_deref()) {
                    match indexer.index_file(ctx, indexed, content) {
                        Ok(output) => {
                            combined.additional_nodes.extend(output.additional_nodes);
                            combined.additional_edges.extend(output.additional_edges);
                        }
                        Err(error) => {
                            status = CoverageFileStatus::Partial;
                            exclusion_reason = Some(language_error_reason(&error));
                        }
                    }
                }
                if surface_flow::should_scan(indexed)
                    && let Some(content) = content.as_deref()
                {
                    match surface_flow::index_file(ctx, indexed, content) {
                        Ok(output) => {
                            combined.additional_nodes.extend(output.additional_nodes);
                            combined.additional_edges.extend(output.additional_edges);
                        }
                        Err(error) => {
                            if status == CoverageFileStatus::Parsed {
                                status = CoverageFileStatus::Partial;
                            }
                            if exclusion_reason.is_none() {
                                exclusion_reason = Some(language_error_reason(&error));
                            }
                        }
                    }
                }
                let lang_output = if combined.additional_nodes.is_empty()
                    && combined.additional_edges.is_empty()
                {
                    None
                } else {
                    Some(combined)
                };

                let built = build_fragment(indexed, lang_output).map_err(IndexRepoError::Build)?;

                let source_bytes = content.as_ref().map_or(0, |content| content.len() as u64);
                let pending_relationships = content.as_deref().and_then(|content| {
                    relationships::index_non_code(ctx.repo_name(), indexed, content)
                });
                let observation = observe_file(
                    indexed,
                    &built,
                    content.as_deref(),
                    parser.take(),
                    status,
                    exclusion_reason,
                );
                Ok((built, source_bytes, observation, pending_relationships))
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let mut built_fragments: Vec<BuiltFragment> = Vec::with_capacity(per_file.len());
    let mut observations = Vec::with_capacity(per_file.len());
    let mut pending_relationships = Vec::new();
    let mut source_bytes_read = 0_u64;
    for (built, source_bytes, observation, pending) in per_file {
        source_bytes_read += source_bytes;
        built_fragments.push(built);
        observations.push(observation);
        if let Some(pending) = pending {
            pending_relationships.push(pending);
        }
    }

    relationships::apply(&mut built_fragments, pending_relationships)
        .map_err(|error| IndexRepoError::Build(BuildFragmentError::Fragment(error)))?;
    let source_indexing_elapsed_us = indexing_started.elapsed().as_micros();

    let mut counts_by_kind: BTreeMap<NodeKind, usize> = BTreeMap::new();
    let mut total_edges = 0_usize;
    for built in &built_fragments {
        for node in built.fragment.nodes() {
            *counts_by_kind.entry(node.kind()).or_default() += 1;
        }
        total_edges += built.fragment.edge_count();
    }

    // Captured before this pass rewrites `units.ndjson`, so pruning can tell
    // what the previous run owned.
    let previously_indexed = previously_indexed_paths(ctx.repo_root());

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

    let currently_indexed: std::collections::HashSet<String> = built_fragments
        .iter()
        .map(|built| built.source_path.to_string())
        .collect();

    // Remove fragments and shards whose source file no longer exists.
    //
    // Writing is not enough to keep the fragment store accurate: a
    // deleted or renamed source leaves its `*.bin` fragment and its
    // `_index/*.ndjson` shard behind forever. The linker keeps reading
    // those shards, so symbols from a file that no longer exists stay
    // in the symbol index and resolve as if they were live.
    //
    // `aethyme graph refresh` avoided this by deleting the whole graph
    // directory before rebuilding, so the divergence was invisible on
    // that path; only a direct `index_repo_to_disk` accumulated stale
    // artifacts. Pruning here makes both paths behave the same.
    let stale_artifacts =
        prune_stale_artifacts(ctx.repo_root(), &previously_indexed, &currently_indexed);

    let coverage = assemble(ctx, &walk, &observations, &built_fragments);
    let coverage_paths = if ctx.source_revision().is_some() {
        let (coverage_path, units_path) =
            write_coverage_artifacts(ctx.repo_root(), &coverage.report, &coverage.units)
                .map_err(IndexRepoError::CoverageWrite)?;
        vec![coverage_path, units_path]
    } else {
        Vec::new()
    };
    let fragment_serialization_elapsed_us = serialization_started.elapsed().as_micros();
    let fragment_bytes_written = fragments_written
        .iter()
        .chain(shards_written.iter())
        .chain(coverage_paths.iter())
        .filter_map(|path| std::fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum();
    let total_nodes = counts_by_kind.values().sum();

    Ok(IndexRepoSummary {
        total_files,
        total_skipped,
        fragments_written,
        shards_written,
        stale_artifacts_removed: stale_artifacts.len(),
        counts_by_kind,
        total_nodes,
        total_edges,
        coverage,
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
    /// Fragment and shard files removed because their source file no
    /// longer exists. Non-zero after a source file is deleted or renamed.
    pub stale_artifacts_removed: usize,
    pub counts_by_kind: BTreeMap<NodeKind, usize>,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub coverage: IndexCoverage,
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

/// Read the source paths recorded by the previous index pass.
///
/// `units.ndjson` names every indexed path, so it is the indexer's own
/// record of what it owns. Reading it must happen *before* this pass
/// rewrites it, which is why the caller captures the set up front.
fn previously_indexed_paths(repo_root: &std::path::Path) -> std::collections::HashSet<String> {
    let mut paths = std::collections::HashSet::new();
    let Ok(contents) = std::fs::read_to_string(repo_root.join(".aethyme/graph/units.ndjson"))
    else {
        return paths;
    };
    for line in contents.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(source) = value.get("path").and_then(|v| v.as_str()) {
            paths.insert(source.to_string());
        }
    }
    paths
}

/// True for a relative path made only of ordinary components, which
/// therefore stays inside whatever directory it is joined onto.
fn is_contained_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// Delete artifacts belonging to source paths this pass no longer indexes.
///
/// Deletion is driven by the previous run's own record rather than by
/// scanning the tree for anything that looks like an artifact, so the
/// set to reclaim is exactly `previously indexed − currently indexed`.
/// That is precise: a hand-placed `.bin` next to the generated ones is
/// not in the record and is never touched.
fn prune_stale_artifacts(
    repo_root: &std::path::Path,
    previously_indexed: &std::collections::HashSet<String>,
    currently_indexed: &std::collections::HashSet<String>,
) -> Vec<PathBuf> {
    let graph_root = repo_root.join(".aethyme").join("graph");
    let mut removed = Vec::new();
    let mut stale_modules = std::collections::HashSet::new();

    for source_path in previously_indexed.difference(currently_indexed) {
        // The record is committed alongside the fragments, so it is
        // repository-controlled: a `..` or absolute path would aim the
        // removal below outside the graph directory.
        if !is_contained_relative_path(source_path) {
            continue;
        }
        let fragment = graph_root.join(format!("{source_path}.bin"));
        if fragment.is_file() && std::fs::remove_file(&fragment).is_ok() {
            removed.push(fragment.clone());
        }
        stale_modules.insert(crate::linker::synthesize_module_name(source_path));
        // Drop the now-empty source directory so the tree does not
        // accumulate one empty directory per deleted file.
        let mut parent = fragment.parent().map(Path::to_path_buf);
        while let Some(directory) = parent {
            if directory == graph_root || std::fs::remove_dir(&directory).is_err() {
                break;
            }
            parent = directory.parent().map(Path::to_path_buf);
        }
    }

    // Remove index shards for modules that no longer exist, so symbols
    // from a deleted file cannot survive in the symbol index and keep
    // resolving as if they were live.
    let current_modules: std::collections::HashSet<String> = currently_indexed
        .iter()
        .map(|path| crate::linker::synthesize_module_name(path))
        .collect();
    if let Ok(entries) = std::fs::read_dir(graph_root.join("_index")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|v| v.to_str()) != Some("ndjson") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if current_modules.contains(stem) || !stale_modules.contains(stem) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
        }
    }

    removed
}

#[derive(Debug)]
pub enum IndexRepoError {
    Walk(FilesystemIndexerError),
    Build(BuildFragmentError),
    FragmentWrite(FragmentWriteError),
    IndexShardWrite(IndexShardWriteError),
    CoverageWrite(CoverageArtifactError),
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
            Self::CoverageWrite(e) => write!(f, "index_repo: {e}"),
            Self::ReadSource {
                source_path,
                message,
            } => write!(f, "index_repo: read {source_path:?}: {message}"),
            Self::Language(e) => write!(f, "index_repo: {e}"),
        }
    }
}

impl std::error::Error for IndexRepoError {}

fn language_error_reason(error: &LanguageIndexError) -> ExclusionReason {
    match error {
        LanguageIndexError::Parse { .. } => ExclusionReason::ParseError,
        LanguageIndexError::NodeConstruction { .. } => ExclusionReason::NodeConstructionError,
    }
}
