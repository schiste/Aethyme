//! Structure overlay producer.
//!
//! Synthesizes the repo's container skeleton — one
//! [`Repository`], a [`Directory`] per implied parent, a [`File`]
//! per discovered file, and one [`Contains`](EdgeAttributes::Contains)
//! edge per parent→child relationship. Replaces the engine's
//! `passes/structure.rs` as part of the Phase 4.7 Variant C1
//! cutover (`docs/architecture/phase4-7-c1-cutover.md`).
//!
//! ## What this producer drops vs. the engine's pass
//!
//! The engine's `StructurePass` also emitted:
//!
//! - `AreaNode`s and the `Repository → Area → Directory/File`
//!   routing,
//! - `BelongsTo` edges from every dir/file with an `area_id`,
//! - inferred subareas with their own `Contains` rewiring.
//!
//! Areas are deferred to commit 4.7.7 per the cutover plan, so
//! every container's parent is either its lexical parent directory
//! (if one exists) or the repository itself. This collapses the
//! engine's three-branch routing (parent_dir | area | repo) into
//! two branches (parent_dir | repo), and removes the `BelongsTo`
//! plane entirely.
//!
//! ## Determinism
//!
//! Per `docs/architecture/graph-schema.md §5.4`, on-disk bytes
//! must be reproducible. This producer:
//!
//! - enumerates implied directories via a `BTreeSet` (structural
//!   ordering),
//! - emits `directories` and `files` in the order they came out
//!   of those sorted sources,
//! - sorts edges by `(src_id, dst_id)` before returning the
//!   fragment.
//!
//! `tests/structure_determinism.rs` exercises
//! [`assert_overlay_producer_is_deterministic`] against a
//! fixture, so any future drift here trips at the producer rather
//! than at the §5.7 CI determinism gate.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_graph_schema::{
    Confidence, Directory, Edge, EdgeAttributes, File, NodeId, Repository, Source,
};
use aethyme_graph_storage::OverlayFragment;
use serde::{Deserialize, Serialize};

use crate::{OverlayProducer, ProducerCtx, ProducerError};

/// The payload of the `structure` overlay.
///
/// All fields are schema types — no engine-private mirror of
/// `DirectoryNode`/`FileNode` lives in this crate. The cutover's
/// load-bearing property is "one definition for graph types,"
/// and the producer is the place that property lands.
///
/// Ordering inside each `Vec` is producer-canonical and
/// determinism-relevant; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructureOverlay {
    /// The single [`Repository`] for this overlay.
    pub repository: Repository,
    /// Every directory implied by the discovered file set. A
    /// directory is "implied" if at least one file lives at or
    /// beneath that path.
    pub directories: Vec<Directory>,
    /// Every file in the [`RepoView`](crate::RepoView). One
    /// [`File`] node per [`RepoFileView`](crate::RepoFileView).
    pub files: Vec<File>,
    /// All `Contains` edges. Sorted by `(src_id, dst_id)` so the
    /// bincode encoding is stable across runs.
    pub edges: Vec<Edge>,
}

/// Zero-sized handle that implements [`OverlayProducer`] for the
/// structure overlay.
///
/// Stateless on purpose — the producer's behavior is fully
/// determined by the [`ProducerCtx`]'s
/// [`RepoView`](crate::RepoView). Anything that would make this
/// struct stateful (e.g. a config knob) belongs on the context,
/// not here, so two `StructureProducer` instances are
/// indistinguishable and the determinism harness can replay them
/// freely.
pub struct StructureProducer;

impl OverlayProducer for StructureProducer {
    type Payload = StructureOverlay;

