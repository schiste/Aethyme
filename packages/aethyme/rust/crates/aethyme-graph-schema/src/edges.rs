//! The canonical edge-kind taxonomy.
//!
//! `EdgeKind` is the flat enum of every edge type the graph admits.
//! There are 16 variants grouped into four categories (structural,
//! behavioral, test, documentation). Each variant has a stable
//! snake_case canonical name used in fragment bodies and logs.
//!
//! ## Same forever-ordering rule as `NodeKind`
//!
//! See `crate::kinds` for the long-form ordering rule. Summary:
//!
//! 1. Group by category, in the order:
//!    structural → behavioral → test → documentation.
//! 2. Within a group, alphabetical by snake_case canonical name for
//!    the initial set. Future additions append to the tail of their
//!    group (intentionally breaking alphabetical order — that's the
//!    trade-off for stable bincode discriminants).
//! 3. New variants append to the end of their group, never insert.
//!
//! Tests assert that the initial set is alphabetical-within-category,
//! and that categories appear contiguously in `ALL_EDGE_KINDS`.

use serde::{Deserialize, Serialize};

/// Every kind of edge the graph admits. See module docs for the ordering
/// rule.
///
/// `#[serde(rename_all = "snake_case")]` aligns the wire form with the
/// canonical name returned by [`EdgeKind::name`]. The bincode
/// discriminant is determined by variant order in source.
///
/// `Ord` / `PartialOrd` are derived for the same reason as on
/// `NodeKind`: fragment serialization needs a canonical sort by kind
/// (per graph-schema.md §5.4 determinism requirements).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    // ─── Structural (alphabetical) ───────────────────────────────────
    /// Parent → child containment. Example: `repository` → `directory`
    /// → `file` → `class`/`function`. The bedrock structural edge —
    /// every node has at most one `Contains` parent.
    Contains,
    /// Scope → defined symbol. Example: a `function` that defines a
    /// nested `function` inside its body. Distinct from `Contains`
    /// because containment is structural while `Defines` carries
    /// scoping semantics (the nested symbol is in the outer's lexical
    /// scope).
    Defines,
    /// Importer → imported symbol. Example: a `file` or `module` that
    /// imports another module's symbol. Edge attributes capture
    /// import-kind variants (`is_namespace`, `is_default`, `is_named`)
    /// once `Edge` lands in commit 1.12.
    Imports,

    // ─── Behavioral (alphabetical) ───────────────────────────────────
    /// Caller → callee. The single most-used behavioral edge. Carries
    /// a `sites[]` attribute (commit 1.12) when the same call appears
    /// at multiple sites; collapsing multi-edges into one edge with a
    /// sites array is the multi-edge policy locked in graph-schema.md
    /// §4.6.
    Calls,
    /// Class/struct → trait/interface it implements. Distinct from
    /// `Inherits` because implementation semantics (trait/interface
    /// satisfaction) differ from inheritance semantics (subtype
    /// relationship with method/field inheritance).
    Implements,
    /// Subclass → superclass. Single-inheritance languages produce
    /// at most one outgoing `Inherits` per class; multi-inheritance
    /// (Python, C++) produces multiple.
    Inherits,
    /// Statement → field/global being read. Tier-2 only. Captures the
    /// read-side of dataflow at statement granularity. Companion to
    /// `Writes`; together they form the foundation for future
    /// dataflow analysis (ARISE-style DefUse/UseDef).
    Reads,
    /// Reader → field/global accessed. Function-level / method-level
    /// "uses this binding somewhere in its body". Coarser than `Reads`
    /// / `Writes` (which are statement-level). The default behavioral
    /// edge for "this symbol depends on that one" when statement-level
    /// detail is not requested.
    Uses,
    /// Statement → field/global being written. Tier-2 only. See
    /// `Reads` for context.
    Writes,

    // ─── Test (alphabetical) ─────────────────────────────────────────
    /// Test function → mocked symbol. Captures the "this test replaces
    /// that symbol with a stub" relationship. Used by `is_test_adjacent`
    /// derivation.
    Mocks,
    /// Test function → tested symbol. Heuristic — confidence is
    /// carried on the edge (commit 1.5's `Confidence` attribute).
    Tests,

    // ─── Documentation (alphabetical) ────────────────────────────────
    /// Code node → config value it reads. Connects the
    /// documentation-as-graph layer to the code that consumes it.
    /// Edge attribute `read_at_runtime: bool` distinguishes
    /// load-time-only reads from runtime reads.
    Configures,
    /// Doc node → target symbol. The decision-record edge. Carries a
    /// `decision_status` attribute (`active`/`superseded`/`rejected`)
    /// so a decision can be retired in place without deleting the
    /// historical record.
    Decides,
    /// Doc node → symbol marked as deprecated. May carry a
    /// `since_version` attribute when the deprecation is versioned.
    Deprecates,
    /// Doc node → target symbol it explains. The primary
    /// documentation edge: docstrings document their attached symbol,
    /// markdown doc sections document the symbols they reference.
    Documents,
    /// Doc node → symbol it mentions but doesn't fully document.
    /// Edge attribute `kind_hint` distinguishes
    /// `see_also`/`mentions`/`alternative` flavors.
    References,
}

