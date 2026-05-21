//! `FragmentStore`: a read-side wrapper around a repo's
//! `.aethyme/graph/` directory.
//!
//! Phase 4.3 — provides the query surface that downstream
//! consumers (the engine, the query CLI, future code-nav tooling)
//! call to look up symbols and walk the graph without each one
//! having to re-derive paths from the layout helpers.
//!
//! Read-only. Writes still go through the per-call helpers in
//! `disk.rs` (`write_fragment`, `write_index_shard`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use aethyme_graph_schema::{NodeId, NodeKind};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::disk::{
    read_fragment, read_index_shard, read_overlay, write_overlay,
    FragmentReadError, IndexShardReadError, OverlayReadError,
    OverlayWriteError,
};
use crate::fragment::Fragment;
use crate::index_shard::SymbolRecord;
use crate::layout::{
    AETHYME_DIR, FRAGMENT_EXT, GRAPH_SUBDIR, INDEX_SHARD_EXT, INDEX_SUBDIR,
    OVERLAYS_SUBDIR,
};
use crate::overlay::OverlayFragment;

/// A read-only view of a repo's committed Aethyme graph.
///
/// Construction does NOT preload anything — it just records the
/// repo root and validates the layout exists. Per-query reads
/// happen lazily and aren't cached at this layer (a future
/// caching wrapper can sit on top if needed; tests will tell us
/// when that becomes worthwhile).
#[derive(Debug, Clone)]
pub struct FragmentStore {
    repo_root: PathBuf,
}

impl FragmentStore {
    /// Open the store at the given repo root. Verifies the
    /// `.aethyme/graph/` directory exists; returns
    /// [`StoreOpenError::MissingLayout`] if not (typical cause:
    /// the repo hasn't been bootstrapped via `bootstrap_repo`).
    pub fn open(
        repo_root: impl Into<PathBuf>,
    ) -> Result<Self, StoreOpenError> {
        let repo_root = repo_root.into();
        let graph_dir = repo_root.join(AETHYME_DIR).join(GRAPH_SUBDIR);
        if !graph_dir.is_dir() {
            return Err(StoreOpenError::MissingLayout {
                expected: graph_dir,
            });
        }
        Ok(FragmentStore { repo_root })
    }

    /// The repo root this store reads from.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Read the fragment for the given source path.
    pub fn read_fragment(
        &self,
        source_path: &str,
    ) -> Result<Fragment, FragmentReadError> {
        read_fragment(&self.repo_root, source_path)
    }

    /// Read the per-module index shard for the given module.
    pub fn read_index_shard(
        &self,
        module: &str,
    ) -> Result<Vec<SymbolRecord>, IndexShardReadError> {
        read_index_shard(&self.repo_root, module)
    }

    /// Enumerate every source path that has a fragment on disk.
    ///
    /// Walks `<repo>/.aethyme/graph/` and returns the relative
    /// source paths (with the `.bin` extension stripped). Sorted
    /// for deterministic iteration.
    pub fn list_indexed_source_paths(&self) -> Result<Vec<String>, StoreOpenError> {
        let graph_dir = self.graph_dir();
        let mut paths = Vec::new();
        collect_bin_files(&graph_dir, &graph_dir, &mut paths)
            .map_err(|e| StoreOpenError::Io {
                path: graph_dir.clone(),
                message: e.to_string(),
            })?;
        paths.sort();
        Ok(paths)
    }