    fn kind(&self) -> &'static str {
        "structure"
    }

    fn producer_version(&self) -> &'static str {
        "structure/0.1.0"
    }

    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let view = ctx.repo_view()?;

        // -- Repository ----------------------------------------------
        let repository = Repository::new(view.name(), view.root_path(), view.vcs())
            .map_err(|e| ProducerError::Other(format!("Repository::new failed: {e}")))?;

        // -- Directories (one per implied parent path) ---------------
        // BTreeSet handles dedupe-and-sort in one pass. The engine
        // does the same with a `BTreeSet<String>` collector, so the
        // implied-directory set across both implementations matches
        // by construction.
        let mut implied: BTreeSet<String> = BTreeSet::new();
        for f in view.files() {
            for parent in parent_directories(&f.path) {
                implied.insert(parent);
            }
        }
        let directories: Vec<Directory> = implied
            .iter()
            .map(|p| Directory::new(view.name(), p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ProducerError::Other(format!("Directory::new failed: {e}")))?;

        // -- Files ---------------------------------------------------
        // Iterate the view in its given order, then sort by NodeId
        // after construction. We can't sort by path alone here
        // because schema's `File::new` is fallible; sorting before
        // construction wouldn't change the eventual ordering since
        // NodeId is a deterministic function of (kind, repo, path).
        let mut files: Vec<File> = view
            .files()
            .iter()
            .map(|f| {
                File::new(
                    view.name(),
                    &f.path,
                    f.language.as_deref().unwrap_or("unknown"),
                    f.byte_size,
                    &f.content_hash,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ProducerError::Other(format!("File::new failed: {e}")))?;
        files.sort_by(|a, b| a.path().cmp(b.path()));

        // -- Contains edges ------------------------------------------
        // Path → Directory lookup so we can route children to their
        // lexical parent without rebuilding NodeIds from scratch.
        // Path strings are unique within a repo so this map is
        // injective.
        let dir_by_path: BTreeMap<&str, &Directory> =
            directories.iter().map(|d| (d.path(), d)).collect();

        let repo_id = repository.id().clone();
        let mk_edge = |src: NodeId, dst: NodeId| -> Edge {
            Edge::new(
                src,
                dst,
                EdgeAttributes::Contains,
                Source::Structure,
                Confidence::FULL,
            )
        };

        let mut edges: Vec<Edge> = Vec::with_capacity(directories.len() + files.len());

        for d in &directories {
            let src = parent_path(d.path())
                .as_deref()
                .and_then(|p| dir_by_path.get(p))
                .map(|pd| pd.id().clone())
                .unwrap_or_else(|| repo_id.clone());
            edges.push(mk_edge(src, d.id().clone()));
        }

        for f in &files {
            let src = parent_path(f.path())
                .as_deref()
                .and_then(|p| dir_by_path.get(p))
                .map(|pd| pd.id().clone())
                .unwrap_or_else(|| repo_id.clone());
            edges.push(mk_edge(src, f.id().clone()));
        }

        // Canonical edge ordering. NodeId implements Ord
        // structurally (string-shaped), so this is stable across
        // platforms.
        edges.sort_by(|a, b| (a.src_id(), a.dst_id()).cmp(&(b.src_id(), b.dst_id())));

        let payload = StructureOverlay {
            repository,
            directories,
            files,
            edges,
        };

        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

/// Every parent path implied by `path`, in lexical order, with
/// the trailing component (the leaf) excluded.
///
/// `"a/b/c.py"` → `["a", "a/b"]`. `"top.py"` → `[]`.
///
/// Historical copy of the engine helper that produced implied parent
/// directories before the pass pipeline was deleted. Duplicated here
/// rather than hidden behind another crate split because the function
/// is small and pure.
fn parent_directories(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return Vec::new();
    }
    (0..parts.len() - 1)
        .map(|i| parts[..=i].join("/"))
        .collect()
}

/// The immediate parent of `path`, or `None` if `path` is
/// already at the repo root.
///
/// `"a/b/c"` → `Some("a/b")`. `"top.py"` → `None`.
fn parent_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return None;
    }
    parts.pop();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_directories_top_level_is_empty() {
        assert!(parent_directories("README.md").is_empty());
    }

    #[test]
    fn parent_directories_nested() {
        assert_eq!(
            parent_directories("src/a/b/c.py"),
            vec!["src", "src/a", "src/a/b"]
        );
    }

    #[test]
    fn parent_path_top_level_is_none() {
        assert_eq!(parent_path("README.md"), None);
    }

    #[test]
    fn parent_path_nested() {
        assert_eq!(parent_path("src/a/b.py"), Some("src/a".to_string()));
    }
}
