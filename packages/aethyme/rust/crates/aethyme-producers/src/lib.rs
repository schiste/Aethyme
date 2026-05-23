//! Overlay producers: the contract for non-per-file graph payloads.
//!
//! ## Background
//!
//! Per-file [`Fragment`](aethyme_graph_storage::Fragment)s cover
//! what the parser finds inside one source file. The engine has a
//! second class of indexer pass that doesn't fit that shape —
//! repo-level structure, config-value harvesting across many files,
//! risk classification from cross-cutting heuristics, etc. Those
//! used to live in the engine's `passes/` directory and write to
//! redb tables. Phase 4.7 (variant C1) collapses them onto the
//! fragment-store layer through
//! [`OverlayFragment`](aethyme_graph_storage::OverlayFragment), so
//! the engine has exactly one persistence target.
//!
//! ## The trait
//!
//! [`OverlayProducer`] is the contract every such producer
//! implements. It pins three things:
//!
//! - the producer's [`kind`](OverlayProducer::kind) — the stable
//!   filename under `_overlays/<kind>.bin`,
//! - the producer's
//!   [`producer_version`](OverlayProducer::producer_version) — a
//!   free-form string the storage layer embeds in the overlay so
//!   consumers can detect logic drift independently of wire-format
//!   drift,
//! - a [`produce`](OverlayProducer::produce) function that returns
//!   a typed `OverlayFragment<Payload>` from a
//!   [`ProducerCtx`].
//!
//! ## Determinism is the load-bearing property
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every byte the
//! storage layer writes must be reproducible across machines. The
//! storage wrapper enforces this at the envelope level (bincode
//! over scalar/string fields); the producer is responsible for the
//! payload. That means **producers MUST canonicalize their payload
//! before construction** — sort collections, prefer `BTreeMap` over
//! `HashMap`, avoid timestamps, etc.
//!
//! Every concrete `OverlayProducer` implementation must have a test
//! exercising [`harness::assert_overlay_producer_is_deterministic`]
//! against a representative input. This crate ships the harness;
//! the producer crates own the calls.
//!
//! ## Scope of this crate
//!
//! This crate ships the **contract** only. Concrete producers
//! (structure, configs, docs, risks) land in commits 4.7.3–4.7.6
//! per `docs/architecture/phase4-7-c1-cutover.md`. The trait lives
//! here rather than in `aethyme-graph-indexer` because:
//!
//! - `aethyme-graph-indexer` pulls in heavyweight parser
//!   dependencies (`ruff_python_parser`, `oxc_*`, `ra_ap_syntax`,
//!   `tree-sitter-php`). Producers don't parse source — they
//!   synthesize from already-indexed fragments. Co-locating would
//!   force every overlay consumer to inherit gigabytes of parser
//!   deps.
//! - The engine's eventual `populate_from_fragments` cutover
//!   (commit 4.7.7) reads producers but doesn't index anything; a
//!   thin `aethyme-producers` dep matches that consumption pattern.

use aethyme_graph_storage::{FragmentStore, OverlayFragment};

pub mod configs;
pub mod harness;
pub mod packages;
pub mod structure;

pub use configs::{ConfigsOverlay, ConfigsProducer};
pub use harness::assert_overlay_producer_is_deterministic;
pub use packages::{PackageProducer, PackagesOverlay};
pub use structure::{StructureOverlay, StructureProducer};

/// Contract for a non-per-file overlay producer.
///
/// One implementation per overlay kind. The kind name and producer
/// version are part of the producer's identity — they're stamped
/// into the on-disk overlay and used by consumers (the engine, the
/// daemon, the CI determinism gate) to reason about provenance and
/// staleness.
pub trait OverlayProducer {
    /// The overlay's payload type. Kept as an associated type
    /// (rather than a generic parameter) because each producer
    /// produces exactly one shape of payload — a `RisksProducer`
    /// always produces risks, never accidentally structure.
    ///
    /// The bounds match what the storage layer needs to round-trip
    /// the payload through bincode and what the harness needs to
    /// compare two consecutive outputs.
    type Payload: serde::Serialize
        + serde::de::DeserializeOwned
        + Clone
        + PartialEq;