    /// Enumerate every module that has an index shard on disk.
    pub fn list_modules(&self) -> Result<Vec<String>, StoreOpenError> {
        let index_dir = self.index_dir();
        if !index_dir.is_dir() {
            // No shards yet — empty result rather than an error.
            return Ok(Vec::new());
        }
        let mut modules = Vec::new();
        let entries = std::fs::read_dir(&index_dir).map_err(|e| {
            StoreOpenError::Io {
                path: index_dir.clone(),
                message: e.to_string(),
            }
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| StoreOpenError::Io {
                path: index_dir.clone(),
                message: e.to_string(),
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(module) = name.strip_suffix(&format!(".{INDEX_SHARD_EXT}"))
            {
                modules.push(module.to_string());
            }
        }
        modules.sort();
        Ok(modules)
    }

    /// Read a typed overlay fragment for the given kind.
    ///
    /// The caller supplies the payload type `P` they expect; a
    /// `KindMismatch` from the decoder means either the wrong `P`
    /// was supplied or the file was tampered with. See
    /// [`OverlayFragment`] for the versioning contract.
    pub fn read_overlay<P: DeserializeOwned>(
        &self,
        kind: &str,
    ) -> Result<OverlayFragment<P>, OverlayReadError> {
        read_overlay(&self.repo_root, kind)
    }

    /// Write a typed overlay fragment for the given kind. The kind
    /// must match `overlay.kind()`; the disk layer enforces this
    /// belt-and-braces.
    pub fn write_overlay<P: Serialize>(
        &self,
        kind: &str,
        overlay: &OverlayFragment<P>,
    ) -> Result<PathBuf, OverlayWriteError> {
        write_overlay(&self.repo_root, kind, overlay)
    }

    /// Enumerate every overlay kind that has a file on disk.
    /// Returns kinds with the `.bin` extension stripped, sorted.
    pub fn list_overlays(&self) -> Result<Vec<String>, StoreOpenError> {
        let overlays_dir = self.overlays_dir();
        if !overlays_dir.is_dir() {
            // No overlays yet — empty result rather than an error.
            return Ok(Vec::new());
        }
        let mut kinds = Vec::new();
        let entries = std::fs::read_dir(&overlays_dir).map_err(|e| {
            StoreOpenError::Io {
                path: overlays_dir.clone(),
                message: e.to_string(),
            }
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| StoreOpenError::Io {
                path: overlays_dir.clone(),
                message: e.to_string(),
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(kind) = name.strip_suffix(&format!(".{FRAGMENT_EXT}"))
            {
                kinds.push(kind.to_string());
            }
        }
        kinds.sort();
        Ok(kinds)
    }

    /// Look up symbols by (optional module, name, optional kind).
    ///
    /// If `module` is `Some`, only that module's shard is consulted
    /// (fast path: one shard read). If `None`, every shard is
    /// scanned (slow path: O(shards × records)). If `kind` is
    /// `Some`, results are filtered to that kind.
    pub fn find_symbols(
        &self,
        module: Option<&str>,
        name: &str,
        kind: Option<NodeKind>,
    ) -> Result<Vec<SymbolRecord>, StoreLookupError> {
        let modules = match module {
            Some(m) => vec![m.to_string()],
            None => self.list_modules().map_err(StoreLookupError::Open)?,
        };
        let mut hits = Vec::new();
        for m in &modules {
            let records = match self.read_index_shard(m) {
                Ok(r) => r,
                // A missing shard for a listed module would be
                // weird, but we'd rather skip than fail the
                // whole query.
                Err(IndexShardReadError::Io(_)) => continue,
                Err(e) => return Err(StoreLookupError::Read(e)),
            };
            for record in records {
                if &*record.symbol != name {
                    continue;
                }
                if let Some(want_kind) = kind
                    && record.kind != want_kind
                {
                    continue;
                }
                hits.push(record);
            }
        }
        hits.sort_by(|a, b| {
            a.module
                .cmp(&b.module)
                .then_with(|| a.kind.name().cmp(b.kind.name()))
                .then_with(|| a.file.cmp(&b.file))
        });
        Ok(hits)
    }

    /// Look up the fragment containing a specific NodeId.
    ///
    /// Returns the fragment, or None if no fragment claims to
    /// hold that NodeId.
    pub fn fragment_for_node(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<Fragment>, StoreLookupError> {
        // Walk fragments and check each for the node. O(fragments)
        // — fine for repos with hundreds of fragments, becomes
        // slow at MediaWiki scale (~10K fragments). A
        // node_id → source_path index is a natural future
        // optimization once we have a use case.
        let source_paths = self
            .list_indexed_source_paths()
            .map_err(StoreLookupError::Open)?;
        for source_path in source_paths {
            let frag = self
                .read_fragment(&source_path)
                .map_err(StoreLookupError::Fragment)?;
            if frag.nodes().iter().any(|n| n.id() == node_id) {
                return Ok(Some(frag));
            }
        }
        Ok(None)
    }

    /// Count nodes by kind across the whole store. Useful for
    /// observability/health checks.
    pub fn count_nodes_by_kind(
        &self,
    ) -> Result<std::collections::BTreeMap<NodeKind, usize>, StoreLookupError>
    {
        let mut counts: std::collections::BTreeMap<NodeKind, usize> =
            Default::default();
        let source_paths = self
            .list_indexed_source_paths()
            .map_err(StoreLookupError::Open)?;
        for source_path in source_paths {
            let frag = self
                .read_fragment(&source_path)
                .map_err(StoreLookupError::Fragment)?;
            for node in frag.nodes() {
                *counts.entry(node.kind()).or_default() += 1;
            }
        }
        Ok(counts)
    }

    /// Distinct languages observed across all `File` nodes in the
    /// store. Read from each File's `language` field. Useful for
    /// "which languages does this repo have?"
    pub fn list_languages(&self) -> Result<BTreeSet<String>, StoreLookupError> {
        use aethyme_graph_schema::Node;
        let mut langs: BTreeSet<String> = BTreeSet::new();
        let source_paths = self
            .list_indexed_source_paths()
            .map_err(StoreLookupError::Open)?;
        for source_path in source_paths {
            let frag = self
                .read_fragment(&source_path)
                .map_err(StoreLookupError::Fragment)?;
            for node in frag.nodes() {
                if let Node::File(f) = node {
                    langs.insert(f.language().to_string());
                }
            }
        }
        Ok(langs)
    }

    // ─── Internal ──────────────────────────────────────────────

    fn graph_dir(&self) -> PathBuf {
        self.repo_root.join(AETHYME_DIR).join(GRAPH_SUBDIR)
    }

    fn index_dir(&self) -> PathBuf {
        self.graph_dir().join(INDEX_SUBDIR)
    }

    fn overlays_dir(&self) -> PathBuf {
        self.graph_dir().join(OVERLAYS_SUBDIR)
    }
}

fn collect_bin_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    let entries = std::fs::read_dir(current)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip the index subdir and the .gitattributes file at
        // graph root.
        if name == INDEX_SUBDIR
            || name == OVERLAYS_SUBDIR
            || name.starts_with('.')
        {
            continue;
        }
        if path.is_dir() {
            collect_bin_files(root, &path, out)?;
        } else if let Some(rel_with_ext) = name.strip_suffix(&format!(".{FRAGMENT_EXT}"))
        {
            // Reconstruct repo-relative source path.
            let parent_rel = path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .unwrap_or(Path::new(""));
            let mut source_path = String::new();
            for (i, comp) in parent_rel.components().enumerate() {
                if i > 0 {
                    source_path.push('/');
                }
                source_path.push_str(&comp.as_os_str().to_string_lossy());
            }
            if !source_path.is_empty() {
                source_path.push('/');
            }
            source_path.push_str(rel_with_ext);
            out.push(source_path);
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum StoreOpenError {
    MissingLayout { expected: PathBuf },
    Io { path: PathBuf, message: String },
}

impl std::fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLayout { expected } => write!(
                f,
                "FragmentStore: expected graph directory at {expected:?} \
                 (repo bootstrapped? run `aethyme-graph-index` first)"
            ),
            Self::Io { path, message } => {
                write!(f, "FragmentStore: I/O at {path:?}: {message}")
            }
        }
    }
}

impl std::error::Error for StoreOpenError {}

#[derive(Debug)]
pub enum StoreLookupError {
    Open(StoreOpenError),
    Fragment(FragmentReadError),
    Read(IndexShardReadError),
}

impl std::fmt::Display for StoreLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "store lookup: {e}"),
            Self::Fragment(e) => write!(f, "store lookup: {e}"),
            Self::Read(e) => write!(f, "store lookup: {e}"),
        }
    }
}

impl std::error::Error for StoreLookupError {}
