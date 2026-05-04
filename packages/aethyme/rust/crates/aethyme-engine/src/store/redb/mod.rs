//! redb-backed storage layers.
//!
//! - `parse_store` — per-file parse cache (replaced `crate::cache::ParseCache`
//!   in Phase 1+2).
//! - `graph_store` — repo-wide graph (replaced the SurrealDB GraphStore in
//!   Phase 3, when the BSL license forced a migration).

pub mod graph_store;
pub mod parse_store;