    /// Stable identifier for this overlay. Used as the filename
    /// under `_overlays/<kind>.bin` and embedded in the on-disk
    /// envelope for belt-and-braces verification.
    ///
    /// Must satisfy
    /// [`validate_module_name`](aethyme_graph_storage::validate_module_name)
    /// — same path-safety discipline as per-module index shards.
    fn kind(&self) -> &'static str;

    /// Free-form identifier for the producer's *logic* version.
    /// Bumped whenever the producer's interpretation of inputs
    /// changes, independent of the payload struct's wire format.
    ///
    /// Stored verbatim in the overlay envelope; not parsed by the
    /// storage layer. Suggested convention: `"<kind>/<semver>"`,
    /// e.g. `"risks/0.2.0"`.
    fn producer_version(&self) -> &'static str;

    /// Run the producer against the given context and return a
    /// typed overlay fragment.
    ///
    /// Producers MUST canonicalize their payload before
    /// constructing the fragment — sort collections, prefer
    /// `BTreeMap` over `HashMap`, avoid wall-clock timestamps —
    /// so the encoded bytes are reproducible. The
    /// [`harness`](crate::harness) module ships an assertion that
    /// pins this property; every producer should call it from
    /// tests.
    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError>;
}

/// Inputs available to an overlay producer.
///
/// Carries the [`FragmentStore`] (always present, for fragment +
/// overlay reads) and an optional [`RepoView`] (present only for
/// structure-class producers that need the discovered repo
/// snapshot — the per-file fragment stream alone isn't enough to
/// produce a repo-shaped overlay because individual fragments
/// don't know their siblings).
///
/// The `RepoView` is optional rather than required so the existing
/// determinism harness (which builds a context from a tempdir
/// store and exercises a fake producer) doesn't have to invent a
/// repo view it doesn't use. Producers that need it call
/// [`Self::repo_view`] which returns
/// [`ProducerError::Other`] when absent — that surfaces at the
/// producer's own error path rather than as a panic.
pub struct ProducerCtx<'a> {
    /// The store the producer reads fragments and prior overlays
    /// from. The producer should NOT write through this handle —
    /// the storage layer owns the write path; producers return an
    /// `OverlayFragment` and let callers persist it.
    pub store: &'a FragmentStore,
    /// Discovered repo snapshot. Private because callers should
    /// reach it through [`Self::repo_view`], which yields a
    /// structured error on absence rather than `Option::None`
    /// silently propagating into producer logic.
    repo: Option<&'a dyn RepoView>,
}

impl<'a> ProducerCtx<'a> {
    /// Convenience constructor for store-only contexts (the
    /// determinism harness, producers that read fragments without
    /// needing repo-level context). `repo_view()` returns an error
    /// on the resulting context.
    pub fn new(store: &'a FragmentStore) -> Self {
        Self { store, repo: None }
    }

    /// Construct a context with both the store and a repo view.
    /// Used by structure-class producers (4.7.3+) that synthesize
    /// overlay payloads from the discovered repo snapshot.
    pub fn with_repo(
        store: &'a FragmentStore,
        repo: &'a dyn RepoView,
    ) -> Self {
        Self {
            store,
            repo: Some(repo),
        }
    }

    /// Borrow the repo view, or surface a typed error if this
    /// context was built without one. Producers that need the
    /// repo snapshot call this from the top of `produce()` and
    /// use the `?` operator — the error variant flags a wiring
    /// bug at the caller (someone passed a store-only context to
    /// a structure-class producer) rather than a producer-logic
    /// bug.
    pub fn repo_view(&self) -> Result<&dyn RepoView, ProducerError> {
        self.repo.ok_or_else(|| {
            ProducerError::Other(
                "ProducerCtx built without a RepoView — \
                 structure-class producers require with_repo()"
                    .to_string(),
            )
        })
    }
}

