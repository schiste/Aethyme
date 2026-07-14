//! Per-module NDJSON index shards.
//!
//! Schema doc §5.3: `.aethyme/graph/_index/<module>.ndjson` carries
//! lightweight `(module, symbol, kind, node_id, file)` records that
//! resolve `(module, symbol, kind)` queries to a NodeId without
//! touching the full per-file fragment. NDJSON specifically (one
//! record per line) so that:
//!
//! 1. Git's `merge=union` strategy can auto-merge two PRs that add
//!    distinct entries to the same shard.
//! 2. Humans can read it during incident investigation.
//! 3. Append-mostly write patterns don't need to rewrite the whole
//!    shard.
//!
//! ## Canonical ordering
//!
//! Records sort by `(module, symbol, kind_name)` lexicographically
//! before writing. After git's `merge=union` runs, the next
//! `aethyme index` pass re-sorts and de-duplicates. The sort is
//! load-bearing for byte-determinism across contributors.

use serde::{Deserialize, Serialize};

use aethyme_graph_schema::{NodeId, NodeIdParseError, NodeKind};

/// One record in a per-module index shard.
///
/// Fields are public for ergonomic construction (the indexer pipeline
/// will mint these by the thousand). Field order matters for serde —
/// it determines the NDJSON column order — but per the NDJSON spec
/// JSON objects are unordered, so a reader doesn't depend on this.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SymbolRecord {
    /// The logical module this symbol lives in (e.g. `src.cli`).
    /// Same value used as the shard filename.
    pub module: Box<str>,
    /// The symbol's name within its module.
    pub symbol: Box<str>,
    /// The symbol's kind.
    pub kind: NodeKind,
    /// The full NodeId. Lets readers jump from the index to the
    /// full per-file fragment without recomputing the hash.
    pub node_id: NodeId,
    /// The source file path containing the symbol. Lets readers
    /// resolve to the right per-file fragment.
    pub file: Box<str>,
}

/// Encode a sequence of SymbolRecords as NDJSON bytes.
///
/// Records are sorted by `(module, symbol, kind_name)` before
/// serialization. The output ends with a trailing newline so
/// appending another record (rare, but legal under git's
/// `merge=union`) doesn't accidentally concatenate two JSON
/// objects on one line.
pub fn write_index_shard_bytes(records: &[SymbolRecord]) -> Result<Vec<u8>, IndexShardEncodeError> {
    let mut sorted: Vec<&SymbolRecord> = records.iter().collect();
    sorted.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.kind.name().cmp(b.kind.name()))
    });

    let mut out = Vec::with_capacity(records.len() * 128);
    for record in sorted {
        let line = serde_json::to_string(record).map_err(|e| IndexShardEncodeError::Json {
            message: e.to_string(),
        })?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Ok(out)
}

/// Decode NDJSON bytes into a vector of SymbolRecords.
///
/// Tolerates blank lines (which appear naturally after a git
/// `merge=union` if one side ended without a trailing newline).
/// Does NOT deduplicate; that's the indexer's job during the next
/// re-index pass.
pub fn read_index_shard_bytes(bytes: &[u8]) -> Result<Vec<SymbolRecord>, IndexShardDecodeError> {
    let text = std::str::from_utf8(bytes).map_err(|e| IndexShardDecodeError::Utf8 {
        message: e.to_string(),
    })?;
    let mut out = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: SymbolRecord =
            serde_json::from_str(line).map_err(|e| IndexShardDecodeError::Json {
                line: line_index + 1,
                message: e.to_string(),
            })?;
        out.push(record);
    }
    Ok(out)
}

/// Deduplicate a sequence of SymbolRecords, keeping one record per
/// `(module, symbol, kind, node_id, file)`. Stable: when two records
/// would collide, the first one wins. The output is sorted in
/// canonical order.
///
/// Called by the re-index pass after git auto-merge: union-merged
/// shards may have duplicates if two contributors added the same
/// symbol; this resolves them.
pub fn dedupe_and_canonicalize(mut records: Vec<SymbolRecord>) -> Vec<SymbolRecord> {
    records.sort();
    records.dedup();
    records
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexShardEncodeError {
    Json { message: String },
}

impl std::fmt::Display for IndexShardEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json { message } => {
                write!(f, "index shard encode failed: {message}")
            }
        }
    }
}

impl std::error::Error for IndexShardEncodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexShardDecodeError {
    Utf8 {
        message: String,
    },
    Json {
        line: usize,
        message: String,
    },
    InvalidNodeId {
        line: usize,
        cause: NodeIdParseError,
    },
}

impl std::fmt::Display for IndexShardDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Utf8 { message } => {
                write!(f, "index shard decode (utf-8) failed: {message}")
            }
            Self::Json { line, message } => {
                write!(f, "index shard decode failed at line {line}: {message}")
            }
            Self::InvalidNodeId { line, cause } => write!(
                f,
                "index shard decode failed at line {line}: invalid \
                 node_id: {cause}"
            ),
        }
    }
}

impl std::error::Error for IndexShardDecodeError {}
