//! The `Fragment` struct: one source file's worth of nodes + edges.
//!
//! A Fragment is the unit of per-file storage in Option C: each
//! source file in the indexed repo produces one `.aethyme/graph/
//! <source-path>.bin` file containing exactly one Fragment, bincode-
//! encoded. The Fragment's `file_path` field MUST match the source
//! file's path (the same path used in the NodeIds of contained
//! nodes), so reading the fragment lets you verify provenance
//! without consulting the filesystem layout.

use serde::{Deserialize, Serialize};

use aethyme_graph_schema::{Edge, Node, NodeId};

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
    /// Nodes are sorted by `id`; edges are sorted by
    /// `(src_id, dst_id, kind_discriminant)`. Duplicate node IDs
    /// or duplicate `(src_id, dst_id, kind)` edges return errors —
    /// a fragment must have a unique representation per logical
    /// content.
    pub fn new(
        file_path: &str,
        mut nodes: Vec<Node>,
        mut edges: Vec<Edge>,
    ) -> Result<Self, FragmentBuildError> {
        if file_path.is_empty() {
            return Err(FragmentBuildError::EmptyFilePath);
        }

        // Sort nodes by id for canonical order.
        nodes.sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
        // Detect duplicate node ids.
        for w in nodes.windows(2) {
            if w[0].id() == w[1].id() {
                return Err(FragmentBuildError::DuplicateNodeId {
                    id: w[0].id().clone(),
                });
            }
        }

        // Sort edges by (src_id, dst_id, kind).
        edges.sort_by(|a, b| {
            a.src_id()
                .as_str()
                .cmp(b.src_id().as_str())
                .then_with(|| a.dst_id().as_str().cmp(b.dst_id().as_str()))
                .then_with(|| (a.kind() as u8).cmp(&(b.kind() as u8)))
        });
        // Detect duplicate (src, dst, kind) edges. Multi-edges
        // (same (src,dst,kind) appearing from multiple call sites)
        // must be collapsed into one Edge with sites[] before
        // reaching here.
        for w in edges.windows(2) {
            if w[0].src_id() == w[1].src_id()
                && w[0].dst_id() == w[1].dst_id()
                && w[0].kind() == w[1].kind()
            {
                return Err(FragmentBuildError::DuplicateEdge {
                    src: w[0].src_id().clone(),
                    dst: w[0].dst_id().clone(),
                });
            }
        }

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
            Self::EmptyFilePath => {
                f.write_str("Fragment: file_path must not be empty")
            }
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
