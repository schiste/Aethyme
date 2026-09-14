//! Revision-bound coverage and code-unit artifacts.
//!
//! These files are deliberately separate from the binary graph fragments.
//! Coverage is an observational contract for downstream tools, while the
//! fragments remain the graph's authoritative payload.  Both artifacts are
//! content-free: code-unit rows carry locations and digests, never source
//! text, embeddings, or vector-store identifiers.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const GRAPH_COVERAGE_RELPATH: &str = ".aethyme/graph/coverage.json";
pub const GRAPH_UNITS_RELPATH: &str = ".aethyme/graph/units.ndjson";
pub const GRAPH_COVERAGE_SCHEMA_VERSION: u32 = 1;

/// The status of a discovered file as seen by the source indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageFileStatus {
    Parsed,
    Partial,
    Unsupported,
    NonCode,
}

/// The status attached to a code-unit row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitCoverageStatus {
    Parsed,
    Partial,
    Unsupported,
}

/// Closed vocabulary for file exclusion diagnostics.  `Other` is the
/// forward-compatible escape hatch; paths and parser diagnostics never enter
/// this map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    UnrecognizedExtension,
    OverSizeLimit,
    ReadError,
    ParserUnavailable,
    ParseError,
    NodeConstructionError,
    SourceRangeUnavailable,
    Other,
}

/// Counts used both at the repository level and in language/parser buckets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageBucket {
    pub files: u64,
    pub bytes: u64,
    pub parsed: u64,
    pub partial: u64,
    pub unsupported: u64,
    pub non_code: u64,
    pub excluded: u64,
}

impl CoverageBucket {
    pub fn observe(&mut self, status: CoverageFileStatus, bytes: u64) {
        self.files += 1;
        self.bytes += bytes;
        match status {
            CoverageFileStatus::Parsed => self.parsed += 1,
            CoverageFileStatus::Partial => self.partial += 1,
            CoverageFileStatus::Unsupported => self.unsupported += 1,
            CoverageFileStatus::NonCode => self.non_code += 1,
        }
    }

    pub fn observe_excluded(&mut self, bytes: u64) {
        self.excluded += 1;
        self.bytes += bytes;
    }
}

/// Repository-wide file and byte counts.  `discovered` includes files the
/// walker rejected; `eligible` is the set that received a graph fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageFiles {
    pub discovered: u64,
    pub eligible: u64,
    pub parsed: u64,
    pub partial: u64,
    pub unsupported: u64,
    pub non_code: u64,
    pub excluded: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageBytes {
    pub discovered: u64,
    pub eligible: u64,
    pub parsed: u64,
    pub partial: u64,
    pub unsupported: u64,
    pub non_code: u64,
    pub excluded: u64,
    /// Bytes actually handed to a parser or surface-flow scanner.
    pub source_read: u64,
}

/// Content-free graph coverage recorded at index time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCoverage {
    pub schema_version: u32,
    pub available: bool,
    pub source_revision: Option<String>,
    pub indexed_revision: Option<String>,
    #[serde(default)]
    pub source_tree_sha256: Option<String>,
    #[serde(default)]
    pub indexed_tree_sha256: Option<String>,
    pub engine_version: String,
    /// `complete` means no known indexing gap. `partial` remains safe to
    /// inspect, but callers must honor `safe_to_use == false`.
    pub coverage_mode: String,
    /// Negative contract: this is false whenever a known gap exists or the
    /// artifact is not bound to an exact source revision.
    pub safe_to_use: bool,
    /// Stable, path-free condition codes.  Counts live in the typed maps
    /// below; order is canonical and deterministic.
    pub gaps: Vec<String>,
    pub files: CoverageFiles,
    pub bytes: CoverageBytes,
    pub by_language: BTreeMap<String, CoverageBucket>,
    pub by_parser: BTreeMap<String, CoverageBucket>,
    pub exclusion_reasons: BTreeMap<ExclusionReason, u64>,
    pub node_counts_by_kind: BTreeMap<String, u64>,
    pub node_counts_by_category: BTreeMap<String, u64>,
    pub edge_counts_by_kind: BTreeMap<String, u64>,
    pub edge_counts_by_category: BTreeMap<String, u64>,
    pub unit_count: u64,
}