/// A producer's view of a discovered repo snapshot.
///
/// The trait exposes only what a structure-class producer needs:
/// the repo's identity (`name`, `root_path`, `vcs`) and the list
/// of files within it. It deliberately does NOT expose the
/// engine's full `RepositoryMap` because:
///
/// - this crate cannot depend on the engine (parser-heavy deps
///   would propagate to every overlay consumer; also reverse
///   layering),
/// - the engine's map carries pass-derived fields (areas, file
///   roles, line counts) that a *structure* producer should not
///   need — those are the engine's interpretation, and the
///   producer is supplanting that interpretation, not consuming
///   it,
/// - keeping the surface narrow makes the producer trivial to
///   unit-test against in-memory fixtures.
///
/// Implementations live outside this crate (the engine adapts its
/// `RepositoryMap` to this trait at the call site; tests provide
/// in-memory fakes).
pub trait RepoView {
    /// The repo's display name (e.g. `"mediawiki"`). Becomes the
    /// `repo` field in every [`NodeId`](aethyme_graph_schema::NodeId)
    /// the producer mints.
    fn name(&self) -> &str;
    /// The repo's filesystem root, as a string (the schema doesn't
    /// require a `Path`, and stringifying once at the boundary
    /// avoids `OsString` complications across the FFI later).
    fn root_path(&self) -> &str;
    /// Version-control system identifier, e.g. `"git"`.
    fn vcs(&self) -> &str;
    /// The files discovered in this repo. Order is producer-defined
    /// — the structure producer canonicalizes via `BTreeSet` before
    /// emitting fragments.
    fn files(&self) -> &[RepoFileView];
}

/// A producer's view of a single discovered file.
///
/// This is intentionally a struct (not another trait) because the
/// fields are pure data — no behavior, no lifetime gymnastics. The
/// engine adapter copies its `FileNode` fields into this struct at
/// the boundary; tests construct it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFileView {
    /// Relative path from the repo root, forward-slash separated
    /// (the schema's `NodeId` hash inputs require this form).
    pub path: String,
    /// Detected language tag (e.g. `"python"`, `"typescript"`).
    /// `None` for binaries, unknown extensions, etc. — the
    /// producer substitutes `"unknown"` when minting the
    /// [`File`](aethyme_graph_schema::File) node so the schema's
    /// non-empty-language invariant holds.
    pub language: Option<String>,
    /// File size in bytes. Stamped into the
    /// [`File`](aethyme_graph_schema::File) node verbatim.
    pub byte_size: u64,
    /// Content-addressable hash (BLAKE3 hex or similar). The
    /// schema doesn't care about the algorithm; producers and the
    /// engine adapter must agree on it across the cutover so
    /// content_hash equality survives the migration.
    pub content_hash: String,
}

/// Errors a producer can surface to its caller.
///
/// Concrete enum (not `Box<dyn Error>`) so callers can match on
/// known failure modes — particularly storage I/O versus payload
/// construction, which require different recovery (the former
/// usually means the on-disk graph is corrupt and the indexer
/// needs to re-run; the latter is a producer bug).
#[derive(Debug)]
pub enum ProducerError {
    /// The producer failed to read a needed fragment or overlay
    /// from the store. Treat as a corrupt-graph signal; rerun the
    /// indexer.
    Storage(aethyme_graph_storage::StoreLookupError),
    /// The producer attempted to construct an
    /// [`OverlayFragment`](aethyme_graph_storage::OverlayFragment)
    /// with an invalid `kind` or empty `producer_version`. This
    /// is a producer bug — fix the impl.
    Build(aethyme_graph_storage::OverlayBuildError),
    /// Producer-specific failure that doesn't fit either bucket
    /// above. Carry a string for diagnostics; structured cases
    /// can be promoted to their own variants as patterns emerge.
    Other(String),
}

impl std::fmt::Display for ProducerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "producer storage read failed: {e}"),
            Self::Build(e) => write!(f, "producer overlay construction failed: {e}"),
            Self::Other(msg) => write!(f, "producer failed: {msg}"),
        }
    }
}

impl std::error::Error for ProducerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            Self::Build(e) => Some(e),
            Self::Other(_) => None,
        }
    }
}

impl From<aethyme_graph_storage::StoreLookupError> for ProducerError {
    fn from(e: aethyme_graph_storage::StoreLookupError) -> Self {
        Self::Storage(e)
    }
}

impl From<aethyme_graph_storage::OverlayBuildError> for ProducerError {
    fn from(e: aethyme_graph_storage::OverlayBuildError) -> Self {
        Self::Build(e)
    }
}
