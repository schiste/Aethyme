//! Overlay fragments: typed, non-per-file producer outputs.
//!
//! Per-file [`Fragment`](crate::Fragment)s cover what the parser
//! finds *inside one source file*. Some indexer passes don't fit
//! that shape — they synthesize repo-level structure, harvest
//! config values across many files, classify risk areas from
//! heuristics, etc. Those producers used to live in the engine's
//! `passes/` directory and write to redb tables; Phase 4.7
//! (variant C1) collapses them into the fragment-store layer so
//! the engine has exactly one persistence target.
//!
//! An `OverlayFragment<P>` is the on-disk unit of that absorption:
//! one file per overlay kind under `<repo>/.aethyme/graph/
//! _overlays/<kind>.bin`, sibling of the per-module `_index/`
//! shards.
//!
//! ## Why generic over `P`
//!
//! Each overlay kind has its own payload type defined in the
//! indexer crate (Phase 4.7.3+: `StructureOverlay`, `ConfigsOverlay`,
//! `DocsOverlay`, `RisksOverlay`, …). Encoding each into its own
//! bincode-discriminant space — rather than under one giant
//! `Overlay` enum — means adding a new producer never disturbs
//! existing overlays' bytes. Tradeoff: callers must know which
//! `P` to decode. That's a non-issue because the caller asked for
//! a specific kind (they typed `read_overlay::<RisksOverlay>(…)`).
//!
//! ## Determinism contract
//!
//! The wrapper guarantees deterministic bytes *given a deterministic
//! payload*. Producers are responsible for canonicalizing their
//! payload — usually by sorting collections before constructing the
//! overlay. Per-kind payload types should document and test their
//! own ordering discipline the same way [`Fragment::new`] sorts
//! nodes and edges.
//!
//! ## Versioning
//!
//! Two independent version fields:
//!
//! - `schema_version` — the wire format. Forever 1 per schema-doc
//!   §5.4 (any bump is a forever-format-change incident).
//! - `producer_version` — a free-form string identifying the
//!   producer's *logic* version. Lets the indexer detect "the
//!   bytes parse fine, but the meaning has drifted" separately
//!   from "the bytes won't parse." Use case: bumping the area-
//!   inference heuristic without changing the payload struct.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::layout::{InvalidPath, validate_module_name};

/// A typed overlay payload written to `_overlays/<kind>.bin`.
///
/// Fields are public for read access. Construction goes through
/// [`OverlayFragment::new`] which records the current
/// [`SCHEMA_VERSION`](Self::SCHEMA_VERSION) and stamps the supplied
/// `kind` and `producer_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayFragment<P> {
    /// The overlay kind (e.g. `"structure"`, `"configs"`,
    /// `"docs"`, `"risks"`, `"areas"`). Duplicated from the
    /// filename for provenance verification at decode time —
    /// same discipline as [`Fragment::file_path`](crate::Fragment::file_path).
    kind: Box<str>,
    /// Wire-format version. Currently 1. A bump requires the
    /// forever-format-change ceremony described in graph-schema
    /// §5.4.
    schema_version: u32,
    /// Free-form producer-logic version. Bumped by the producer
    /// when its *interpretation* of inputs changes, independent
    /// of the payload struct's wire format. The storage layer
    /// records this verbatim and never inspects it; consumers
    /// (indexer rerun decisions, daemon staleness checks) read
    /// it through [`producer_version`](Self::producer_version).
    producer_version: Box<str>,
    /// The kind-specific payload.
    payload: P,
}

impl<P> OverlayFragment<P> {
    /// Current wire-format version. Per the "no schema versioning"
    /// rule in graph-schema §5.4, this is forever 1.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Construct an OverlayFragment. Validates `kind` against
    /// [`validate_module_name`] (overlay kinds share the same
    /// path-safety rules as module names — no separators, no NUL,
    /// no `.`/`..`).
    pub fn new(kind: &str, producer_version: &str, payload: P) -> Result<Self, OverlayBuildError> {
        validate_module_name(kind).map_err(OverlayBuildError::Kind)?;
        if producer_version.is_empty() {
            return Err(OverlayBuildError::EmptyProducerVersion);
        }
        Ok(Self {
            kind: kind.into(),
            schema_version: Self::SCHEMA_VERSION,
            producer_version: producer_version.into(),
            payload,
        })
    }

    /// The overlay kind. Same string as the on-disk filename
    /// (without the `.bin` extension).
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The wire-format version this overlay was written with.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The producer's logic version.
    pub fn producer_version(&self) -> &str {
        &self.producer_version
    }