impl GraphCoverage {
    pub fn unavailable(engine_version: impl Into<String>) -> Self {
        Self {
            schema_version: GRAPH_COVERAGE_SCHEMA_VERSION,
            available: false,
            source_revision: None,
            indexed_revision: None,
            source_tree_sha256: None,
            indexed_tree_sha256: None,
            engine_version: engine_version.into(),
            coverage_mode: "unavailable".into(),
            safe_to_use: false,
            gaps: vec!["coverage_artifact_missing".into()],
            files: CoverageFiles::default(),
            bytes: CoverageBytes::default(),
            by_language: BTreeMap::new(),
            by_parser: BTreeMap::new(),
            exclusion_reasons: BTreeMap::new(),
            node_counts_by_kind: BTreeMap::new(),
            node_counts_by_category: BTreeMap::new(),
            edge_counts_by_kind: BTreeMap::new(),
            edge_counts_by_category: BTreeMap::new(),
            unit_count: 0,
        }
    }

    pub fn unavailable_with_gap(engine_version: impl Into<String>, gap: impl Into<String>) -> Self {
        let mut coverage = Self::unavailable(engine_version);
        coverage.gaps = vec![gap.into()];
        coverage
    }

    pub fn validate(&self) -> Result<(), CoverageArtifactError> {
        if self.schema_version != GRAPH_COVERAGE_SCHEMA_VERSION {
            return Err(CoverageArtifactError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        if self.available
            && (self.source_revision.as_deref().is_none()
                || self.indexed_revision.as_deref().is_none())
        {
            return Err(CoverageArtifactError::Invalid(
                "available coverage must carry source_revision and indexed_revision".into(),
            ));
        }
        if !self.available && self.safe_to_use {
            return Err(CoverageArtifactError::Invalid(
                "unavailable coverage cannot be safe to use".into(),
            ));
        }
        if self.safe_to_use
            && (self.source_revision != self.indexed_revision
                || self.source_tree_sha256.is_none()
                || self.indexed_tree_sha256.is_none()
                || self.source_tree_sha256 != self.indexed_tree_sha256
                || !self.gaps.is_empty())
        {
            return Err(CoverageArtifactError::Invalid(
                "safe coverage must have matching revisions and no gaps".into(),
            ));
        }
        Ok(())
    }
}

/// A byte location is line-based for compatibility with the graph schema and
/// offset-based for exact slicing/digesting.  Offsets are UTF-8 byte offsets
/// and `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct UnitPosition {
    pub line: u32,
    pub byte_offset: u64,
}

/// One bounded code-unit record.  Anonymous units use a deterministic
/// positional `symbol_identity` synthesized by the indexer.  The digest
/// covers the UTF-8 bytes of the inclusive line range represented by the
/// graph node; leading decorators, doc comments, and trailing comments are
/// included only when that node's source range includes their lines.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GraphUnit {
    pub path: String,
    pub symbol_identity: Option<String>,
    pub kind: String,
    pub language: String,
    pub parser: Option<String>,
    pub start: UnitPosition,
    pub end: UnitPosition,
    pub content_digest: String,
    pub graph_node_ref: String,
    pub coverage_status: UnitCoverageStatus,
}

pub fn sort_units(units: &mut [GraphUnit]) {
    units.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.start.byte_offset.cmp(&right.start.byte_offset))
            .then(left.end.byte_offset.cmp(&right.end.byte_offset))
            .then(left.kind.cmp(&right.kind))
            .then(left.symbol_identity.cmp(&right.symbol_identity))
            .then(left.language.cmp(&right.language))
            .then(left.parser.cmp(&right.parser))
            .then(left.content_digest.cmp(&right.content_digest))
            .then(left.graph_node_ref.cmp(&right.graph_node_ref))
            .then(left.coverage_status.cmp(&right.coverage_status))
    });
}

