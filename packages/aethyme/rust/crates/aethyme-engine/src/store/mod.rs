//! Persistence layer for the Aethyme engine.
//!
//! As of Phase 3 the engine has a single backend — redb — and this module
//! is just the namespace it lives under. The previous SurrealDB layer was
//! removed when the redb graph store reached parity (BSL → Apache 2.0).

pub mod redb;