    /// The kind-specific payload (borrowed).
    pub fn payload(&self) -> &P {
        &self.payload
    }

    /// Consume the wrapper and return the payload.
    pub fn into_payload(self) -> P {
        self.payload
    }
}

// ─── Bincode I/O ─────────────────────────────────────────────────────

/// Encode an OverlayFragment to bincode bytes.
///
/// Determinism: bincode produces stable bytes for a deterministic
/// input, and the wrapper fields (`kind`, `schema_version`,
/// `producer_version`) are scalar/string. Whether the *bytes* are
/// reproducible across machines therefore reduces entirely to
/// whether `P` itself serializes deterministically — i.e. whether
/// the payload's collections are sorted, its maps are
/// `BTreeMap`s rather than `HashMap`s, etc. Producers own that
/// discipline; see each per-kind payload type's docs.
pub fn write_overlay_bytes<P: Serialize>(
    overlay: &OverlayFragment<P>,
) -> Result<Vec<u8>, OverlayEncodeError> {
    bincode::serialize(overlay).map_err(|e| OverlayEncodeError::Bincode {
        message: e.to_string(),
    })
}

/// Decode bincode bytes into an OverlayFragment of the requested
/// payload type and verify the kind and schema version.
///
/// The caller passes the expected `kind` so the decoder can fail
/// fast on a mismatch (e.g. a `_overlays/risks.bin` accidentally
/// decoded as a `StructureOverlay`). Schema-version mismatch
/// surfaces as [`OverlayDecodeError::SchemaVersionMismatch`] —
/// loud failure rather than silent compat, matching
/// [`read_fragment_bytes`](crate::read_fragment_bytes).
pub fn read_overlay_bytes<P: DeserializeOwned>(
    bytes: &[u8],
    expected_kind: &str,
) -> Result<OverlayFragment<P>, OverlayDecodeError> {
    let overlay: OverlayFragment<P> =
        bincode::deserialize(bytes).map_err(|e| OverlayDecodeError::Bincode {
            message: e.to_string(),
        })?;
    if overlay.schema_version() != OverlayFragment::<P>::SCHEMA_VERSION {
        return Err(OverlayDecodeError::SchemaVersionMismatch {
            expected: OverlayFragment::<P>::SCHEMA_VERSION,
            found: overlay.schema_version(),
        });
    }
    if overlay.kind() != expected_kind {
        return Err(OverlayDecodeError::KindMismatch {
            expected: expected_kind.into(),
            found: overlay.kind().into(),
        });
    }
    Ok(overlay)
}

// ─── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayBuildError {
    /// `kind` failed [`validate_module_name`].
    Kind(InvalidPath),
    /// `producer_version` was empty. Producers must always stamp
    /// a non-empty logic-version identifier.
    EmptyProducerVersion,
}

impl std::fmt::Display for OverlayBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kind(e) => write!(f, "OverlayFragment: invalid kind: {e}"),
            Self::EmptyProducerVersion => {
                f.write_str("OverlayFragment: producer_version must not be empty")
            }
        }
    }
}

impl std::error::Error for OverlayBuildError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEncodeError {
    Bincode { message: String },
}

impl std::fmt::Display for OverlayEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bincode { message } => {
                write!(f, "OverlayFragment encode failed: {message}")
            }
        }
    }
}

impl std::error::Error for OverlayEncodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayDecodeError {
    Bincode {
        message: String,
    },
    /// Wire-format version on disk differs from the reader's.
    /// Re-run the producer to get a current overlay.
    SchemaVersionMismatch {
        expected: u32,
        found: u32,
    },
    /// The `kind` field on disk doesn't match what the caller
    /// asked for. Usually means the wrong `P` was supplied or the
    /// filename was tampered with.
    KindMismatch {
        expected: Box<str>,
        found: Box<str>,
    },
}

impl std::fmt::Display for OverlayDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bincode { message } => {
                write!(f, "OverlayFragment decode failed: {message}")
            }
            Self::SchemaVersionMismatch { expected, found } => write!(
                f,
                "OverlayFragment schema_version mismatch: reader \
                 expects {expected}, overlay was written with {found}. \
                 Re-run the producer.",
            ),
            Self::KindMismatch { expected, found } => write!(
                f,
                "OverlayFragment kind mismatch: expected {expected:?}, \
                 file contains {found:?}",
            ),
        }
    }
}

impl std::error::Error for OverlayDecodeError {}