/// Write both artifacts atomically.  The units are sorted here even when the
/// caller already sorted them, making byte determinism a storage guarantee.
pub fn write_coverage_artifacts(
    repo_root: &Path,
    coverage: &GraphCoverage,
    units: &[GraphUnit],
) -> Result<(PathBuf, PathBuf), CoverageArtifactError> {
    coverage.validate()?;
    let coverage_path = repo_root.join(GRAPH_COVERAGE_RELPATH);
    let units_path = repo_root.join(GRAPH_UNITS_RELPATH);

    let mut coverage_bytes = serde_json::to_vec_pretty(coverage)
        .map_err(|error| CoverageArtifactError::Encode(error.to_string()))?;
    coverage_bytes.push(b'\n');

    let mut canonical_units = units.to_vec();
    sort_units(&mut canonical_units);
    let mut units_bytes = Vec::new();
    for unit in canonical_units {
        let line = serde_json::to_vec(&unit)
            .map_err(|error| CoverageArtifactError::Encode(error.to_string()))?;
        units_bytes.extend_from_slice(&line);
        units_bytes.push(b'\n');
    }

    crate::disk::atomic_write(&coverage_path, &coverage_bytes).map_err(|source| {
        CoverageArtifactError::Io {
            path: coverage_path.clone(),
            source,
        }
    })?;
    crate::disk::atomic_write(&units_path, &units_bytes).map_err(|source| {
        CoverageArtifactError::Io {
            path: units_path.clone(),
            source,
        }
    })?;
    Ok((coverage_path, units_path))
}

pub fn read_coverage(repo_root: &Path) -> Result<GraphCoverage, CoverageArtifactError> {
    let path = repo_root.join(GRAPH_COVERAGE_RELPATH);
    let bytes = std::fs::read(&path).map_err(|source| CoverageArtifactError::Io {
        path: path.clone(),
        source,
    })?;
    let coverage: GraphCoverage = serde_json::from_slice(&bytes)
        .map_err(|error| CoverageArtifactError::Decode(error.to_string()))?;
    coverage.validate()?;
    Ok(coverage)
}

pub fn read_units(repo_root: &Path) -> Result<Vec<GraphUnit>, CoverageArtifactError> {
    let path = repo_root.join(GRAPH_UNITS_RELPATH);
    let bytes = std::fs::read(&path).map_err(|source| CoverageArtifactError::Io {
        path: path.clone(),
        source,
    })?;
    decode_units(&bytes)
}

pub fn decode_units(bytes: &[u8]) -> Result<Vec<GraphUnit>, CoverageArtifactError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| CoverageArtifactError::Decode(error.to_string()))?;
    let mut units = Vec::new();
    for (line, record) in text.lines().enumerate() {
        if record.trim().is_empty() {
            continue;
        }
        let unit = serde_json::from_str(record).map_err(|error| CoverageArtifactError::Unit {
            line: line + 1,
            message: error.to_string(),
        })?;
        units.push(unit);
    }
    sort_units(&mut units);
    Ok(units)
}

#[derive(Debug, thiserror::Error)]
pub enum CoverageArtifactError {
    #[error("coverage artifact I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encode coverage artifact: {0}")]
    Encode(String),
    #[error("decode coverage artifact: {0}")]
    Decode(String),
    #[error("coverage artifact schema {found} is unsupported (expected 1)")]
    UnsupportedSchema { found: u32 },
    #[error("invalid coverage artifact: {0}")]
    Invalid(String),
    #[error("decode unit row {line}: {message}")]
    Unit { line: usize, message: String },
}
