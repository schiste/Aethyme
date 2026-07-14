//! Common attributes that appear on many nodes and edges.
//!
//! Three types live here:
//!
//! - [`Confidence`] — an integer confidence score in milli-units
//!   (0–1000). Appears on every edge (per schema doc §4.6) and on
//!   any node whose value is heuristically derived (`UnresolvedSymbol`,
//!   `Tests` edges, etc.). Integer-only to preserve byte-determinism
//!   in fragment serialization.
//! - [`Source`] — provenance of a fact: parsed structurally, resolved
//!   from code, or computed at index time. Appears on every edge.
//! - [`BindingKind`] — for cross-language edges, the convention used
//!   to bridge the language boundary (PyO3, napi, wasm-bindgen, FFI,
//!   or unknown). Optional attribute on edges marked
//!   `language_boundary: true`.
//!
//! Other per-kind / per-edge-kind attributes live with their owning
//! kind in `nodes/*` (commits 1.6–1.11) or `edges` (commit 1.12).

use serde::{Deserialize, Deserializer, Serialize};

/// A heuristic confidence score in integer milli-units.
///
/// Range: 0 (no confidence) to 1000 (full confidence). Integer
/// representation rather than `f32` / `f64` to preserve byte-level
/// determinism in fragment serialization — floating-point arithmetic
/// produces platform-specific bit patterns, which would break the
/// "byte-identical fragments on every machine" guarantee from the
/// schema doc §5.4.
///
/// Constructed via [`Confidence::from_milli`], which validates the
/// 0–1000 range. The [`Confidence::ZERO`] and [`Confidence::FULL`]
/// constants cover the two endpoints; intermediate values come from
/// the indexer's heuristic scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Confidence(u16);

impl Confidence {
    /// 0 milli-units. No confidence in the underlying fact.
    pub const ZERO: Self = Confidence(0);

    /// 1000 milli-units. Full confidence (parsed directly from
    /// structure, no heuristic involved).
    pub const FULL: Self = Confidence(1000);

    /// Upper bound — confidences greater than this are rejected.
    pub const MAX_MILLI: u16 = 1000;

    /// Construct a confidence from a milli-unit value. Returns
    /// [`ConfidenceOutOfRange`] if the value exceeds 1000.
    pub const fn from_milli(value: u16) -> Result<Self, ConfidenceOutOfRange> {
        if value > Self::MAX_MILLI {
            return Err(ConfidenceOutOfRange { given: value });
        }
        Ok(Confidence(value))
    }

    /// The raw milli-unit value (0–1000).
    pub const fn as_milli(self) -> u16 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Match Serialize's transparent shape but enforce the range
        // invariant on the way in. Without this, a malformed shard
        // could carry Confidence(65535).
        let raw = <u16 as Deserialize>::deserialize(d)?;
        Confidence::from_milli(raw).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.0, Self::MAX_MILLI)
    }
}

/// Returned by [`Confidence::from_milli`] when the input exceeds
/// 1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfidenceOutOfRange {
    given: u16,
}

impl ConfidenceOutOfRange {
    pub const fn given(self) -> u16 {
        self.given
    }
}

impl std::fmt::Display for ConfidenceOutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Confidence value {} out of range; valid range is 0..={}",
            self.given,
            Confidence::MAX_MILLI,
        )
    }
}

impl std::error::Error for ConfidenceOutOfRange {}

/// Provenance of a fact.
///
/// Every edge and many derived node attributes carry this. Distinct
/// from confidence: `Source::Structure` facts have full confidence
/// but they're still labeled "Structure" to indicate they came from
/// the parser. `Source::Derived` facts may have full confidence too
/// (e.g., `in_degree` is just a count); the source label distinguishes
/// "how was this computed" from "how much do we trust it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Parsed directly from tree-sitter AST.
    Structure,
    /// Resolved via code analysis (e.g., call-graph construction,
    /// symbol resolution).
    Code,
    /// Computed at index time from other facts (e.g., `in_degree`,
    /// `is_unused`).
    Derived,
}

impl Source {
    /// Canonical snake_case name. Parallel to
    /// [`crate::NodeKind::name`].
    pub const fn name(self) -> &'static str {
        match self {
            Source::Structure => "structure",
            Source::Code => "code",
            Source::Derived => "derived",
        }
    }

    /// Inverse of [`Source::name`].
    pub fn from_name(name: &str) -> Result<Source, UnknownSource> {
        Ok(match name {
            "structure" => Source::Structure,
            "code" => Source::Code,
            "derived" => Source::Derived,
            _ => return Err(UnknownSource { given: name.into() }),
        })
    }
}

/// Returned by [`Source::from_name`] when the given string does not
/// match any known source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownSource {
    given: Box<str>,
}

impl UnknownSource {
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown source: {:?}", self.given)
    }
}

impl std::error::Error for UnknownSource {}

/// For cross-language edges, the convention used to bridge the
/// language boundary.
///
/// Most cross-language calls go through one of four well-known
/// bindings; the fifth variant `Unknown` covers anything we can't
/// classify. Edge attributes on `Calls`/`Imports`/`Uses` use this
/// when `language_boundary: true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    /// Python ↔ Rust via PyO3.
    Pyo3,
    /// Node.js ↔ Rust via napi-rs / N-API.
    Napi,
    /// JavaScript ↔ Rust via wasm-bindgen.
    WasmBindgen,
    /// C-style FFI (extern "C" boundaries, dlopen, etc.).
    Ffi,
    /// A cross-language binding we couldn't classify.
    Unknown,
}

impl BindingKind {
    pub const fn name(self) -> &'static str {
        match self {
            BindingKind::Pyo3 => "pyo3",
            BindingKind::Napi => "napi",
            BindingKind::WasmBindgen => "wasm_bindgen",
            BindingKind::Ffi => "ffi",
            BindingKind::Unknown => "unknown",
        }
    }

    pub fn from_name(name: &str) -> Result<BindingKind, UnknownBindingKind> {
        Ok(match name {
            "pyo3" => BindingKind::Pyo3,
            "napi" => BindingKind::Napi,
            "wasm_bindgen" => BindingKind::WasmBindgen,
            "ffi" => BindingKind::Ffi,
            "unknown" => BindingKind::Unknown,
            _ => return Err(UnknownBindingKind { given: name.into() }),
        })
    }
}

/// Returned by [`BindingKind::from_name`] when the given string does
/// not match any known binding kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownBindingKind {
    given: Box<str>,
}

impl UnknownBindingKind {
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownBindingKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown binding kind: {:?}", self.given)
    }
}

impl std::error::Error for UnknownBindingKind {}

/// Every [`Source`] in declaration order.
pub const ALL_SOURCES: &[Source] = &[Source::Structure, Source::Code, Source::Derived];

/// Every [`BindingKind`] in declaration order.
pub const ALL_BINDING_KINDS: &[BindingKind] = &[
    BindingKind::Pyo3,
    BindingKind::Napi,
    BindingKind::WasmBindgen,
    BindingKind::Ffi,
    BindingKind::Unknown,
];
