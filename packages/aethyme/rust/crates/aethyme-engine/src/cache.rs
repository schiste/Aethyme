//! Per-file parse cache types.
//!
//! Storage moved to `crate::store::redb::parse_store::ParseStore` (Phase 1 of
//! the redb migration). This file now only contains the value type stored in
//! that table plus a small content hasher.

use sha2::{Digest, Sha256};

use crate::model::edge::Edge;
use crate::model::symbol::Symbol;

/// One file's parse output. Stored as a bincoded value in the redb
/// `parse_entries` table; see `crate::store::redb::parse_store`.
///
/// Field order is part of the on-disk format — bincode is positional and the
/// `BuildSession::insert_borrowed` view struct relies on this ordering for
/// round-trip equivalence. Don't reorder without bumping the parse-store
/// schema version.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
    pub import_edges: Vec<Edge>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_produces_consistent_output() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(sha256_hex("hello world"), hash);
        assert_ne!(sha256_hex("different"), hash);
    }
}
