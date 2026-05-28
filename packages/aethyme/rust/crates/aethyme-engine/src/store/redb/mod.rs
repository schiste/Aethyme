//! redb-backed storage layers.
//!
//! - `graph_store` — repo-wide graph (replaced the SurrealDB GraphStore in
//!   Phase 3, when the BSL license forced a migration).

pub mod graph_store;
