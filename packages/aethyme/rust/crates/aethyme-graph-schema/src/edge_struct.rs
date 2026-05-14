//! The full `Edge` struct with its per-kind attributes.
//!
//! Schema doc §4: every edge carries the common envelope (src, dst,
//! kind, confidence, source) plus optional cross-language markers
//! and zero or more call-site / use-site records. Per-edge-kind
//! attributes (`import_path` on Imports, `decision_status` on
//! Decides, etc.) live in the strongly-typed [`EdgeAttributes`]
//! enum, which is the single source of truth for the edge's kind.

use serde::{Deserialize, Serialize};

use crate::attributes::{BindingKind, Confidence, Source};
use crate::edges::EdgeKind;
use crate::identity::NodeId;

// ─── EdgeSite ────────────────────────────────────────────────────────

/// One call site / use site / write site for a multi-edge.
///
/// Per the schema's multi-edge policy: when `a:foo` calls `b:bar`
/// from three call sites, we emit ONE Edge with three EdgeSite
/// entries (not three separate Edges). This preserves the "one
/// logical edge per (src, dst, kind)" property while keeping site
/// detail addressable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeSite {
    pub line: u32,
    pub is_in_branch: bool,
    pub is_in_loop: bool,
    /// Language-specific site kind: "direct", "virtual",
    /// "dynamic", "via_trait", "default_arg", etc. Kept as
    /// Box<str> for flexibility; canonical English-snake_case
    /// values are conventional but not enforced.
    pub kind_tag: Box<str>,
}

// ─── Helper enums for per-kind attributes ────────────────────────────

/// Status of a decision-record edge (`Decides`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Active,
    Superseded,
    Rejected,
}

impl DecisionStatus {
    pub const fn name(self) -> &'static str {
        match self {
            DecisionStatus::Active => "active",
            DecisionStatus::Superseded => "superseded",
            DecisionStatus::Rejected => "rejected",
        }
    }
}

pub const ALL_DECISION_STATUSES: &[DecisionStatus] = &[
    DecisionStatus::Active,
    DecisionStatus::Superseded,
    DecisionStatus::Rejected,
];

/// Sub-kind of a `References` edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKindHint {
    SeeAlso,
    Mentions,
    Alternative,
}

impl ReferenceKindHint {
    pub const fn name(self) -> &'static str {
        match self {
            ReferenceKindHint::SeeAlso => "see_also",
            ReferenceKindHint::Mentions => "mentions",
            ReferenceKindHint::Alternative => "alternative",
        }
    }
}

pub const ALL_REFERENCE_KIND_HINTS: &[ReferenceKindHint] = &[
    ReferenceKindHint::SeeAlso,
    ReferenceKindHint::Mentions,
    ReferenceKindHint::Alternative,
];

// ─── EdgeAttributes ──────────────────────────────────────────────────

/// Per-edge-kind structured attributes.
///
/// The discriminant of this enum IS the edge's kind. [`Edge::kind`]
/// returns it via a small lookup, so the (kind, attributes) pair
/// can never drift out of sync — the kind is derived from the
/// attributes, not stored alongside them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeAttributes {
    Contains,
    Defines,
    Imports {
        import_path: Box<str>,
        is_namespace: bool,
        is_default: bool,
        is_named: bool,
    },
    Calls,
    Implements,
    Inherits,
    Reads,
    Uses,
    Writes,
    Mocks,
    Tests {
        assertion_count: Option<u32>,
    },
    Configures {
        read_at_runtime: bool,
    },
    Decides {
        decision_status: DecisionStatus,
    },
    Deprecates {
        since_version: Option<Box<str>>,
    },
    Documents,
    References {
        kind_hint: ReferenceKindHint,
    },
}

impl EdgeAttributes {
    /// Map attributes to the canonical edge kind.
    pub const fn kind(&self) -> EdgeKind {
        match self {
            EdgeAttributes::Contains => EdgeKind::Contains,
            EdgeAttributes::Defines => EdgeKind::Defines,
            EdgeAttributes::Imports { .. } => EdgeKind::Imports,
            EdgeAttributes::Calls => EdgeKind::Calls,
            EdgeAttributes::Implements => EdgeKind::Implements,
            EdgeAttributes::Inherits => EdgeKind::Inherits,
            EdgeAttributes::Reads => EdgeKind::Reads,
            EdgeAttributes::Uses => EdgeKind::Uses,
            EdgeAttributes::Writes => EdgeKind::Writes,
            EdgeAttributes::Mocks => EdgeKind::Mocks,
            EdgeAttributes::Tests { .. } => EdgeKind::Tests,
            EdgeAttributes::Configures { .. } => EdgeKind::Configures,
            EdgeAttributes::Decides { .. } => EdgeKind::Decides,
            EdgeAttributes::Deprecates { .. } => EdgeKind::Deprecates,
            EdgeAttributes::Documents => EdgeKind::Documents,
            EdgeAttributes::References { .. } => EdgeKind::References,
        }
    }
}

// ─── Edge ────────────────────────────────────────────────────────────

/// A typed edge in the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    src_id: NodeId,
    dst_id: NodeId,
    confidence: Confidence,
    source: Source,
    language_boundary: bool,
    binding_kind: Option<BindingKind>,
    /// Empty for kinds that don't carry sites (Contains, Inherits,
    /// Implements, Documents, etc.). Populated for behavioral
    /// edges (Calls, Reads, Writes, Uses).
    sites: Vec<EdgeSite>,
    attributes: EdgeAttributes,
}

impl Edge {
    pub fn new(
        src_id: NodeId,
        dst_id: NodeId,
        attributes: EdgeAttributes,
        source: Source,
        confidence: Confidence,
    ) -> Self {
        Edge {
            src_id,
            dst_id,
            confidence,
            source,
            language_boundary: false,
            binding_kind: None,
            sites: Vec::new(),
            attributes,
        }
    }

    /// Builder: mark this as a cross-language edge.
    pub fn with_language_boundary(
        mut self,
        binding_kind: BindingKind,
    ) -> Self {
        self.language_boundary = true;
        self.binding_kind = Some(binding_kind);
        self
    }

    /// Builder: add a call/use site to the edge.
    pub fn with_site(mut self, site: EdgeSite) -> Self {
        self.sites.push(site);
        self
    }

    /// Builder: replace all sites at once.
    pub fn with_sites(mut self, sites: Vec<EdgeSite>) -> Self {
        self.sites = sites;
        self
    }

    pub fn src_id(&self) -> &NodeId {
        &self.src_id
    }
    pub fn dst_id(&self) -> &NodeId {
        &self.dst_id
    }
    /// The canonical edge kind, derived from `attributes`.
    pub fn kind(&self) -> EdgeKind {
        self.attributes.kind()
    }
    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
    pub fn source(&self) -> Source {
        self.source
    }
    pub fn is_cross_language(&self) -> bool {
        self.language_boundary
    }
    pub fn binding_kind(&self) -> Option<BindingKind> {
        self.binding_kind
    }
    pub fn sites(&self) -> &[EdgeSite] {
        &self.sites
    }
    pub fn attributes(&self) -> &EdgeAttributes {
        &self.attributes
    }
}
