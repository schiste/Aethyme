//! redb-backed storage layers.
//!
//! This module is intentionally separate from the surrealdb-backed code in
//! `super::{read, write, schema, ...}`. The plan is to migrate one layer at a
//! time; surrealdb code stays in place until each redb layer replaces it.
//!
//! Phase 1 (here): `parse_store` — replaces `crate::cache::ParseCache`.

pub mod parse_store;
