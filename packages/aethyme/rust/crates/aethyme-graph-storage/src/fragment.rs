//! The `Fragment` struct: one source file's worth of nodes + edges.
//!
//! A Fragment is the unit of per-file storage in Option C: each
//! source file in the indexed repo produces one `.aethyme/graph/
//! <source-path>.bin` file containing exactly one Fragment, bincode-
//! encoded. The Fragment's `file_path` field MUST match the source
//! file's path (the same path used in the NodeIds of contained
//! nodes), so reading the fragment lets you verify provenance
//! without consulting the filesystem layout.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use aethyme_graph_schema::{Edge, EdgeAttributes, Node, NodeId};

/// One source file's nodes and outgoing edges.
///
/// Fields are public for read access. Construction goes through
/// [`Fragment::new`] which validates and canonicalizes inputs.
///
/// ### Canonical ordering
///
/// Nodes and edges are sorted at construction time (per
/// [`Fragment::new`]) so the bincode serialization is
/// byte-deterministic regardless of the order callers passed them
/// in. Nodes sort by `id`; edges sort by `(src_id, dst_id, kind
/// discriminant)`. This is the load-bearing rule for cross-machine
/// fragment reproducibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fragment {
    /// The source file this fragment covers (relative to the repo
    /// root). Same form as the `file_path` argument that was passed
    /// to the NodeIds inside.
    file_path: Box<str>,
    /// Schema version. Currently 1. Increments on a wire-format
    /// break (which would require a forever-format-change incident).
    schema_version: u32,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fragment {
    /// Current schema version. Per the "no schema versioning" rule,
    /// this is forever 1 — any change requires a deliberate
    /// migration plan. Stored on every fragment so a future reader
    /// can detect mismatches at decode time.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Construct a Fragment, canonicalizing the inputs.
    ///
    /// Nodes are sorted by `id` and deduplicated (first occurrence
    /// in canonical order wins). Edges are sorted by
    /// `(src_id, dst_id, kind_discriminant)` and deduplicated the
    /// same way.
    ///
    /// Silent deduplication (rather than erroring on duplicates)
    /// matches the indexer's real-world output: Python `@overload`
    /// definitions, conditional-branch `def foo` patterns, and TS
    /// declaration merging all produce multiple AST nodes that
    /// hash to the same NodeId. The "right" answer is one node per
    /// NodeId; the indexer doesn't always know it's emitting
    /// duplicates and shouldn't have to. The first one in
    /// canonical sort order wins.
    pub fn new(
        file_path: &str,
        mut nodes: Vec<Node>,
        mut edges: Vec<Edge>,
    ) -> Result<Self, FragmentBuildError> {
        if file_path.is_empty() {
            return Err(FragmentBuildError::EmptyFilePath);
        }

        // Sort nodes by id, then dedup adjacent duplicates (the
        // sort puts all entries with the same id consecutively).
        nodes.sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
        nodes.dedup_by(|a, b| a.id() == b.id());

        // Sort edges by (src_id, dst_id, kind) for canonical
        // ordering. Multi-edges (same logical edge from multiple
        // call sites) collapse to one Edge with sites[] upstream
        // of this layer.
        edges.sort_by(|a, b| {
            a.src_id()
                .as_str()
                .cmp(b.src_id().as_str())
                .then_with(|| a.dst_id().as_str().cmp(b.dst_id().as_str()))
                .then_with(|| (a.kind() as u8).cmp(&(b.kind() as u8)))
        });
        // Dedup on (src, dst, attributes). Including the full
        // attributes payload preserves edges that differ only in
        // attribute fields — most importantly `Imports` edges
        // whose `import_path` distinguishes `import a.b` from
        // `from a import b` when both resolve to the same target
        // (Phase 4.5 linker can produce this shape). The first
        // occurrence in canonical order wins, matching the
        // node-dedup discipline. HashSet retain (rather than
        // dedup_by adjacency) is used because the sort key only
        // includes src/dst/kind — true (src, dst, attributes)
        // duplicates may not be adjacent after the sort.
        let mut seen: HashSet<(NodeId, NodeId, EdgeAttributes)> =
            HashSet::with_capacity(edges.len());
        edges.retain(|e| {
            seen.insert((
                e.src_id().clone(),
                e.dst_id().clone(),
                e.attributes().clone(),
            ))
        });

        Ok(Fragment {
            file_path: file_path.into(),
            schema_version: Self::SCHEMA_VERSION,
            nodes,
            edges,
        })
    }

    /// The source file this fragment covers.
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// The schema version this fragment was written with.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// All nodes in this fragment, in canonical order (by id).
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// All edges in this fragment, in canonical order (by src,
    /// then dst, then kind).
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentBuildError {
    /// `file_path` was empty.
    EmptyFilePath,
    /// Two nodes in the input had the same NodeId.
    DuplicateNodeId { id: NodeId },
    /// Two edges in the input had the same (src, dst, kind).
    /// Multi-edge collapsing should have happened upstream.
    DuplicateEdge { src: NodeId, dst: NodeId },
}

impl std::fmt::Display for FragmentBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => f.write_str("Fragment: file_path must not be empty"),
            Self::DuplicateNodeId { id } => write!(
                f,
                "Fragment: duplicate node id {:?}; each NodeId may \
                 appear at most once per fragment",
                id.as_str(),
            ),
            Self::DuplicateEdge { src, dst } => write!(
                f,
                "Fragment: duplicate edge ({:?} -> {:?}) of the same \
                 kind; multi-edges must be collapsed to one Edge with \
                 sites[] upstream",
                src.as_str(),
                dst.as_str(),
            ),
        }
    }
}

impl std::error::Error for FragmentBuildError {}