impl EdgeKind {
    /// Canonical snake_case name. Stable for fragment bodies, logs, and
    /// any cross-process surface. See [`crate::kinds::NodeKind::name`]
    /// for the rationale on hand-maintained mappings.
    pub const fn name(self) -> &'static str {
        match self {
            // Structural
            EdgeKind::Contains => "contains",
            EdgeKind::Defines => "defines",
            EdgeKind::Imports => "imports",
            // Behavioral
            EdgeKind::Calls => "calls",
            EdgeKind::Implements => "implements",
            EdgeKind::Inherits => "inherits",
            EdgeKind::Reads => "reads",
            EdgeKind::Uses => "uses",
            EdgeKind::Writes => "writes",
            // Test
            EdgeKind::Mocks => "mocks",
            EdgeKind::Tests => "tests",
            // Documentation
            EdgeKind::Configures => "configures",
            EdgeKind::Decides => "decides",
            EdgeKind::Deprecates => "deprecates",
            EdgeKind::Documents => "documents",
            EdgeKind::References => "references",
        }
    }

    /// Inverse of [`EdgeKind::name`]. Returns [`UnknownEdgeKind`] for
    /// unknown names, mirroring the `Result` shape locked in commit 1.2
    /// for `NodeKind::from_name`.
    pub fn from_name(name: &str) -> Result<EdgeKind, UnknownEdgeKind> {
        Ok(match name {
            // Structural
            "contains" => EdgeKind::Contains,
            "defines" => EdgeKind::Defines,
            "imports" => EdgeKind::Imports,
            // Behavioral
            "calls" => EdgeKind::Calls,
            "implements" => EdgeKind::Implements,
            "inherits" => EdgeKind::Inherits,
            "reads" => EdgeKind::Reads,
            "uses" => EdgeKind::Uses,
            "writes" => EdgeKind::Writes,
            // Test
            "mocks" => EdgeKind::Mocks,
            "tests" => EdgeKind::Tests,
            // Documentation
            "configures" => EdgeKind::Configures,
            "decides" => EdgeKind::Decides,
            "deprecates" => EdgeKind::Deprecates,
            "documents" => EdgeKind::Documents,
            "references" => EdgeKind::References,
            _ => return Err(UnknownEdgeKind { given: name.into() }),
        })
    }

    /// Category this edge kind belongs to. Used by query routing
    /// (e.g. "find all structural edges from X") and by tier-decision
    /// logic in the indexer.
    pub const fn category(self) -> EdgeKindCategory {
        match self {
            EdgeKind::Contains | EdgeKind::Defines | EdgeKind::Imports => {
                EdgeKindCategory::Structural
            }

            EdgeKind::Calls
            | EdgeKind::Implements
            | EdgeKind::Inherits
            | EdgeKind::Reads
            | EdgeKind::Uses
            | EdgeKind::Writes => EdgeKindCategory::Behavioral,

            EdgeKind::Mocks | EdgeKind::Tests => EdgeKindCategory::Test,

            EdgeKind::Configures
            | EdgeKind::Decides
            | EdgeKind::Deprecates
            | EdgeKind::Documents
            | EdgeKind::References => EdgeKindCategory::Documentation,
        }
    }
}

/// Categories of edge kinds, parallel to [`crate::kinds::NodeKindCategory`].
///
/// Categories are part of the schema's stable surface. Adding a new
/// category is a contract change; reordering is a contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKindCategory {
    Structural,
    Behavioral,
    Test,
    Documentation,
}

impl EdgeKindCategory {
    /// Canonical snake_case name. Parallel to
    /// [`crate::kinds::NodeKindCategory::name`].
    pub const fn name(self) -> &'static str {
        match self {
            EdgeKindCategory::Structural => "structural",
            EdgeKindCategory::Behavioral => "behavioral",
            EdgeKindCategory::Test => "test",
            EdgeKindCategory::Documentation => "documentation",
        }
    }

    /// Inverse of [`EdgeKindCategory::name`]. Returns
    /// [`UnknownEdgeKindCategory`] for unknown names.
    pub fn from_name(
        name: &str,
    ) -> Result<EdgeKindCategory, UnknownEdgeKindCategory> {
        Ok(match name {
            "structural" => EdgeKindCategory::Structural,
            "behavioral" => EdgeKindCategory::Behavioral,
            "test" => EdgeKindCategory::Test,
            "documentation" => EdgeKindCategory::Documentation,
            _ => {
                return Err(UnknownEdgeKindCategory { given: name.into() });
            }
        })
    }
}

/// Every variant in declaration order. Same role as
/// [`crate::kinds::ALL_NODE_KINDS`] — canonical iteration handle.
pub const ALL_EDGE_KINDS: &[EdgeKind] = &[
    // Structural (alphabetical)
    EdgeKind::Contains,
    EdgeKind::Defines,
    EdgeKind::Imports,
    // Behavioral (alphabetical)
    EdgeKind::Calls,
    EdgeKind::Implements,
    EdgeKind::Inherits,
    EdgeKind::Reads,
    EdgeKind::Uses,
    EdgeKind::Writes,
    // Test (alphabetical)
    EdgeKind::Mocks,
    EdgeKind::Tests,
    // Documentation (alphabetical)
    EdgeKind::Configures,
    EdgeKind::Decides,
    EdgeKind::Deprecates,
    EdgeKind::Documents,
    EdgeKind::References,
];

/// Every category in declaration order. Parallel to
/// [`crate::kinds::ALL_NODE_KIND_CATEGORIES`].
pub const ALL_EDGE_KIND_CATEGORIES: &[EdgeKindCategory] = &[
    EdgeKindCategory::Structural,
    EdgeKindCategory::Behavioral,
    EdgeKindCategory::Test,
    EdgeKindCategory::Documentation,
];

/// Returned by [`EdgeKind::from_name`] when the given string does not
/// match any known edge kind. Mirrors
/// [`crate::kinds::UnknownNodeKind`] in shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownEdgeKind {
    given: Box<str>,
}

impl UnknownEdgeKind {
    /// The string the caller passed that did not match any edge kind.
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownEdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown edge kind: {:?}", self.given)
    }
}

impl std::error::Error for UnknownEdgeKind {}

/// Returned by [`EdgeKindCategory::from_name`] when the given string
/// does not match any known category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownEdgeKindCategory {
    given: Box<str>,
}

impl UnknownEdgeKindCategory {
    /// The string the caller passed that did not match any category.
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownEdgeKindCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown edge kind category: {:?}", self.given)
    }
}

impl std::error::Error for UnknownEdgeKindCategory {}
